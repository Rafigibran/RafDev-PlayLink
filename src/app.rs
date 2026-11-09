//! Application wiring: builds the Slint window, connects UI callbacks to
//! the engines, and pumps shared state into the UI on a timer.
//!
//! The UI never talks to devices directly. All work happens on worker
//! threads; results land in [`SharedState`] and are pushed to the UI with
//! `slint::invoke_from_event_loop`.

use crate::core::{
    ConnectionState, RecordingState, Result, Settings, SharedState, VideoPreset,
};
use crate::device::Adb;
use crate::input::{InputController, KEYCODE_VOLUME_DOWN, KEYCODE_VOLUME_UP};
use crate::mirror::{MirrorConfig, MirrorEngine};
use crate::recording::RecordingEngine;
use crate::streaming::CleanFeed;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

slint::include_modules!();

/// Engine handles shared across callbacks. Every engine lives behind a
/// mutex so worker threads can take it without touching the UI thread.
/// `MirrorEngine` and `InputController` are stored side by side: the
/// controller is taken from the engine when mirroring starts and dropped
/// when the session ends.
struct Engines {
    mirror: Mutex<Option<MirrorEngine>>,
    input: Mutex<Option<InputController>>,
    recording: Mutex<Option<RecordingEngine>>,
    recorder_thread: Mutex<Option<JoinHandle<()>>>,
    clean_feed: Mutex<Option<CleanFeed>>,
}

impl Engines {
    fn new() -> Self {
        Self {
            mirror: Mutex::new(None),
            input: Mutex::new(None),
            recording: Mutex::new(None),
            recorder_thread: Mutex::new(None),
            clean_feed: Mutex::new(None),
        }
    }
}

/// Build the window, wire callbacks, run the event loop.
pub fn run(settings: Settings) -> Result<()> {
    let state = SharedState::new(settings);
    let engines = Arc::new(Engines::new());
    let window = AppWindow::new().map_err(|e| crate::core::PlayLinkError::Other(e.into()))?;
    window.set_version(env!("CARGO_PKG_VERSION").into());
    wire_callbacks(&window, state.clone(), engines.clone());
    spawn_state_pump(window.as_weak(), state.clone(), engines);
    // Initial device list refresh (worker thread, never blocks startup).
    let refresh_state = state.clone();
    std::thread::spawn(move || refresh_devices(&refresh_state));
    window
        .run()
        .map_err(|e| crate::core::PlayLinkError::Other(e.into()))?;
    Ok(())
}

fn wire_callbacks(window: &AppWindow, state: Arc<SharedState>, engines: Arc<Engines>) {
    let s = state.clone();
    window.on_refresh_devices(move || {
        let s = s.clone();
        std::thread::spawn(move || refresh_devices(&s));
    });

    let s = state.clone();
    window.on_connect_device(move |serial| {
        connect_device(&s, &serial);
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_disconnect_device(move || {
        disconnect_device(&s, &e);
    });

    let s = state.clone();
    window.on_forget_device(move |serial| {
        forget_device(&s, &serial);
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_start_mirroring(move || {
        start_mirroring(&s, &e);
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_stop_mirroring(move || {
        stop_mirroring(&s, &e);
    });

    let s = state.clone();
    window.on_take_screenshot(move || {
        take_screenshot(&s);
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_toggle_recording(move || {
        toggle_recording(&s, &e);
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_open_clean_feed(move || {
        open_clean_feed(&s, &e);
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_nav_back(move || {
        input_action(s.clone(), e.clone(), "Back", |input| input.back());
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_nav_home(move || {
        input_action(s.clone(), e.clone(), "Home", |input| input.home());
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_nav_recents(move || {
        input_action(s.clone(), e.clone(), "Recent apps", |input| {
            input.recent_apps()
        });
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_volume_up(move || {
        input_action(s.clone(), e.clone(), "Volume up", |input| {
            input.key(KEYCODE_VOLUME_UP)
        });
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_volume_down(move || {
        input_action(s.clone(), e.clone(), "Volume down", |input| {
            input.key(KEYCODE_VOLUME_DOWN)
        });
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_preview_click(move |x, y| {
        preview_click(&s, &e, x, y);
    });

    let s = state.clone();
    let e = engines.clone();
    window.on_preview_drag(move |x1, y1, x2, y2| {
        preview_drag(&s, &e, x1, y1, x2, y2);
    });

    let s = state.clone();
    window.on_set_quality(move |index| {
        set_quality(&s, index);
    });

    let s = state.clone();
    window.on_set_audio_forwarding(move |enabled| {
        set_audio_forwarding(&s, enabled);
    });

    let s = state.clone();
    window.on_set_hw_decoding(move |enabled| {
        set_hw_decoding(&s, enabled);
    });

    let s = state.clone();
    window.on_set_auto_reconnect(move |enabled| {
        set_auto_reconnect(&s, enabled);
    });

    let s = state.clone();
    window.on_set_adb_path(move |path| {
        set_adb_path(&s, &path);
    });

    let s = state.clone();
    window.on_export_diagnostics(move || {
        export_diagnostics(&s);
    });
}

/// Snapshot of the current settings (cheap clone, no I/O).
fn settings_snapshot(state: &Arc<SharedState>) -> Settings {
    state.settings.lock().map(|s| s.clone()).unwrap_or_default()
}

/// Persist settings to disk. Best effort: failures are logged, never fatal.
fn persist_settings(state: &Arc<SharedState>) {
    let settings = settings_snapshot(state);
    if let Err(e) = settings.save() {
        tracing::warn!("Could not save settings: {}", e);
    }
}

/// Apply one change to settings, then persist (best effort).
fn update_setting(state: &Arc<SharedState>, update: impl FnOnce(&mut Settings)) {
    if let Ok(mut settings) = state.settings.lock() {
        update(&mut settings);
    }
    persist_settings(state);
}

fn set_connection(state: &Arc<SharedState>, next: ConnectionState) {
    if let Ok(mut c) = state.connection.lock() {
        *c = next;
    }
}

/// Refresh the device list via ADB and push it into shared state.
/// Runs on a worker thread; callers spawn it.
fn refresh_devices(state: &Arc<SharedState>) {
    let adb_path = settings_snapshot(state).adb_path;
    match Adb::locate(&adb_path).and_then(|adb| {
        let mut manager = crate::device::DeviceManager::new(adb);
        manager.refresh()
    }) {
        Ok(devices) => {
            if let Ok(mut slot) = state.devices.lock() {
                *slot = devices;
            }
            state.set_status("Device list refreshed.");
        }
        Err(e) => state.set_status(format!("Could not list devices: {}", e)),
    }
}

fn connect_device(state: &Arc<SharedState>, serial: &str) {
    let serial = serial.to_string();
    if let Ok(mut slot) = state.selected_serial.lock() {
        *slot = Some(serial.clone());
    }
    set_connection(state, ConnectionState::Connecting);
    state.set_status(format!("Connecting to {}…", serial));
    let state = state.clone();
    std::thread::spawn(move || {
        // Confirm the device is really there and read its live state.
        let adb_path = settings_snapshot(&state).adb_path;
        let refreshed = Adb::locate(&adb_path).and_then(|adb| {
            let mut manager = crate::device::DeviceManager::new(adb);
            manager.refresh()
        });
        // The user may have disconnected while the refresh was running.
        let still_selected = state
            .selected_serial
            .lock()
            .ok()
            .and_then(|s| s.clone())
            .as_deref()
            == Some(serial.as_str());
        if !still_selected {
            return;
        }
        match refreshed {
            Ok(devices) => {
                if let Ok(mut slot) = state.devices.lock() {
                    *slot = devices.clone();
                }
                match devices.iter().find(|d| d.serial == serial) {
                    Some(device) => {
                        set_connection(&state, device.state.clone());
                        match device.state {
                            ConnectionState::Connected => {
                                state
                                    .set_status(format!("Connected to {}.", device.display_name()));
                            }
                            ConnectionState::Unauthorized => {
                                state.set_status(
                                    "Device is unauthorized. Approve the USB debugging prompt on the phone.",
                                );
                            }
                            ConnectionState::Offline => {
                                state.set_status(
                                    "Device is offline. Reconnect the cable or restart the wireless connection.",
                                );
                            }
                            _ => {
                                state
                                    .set_status(format!("Device state: {}.", device.state.label()));
                            }
                        }
                    }
                    None => {
                        set_connection(
                            &state,
                            ConnectionState::Error("Device not found".to_string()),
                        );
                        state
                            .set_status("Device not found. Refresh the device list and try again.");
                    }
                }
            }
            Err(e) => {
                set_connection(&state, ConnectionState::Error(e.to_string()));
                state.set_status(format!("Could not list devices: {}", e));
            }
        }
    });
}

fn disconnect_device(state: &Arc<SharedState>, engines: &Arc<Engines>) {
    let state = state.clone();
    let engines = engines.clone();
    std::thread::spawn(move || {
        let recording_msg = stop_recording(&state, &engines);
        stop_engine(&engines);
        if let Ok(mut slot) = engines.clean_feed.lock() {
            if let Some(feed) = slot.take() {
                feed.close();
            }
        }
        if let Ok(mut m) = state.mirroring.lock() {
            *m = false;
        }
        if let Ok(mut slot) = state.selected_serial.lock() {
            *slot = None;
        }
        set_connection(&state, ConnectionState::Disconnected);
        match recording_msg {
            Some(msg) => state.set_status(msg),
            None => state.set_status("Disconnected."),
        }
    });
}

fn forget_device(state: &Arc<SharedState>, serial: &str) {
    let serial = serial.to_string();
    let state = state.clone();
    std::thread::spawn(move || {
        let adb_path = settings_snapshot(&state).adb_path;
        match Adb::locate(&adb_path).and_then(|adb| {
            let mut manager = crate::device::DeviceManager::new(adb);
            manager.forget(&serial)
        }) {
            Ok(()) => {
                state.set_status(format!("Forgot device {}.", serial));
                refresh_devices(&state);
            }
            Err(e) => state.set_status(format!("Could not forget device: {}", e)),
        }
    });
}

fn start_mirroring(state: &Arc<SharedState>, engines: &Arc<Engines>) {
    let Some(serial) = state.selected_serial.lock().ok().and_then(|s| s.clone()) else {
        state.set_status("Select a device first.");
        return;
    };
    let device = state
        .devices
        .lock()
        .ok()
        .and_then(|devices| devices.iter().find(|d| d.serial == serial).cloned());
    let Some(device) = device else {
        state.set_status("Device not found. Refresh the device list and try again.");
        return;
    };
    if state.is_mirroring() {
        state.set_status("Mirroring is already running.");
        return;
    }
    let settings = settings_snapshot(state);
    set_connection(state, ConnectionState::Connecting);
    state.set_status(format!("Starting mirroring on {}…", device.display_name()));
    let state = state.clone();
    let engines = engines.clone();
    std::thread::spawn(move || {
        let adb = match Adb::locate(&settings.adb_path) {
            Ok(adb) => adb,
            Err(e) => {
                set_connection(&state, ConnectionState::Error(e.to_string()));
                state.set_status(format!("Could not start mirroring: {}", e));
                return;
            }
        };
        let mut engine = MirrorEngine::new(adb, state.clone());
        let cfg = MirrorConfig::from_settings(&settings);
        match engine.start(&device, cfg) {
            Ok(()) => {
                // The user may have disconnected while the engine started.
                let cancelled = state
                    .connection
                    .lock()
                    .map(|c| *c == ConnectionState::Disconnected)
                    .unwrap_or(false);
                if cancelled {
                    engine.stop();
                    return;
                }
                if let Some(controller) = engine.input_controller() {
                    if let Ok(mut slot) = engines.input.lock() {
                        *slot = Some(controller);
                    }
                }
                if let Ok(mut slot) = engines.mirror.lock() {
                    *slot = Some(engine);
                }
                if let Ok(mut m) = state.mirroring.lock() {
                    *m = true;
                }
                set_connection(&state, ConnectionState::Connected);
                state.set_status(format!("Mirroring {}.", device.display_name()));
            }
            Err(e) => {
                set_connection(&state, ConnectionState::Error(e.to_string()));
                state.set_status(format!("Could not start mirroring: {}", e));
            }
        }
    });
}

fn stop_mirroring(state: &Arc<SharedState>, engines: &Arc<Engines>) {
    let state = state.clone();
    let engines = engines.clone();
    std::thread::spawn(move || {
        let recording_msg = stop_recording(&state, &engines);
        stop_engine(&engines);
        if let Ok(mut m) = state.mirroring.lock() {
            *m = false;
        }
        match recording_msg {
            Some(msg) => state.set_status(msg),
            None => state.set_status("Mirroring stopped."),
        }
    });
}

/// Stop the mirror engine and drop its input controller. No status message.
fn stop_engine(engines: &Arc<Engines>) {
    if let Ok(mut slot) = engines.mirror.lock() {
        if let Some(mut engine) = slot.take() {
            engine.stop();
        }
    }
    if let Ok(mut slot) = engines.input.lock() {
        *slot = None;
    }
}

fn take_screenshot(state: &Arc<SharedState>) {
    let settings = settings_snapshot(state);
    let state = state.clone();
    std::thread::spawn(move || {
        match crate::recording::save_screenshot(&state, &settings.screenshot_folder) {
            Ok(path) => state.set_status(format!("Screenshot saved: {}", path.display())),
            Err(e) => state.set_status(format!("Screenshot failed: {}", e)),
        }
    });
}

fn toggle_recording(state: &Arc<SharedState>, engines: &Arc<Engines>) {
    let recording = state
        .recording
        .lock()
        .map(|r| *r)
        .unwrap_or(RecordingState::Idle);
    if recording != RecordingState::Idle {
        let state = state.clone();
        let engines = engines.clone();
        std::thread::spawn(move || {
            if let Some(msg) = stop_recording(&state, &engines) {
                state.set_status(msg);
            }
        });
        return;
    }
    if !state.is_mirroring() {
        state.set_status("Start mirroring before recording.");
        return;
    }
    let settings = settings_snapshot(state);
    let state = state.clone();
    let engines = engines.clone();
    std::thread::spawn(move || {
        let mut recorder = RecordingEngine::new(state.clone());
        match recorder.start(&settings.recording_folder) {
            Ok(path) => {
                if let Ok(mut r) = state.recording.lock() {
                    *r = RecordingState::Recording;
                }
                if let Ok(mut e) = state.recording_elapsed_secs.lock() {
                    *e = 0;
                }
                let started = Instant::now();
                if let Ok(mut slot) = engines.recording.lock() {
                    *slot = Some(recorder);
                }
                spawn_recorder_feeder(state.clone(), engines.clone(), started);
                state.set_status(format!("Recording started: {}", path.display()));
            }
            Err(e) => {
                state.set_status(format!("Could not start recording: {}", e));
            }
        }
    });
}

/// Poll the mirror engine for recorded chunks and feed them to the
/// recorder until recording stops. Runs on its own thread.
fn spawn_recorder_feeder(state: Arc<SharedState>, engines: Arc<Engines>, started: Instant) {
    let feeder_engines = engines.clone();
    let handle = std::thread::spawn(move || loop {
        let recording = state
            .recording
            .lock()
            .map(|r| *r)
            .unwrap_or(RecordingState::Idle);
        if recording != RecordingState::Recording {
            break;
        }
        let chunk = {
            let mut slot = match feeder_engines.mirror.lock() {
                Ok(slot) => slot,
                Err(_) => break,
            };
            match slot.as_mut() {
                Some(engine) => engine.take_recorded_chunk(),
                None => None,
            }
        };
        match chunk {
            Some(data) => {
                let pts_us = started.elapsed().as_micros() as u64;
                if let Ok(mut slot) = feeder_engines.recording.lock() {
                    if let Some(recorder) = slot.as_mut() {
                        recorder.feed_video(&data, pts_us);
                    }
                }
                if let Ok(mut e) = state.recording_elapsed_secs.lock() {
                    *e = started.elapsed().as_secs();
                }
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    });
    if let Ok(mut slot) = engines.recorder_thread.lock() {
        *slot = Some(handle);
    }
}

/// Stop the active recording, if any, and finalize the file.
/// Returns the status message to show, or `None` when nothing was
/// recording. Blocks briefly while the feeder thread exits.
fn stop_recording(state: &Arc<SharedState>, engines: &Arc<Engines>) -> Option<String> {
    if let Ok(mut r) = state.recording.lock() {
        *r = RecordingState::Idle;
    }
    if let Ok(mut slot) = engines.recorder_thread.lock() {
        if let Some(handle) = slot.take() {
            let _ = handle.join();
        }
    }
    let recorder = {
        let mut slot = match engines.recording.lock() {
            Ok(slot) => slot,
            Err(_) => return None,
        };
        slot.take()
    };
    let Some(mut recorder) = recorder else {
        return None;
    };
    if let Ok(mut e) = state.recording_elapsed_secs.lock() {
        *e = 0;
    }
    match recorder.stop() {
        Ok(path) => Some(format!("Recording saved: {}", path.display())),
        Err(e) => Some(format!("Could not finish recording: {}", e)),
    }
}

fn open_clean_feed(state: &Arc<SharedState>, engines: &Arc<Engines>) {
    if let Ok(slot) = engines.clean_feed.lock() {
        if let Some(feed) = slot.as_ref() {
            if feed.is_open() {
                state.set_status("Clean Feed is already open.");
                return;
            }
        }
    }
    let settings = settings_snapshot(state);
    let state = state.clone();
    let engines = engines.clone();
    std::thread::spawn(move || {
        let feed = CleanFeed::new(state.clone(), settings.clean_feed_aspect);
        match feed.open() {
            Ok(()) => {
                if let Ok(mut slot) = engines.clean_feed.lock() {
                    *slot = Some(feed);
                }
                if let Ok(mut open) = state.clean_feed_open.lock() {
                    *open = true;
                }
                state.set_status("Clean Feed opened.");
            }
            Err(e) => {
                state.set_status(format!("Could not open Clean Feed: {}", e));
            }
        }
    });
}

/// Run one input action on a worker thread. When no controller is
/// available (not mirroring) the user is told why; failures are logged
/// and reported in the status line. Never panics the UI.
fn input_action<F>(state: Arc<SharedState>, engines: Arc<Engines>, label: &'static str, action: F)
where
    F: FnOnce(&mut InputController) -> Result<()> + Send + 'static,
{
    std::thread::spawn(move || {
        let mut slot = match engines.input.lock() {
            Ok(slot) => slot,
            Err(_) => return,
        };
        let Some(input) = slot.as_mut() else {
            drop(slot);
            state.set_status("Start mirroring to control the phone.");
            return;
        };
        if let Err(e) = action(input) {
            tracing::warn!("{} failed: {}", label, e);
            state.set_status(format!("{} failed: {}", label, e));
        }
    });
}

/// Convert normalized preview coordinates (0..1) to device pixels using
/// the current video size. Returns `None` when no frame size is known yet.
fn normalized_to_pixels(state: &Arc<SharedState>, nx: f32, ny: f32) -> Option<(u32, u32)> {
    let stats = state.stats.lock().ok()?;
    if stats.width == 0 || stats.height == 0 {
        return None;
    }
    let nx = nx.clamp(0.0, 1.0);
    let ny = ny.clamp(0.0, 1.0);
    Some((
        (nx * stats.width as f32) as u32,
        (ny * stats.height as f32) as u32,
    ))
}

fn preview_click(state: &Arc<SharedState>, engines: &Arc<Engines>, nx: f32, ny: f32) {
    let Some((x, y)) = normalized_to_pixels(state, nx, ny) else {
        state.set_status("Start mirroring to control the phone.");
        return;
    };
    input_action(state.clone(), engines.clone(), "Tap", move |input| {
        input.tap(x, y)
    });
}

fn preview_drag(
    state: &Arc<SharedState>,
    engines: &Arc<Engines>,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
) {
    let Some((px1, py1)) = normalized_to_pixels(state, x1, y1) else {
        state.set_status("Start mirroring to control the phone.");
        return;
    };
    let Some((px2, py2)) = normalized_to_pixels(state, x2, y2) else {
        state.set_status("Start mirroring to control the phone.");
        return;
    };
    input_action(state.clone(), engines.clone(), "Swipe", move |input| {
        input.swipe(px1, py1, px2, py2)
    });
}

fn set_quality(state: &Arc<SharedState>, index: i32) {
    let preset = match index {
        0 => VideoPreset::Low,
        1 => VideoPreset::Balanced,
        2 => VideoPreset::High,
        _ => VideoPreset::Custom,
    };
    update_setting(state, |s| s.video_preset = preset);
    state.set_status(format!("Quality set to {}.", preset.label()));
}

fn set_audio_forwarding(state: &Arc<SharedState>, enabled: bool) {
    update_setting(state, |s| s.audio_forwarding = enabled);
    if let Ok(mut on) = state.audio_forwarding_on.lock() {
        *on = enabled;
    }
}

fn set_hw_decoding(state: &Arc<SharedState>, enabled: bool) {
    update_setting(state, |s| s.hardware_decoding = enabled);
}

fn set_auto_reconnect(state: &Arc<SharedState>, enabled: bool) {
    update_setting(state, |s| s.auto_reconnect = enabled);
}

fn set_adb_path(state: &Arc<SharedState>, path: &str) {
    update_setting(state, |s| s.adb_path = path.to_string());
}

fn export_diagnostics(state: &Arc<SharedState>) {
    let state = state.clone();
    std::thread::spawn(move || match crate::diagnostics::export_diagnostics() {
        Ok(path) => state.set_status(format!("Diagnostics saved: {}", path.display())),
        Err(e) => state.set_status(format!("Could not export diagnostics: {}", e)),
    });
}

/// Poll shared state and push it into the Slint window (frames at ~30 Hz).
fn spawn_state_pump(
    window: slint::Weak<AppWindow>,
    state: Arc<SharedState>,
    engines: Arc<Engines>,
) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(33));
        let weak = window.clone();
        let state = state.clone();
        let engines = engines.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else { return };
            if let Ok(frame) = state.frame.lock() {
                if let Some(frame) = frame.as_ref() {
                    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                        &frame.rgba,
                        frame.width,
                        frame.height,
                    );
                    window.set_video_frame(slint::Image::from_rgba8(buffer));
                }
            }
            // Same frame into the Clean Feed window (OBS capture target).
            if let Ok(slot) = engines.clean_feed.lock() {
                if let Some(feed) = slot.as_ref() {
                    feed.pump_frame();
                }
            }
            if let Ok(stats) = state.stats.lock() {
                window.set_fps(format!("{:.0}", stats.fps).into());
                window.set_latency(format!("{:.0} ms", stats.latency_ms).into());
            }
            if let Ok(c) = state.connection.lock() {
                window.set_connection_status(c.label().into());
            }
            window.set_is_mirroring(state.is_mirroring());
            if let Ok(serial) = state.selected_serial.lock() {
                window.set_connected_device(serial.as_deref().unwrap_or("").into());
            }
            if let Ok(recording) = state.recording.lock() {
                window.set_is_recording(*recording == RecordingState::Recording);
            }
            if let Ok(elapsed) = state.recording_elapsed_secs.lock() {
                let elapsed = *elapsed;
                window
                    .set_recording_time(format!("{:02}:{:02}", elapsed / 60, elapsed % 60).into());
            }
            if let Ok(open) = state.clean_feed_open.lock() {
                window.set_clean_feed_open(*open);
            }
            if let Ok(settings) = state.settings.lock() {
                window.set_audio_status(if settings.audio_forwarding {
                    "On".into()
                } else {
                    "Off".into()
                });
                window.set_screenshot_folder(settings.screenshot_folder.clone().into());
                window.set_recording_folder(settings.recording_folder.clone().into());
                window.set_adb_path(settings.adb_path.clone().into());
                window.set_quality_index(match settings.video_preset {
                    VideoPreset::Low => 0,
                    VideoPreset::Balanced => 1,
                    VideoPreset::High => 2,
                    VideoPreset::Custom => 3,
                });
                window.set_hw_decoding_enabled(settings.hardware_decoding);
                window.set_audio_forwarding_enabled(settings.audio_forwarding);
                window.set_auto_reconnect_enabled(settings.auto_reconnect);
            }
            if let Some(msg) = state.take_status() {
                window.set_status_message(msg.into());
            }
            if let Ok(devices) = state.devices.lock() {
                let rows: Vec<DeviceRow> = devices
                    .iter()
                    .map(|d| DeviceRow {
                        serial: d.serial.clone().into(),
                        name: d.display_name().into(),
                        android: format!("Android {}", d.android_version).into(),
                        connection: match d.connection {
                            crate::core::ConnectionType::Usb => "USB".into(),
                            crate::core::ConnectionType::Wifi => "Wi-Fi".into(),
                        },
                        state: d.state.label().into(),
                    })
                    .collect();
                window.set_device_list(slint::ModelRc::new(slint::VecModel::from_slice(&rows)));
            }
        });
    });
}
