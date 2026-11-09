//! Stream output engine: Clean Feed and virtual camera (PRD section 5).
//!
//! Clean Feed is a separate borderless window showing only the mirrored
//! Android screen (same shared frame as the main preview). It exists so OBS
//! / TikTok LIVE Studio can capture it with Window Capture without any
//! PlayLink UI in frame. The virtual camera (Windows 11 Media Foundation
//! API) is Phase 2; this module exposes its state and the Phase 1
//! "not available, use Clean Feed" guidance.
//!
//! Threading: Slint windows are not `Send` or `Sync`, so `CleanFeed`
//! never holds a window. It holds a `slint::Weak` handle (which is
//! `Send` + `Sync`) to the window created on the event loop thread.
//! `open` must be called from a worker thread; frame pumping runs from
//! the main window's state pump via `pump_frame` on the event loop thread.

use crate::core::{OutputAspect, PlayLinkError, Result, SharedState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

slint::include_modules!();

/// Clean Feed window state. Cheap to clone handles; safe to share across
/// worker threads. The window itself lives on the event loop thread.
pub struct CleanFeed {
    state: Arc<SharedState>,
    aspect: Mutex<OutputAspect>,
    open: Arc<AtomicBool>,
    weak: Arc<Mutex<Option<slint::Weak<CleanFeedWindow>>>>,
}

impl CleanFeed {
    pub fn new(state: Arc<SharedState>, aspect: OutputAspect) -> Self {
        Self {
            state,
            aspect: Mutex::new(aspect),
            open: Arc::new(AtomicBool::new(false)),
            weak: Arc::new(Mutex::new(None)),
        }
    }

    /// Open the borderless Clean Feed window (separate Slint window).
    ///
    /// Must be called from a worker thread, never the event loop thread:
    /// the window is created on the event loop via `invoke_from_event_loop`
    /// and this call blocks until that completes (up to 5 s). Calling it
    /// twice is a no-op.
    pub fn open(&self) -> Result<()> {
        if self.open.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let aspect = self
            .aspect
            .lock()
            .map(|a| *a)
            .unwrap_or(OutputAspect::Portrait);
        let weak_slot = self.weak.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Result<()>>();
        if let Err(e) = slint::invoke_from_event_loop(move || {
            let result = (|| -> Result<()> {
                let window = CleanFeedWindow::new().map_err(|e| PlayLinkError::Other(e.into()))?;
                apply_window_size(&window, aspect);
                window.show().map_err(|e| PlayLinkError::Other(e.into()))?;
                if let Ok(mut slot) = weak_slot.lock() {
                    *slot = Some(window.as_weak());
                }
                Ok(())
            })();
            let _ = tx.send(result);
        }) {
            self.open.store(false, Ordering::SeqCst);
            return Err(PlayLinkError::Other(e.into()));
        }
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => {
                if let Ok(mut flag) = self.state.clean_feed_open.lock() {
                    *flag = true;
                }
                Ok(())
            }
            Ok(Err(e)) => {
                self.open.store(false, Ordering::SeqCst);
                Err(e)
            }
            Err(_) => {
                self.open.store(false, Ordering::SeqCst);
                Err(PlayLinkError::Other(anyhow::anyhow!(
                    "Timed out waiting for the Clean Feed window to open"
                )))
            }
        }
    }

    /// Hide the Clean Feed window. Safe to call from any thread.
    pub fn close(&self) {
        if !self.open.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Ok(mut flag) = self.state.clean_feed_open.lock() {
            *flag = false;
        }
        let weak_slot = self.weak.clone();
        let state = self.state.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Ok(mut slot) = weak_slot.lock() {
                if let Some(weak) = slot.take() {
                    if let Some(window) = weak.upgrade() {
                        let _ = window.hide();
                    }
                }
            }
            if let Ok(mut flag) = state.clean_feed_open.lock() {
                *flag = false;
            }
        });
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    pub fn set_aspect(&self, aspect: OutputAspect) {
        if let Ok(mut slot) = self.aspect.lock() {
            *slot = aspect;
        }
        if let Ok(mut settings) = self.state.settings.lock() {
            settings.clean_feed_aspect = aspect;
        }
        if self.open.load(Ordering::SeqCst) {
            let weak_slot = self.weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Ok(slot) = weak_slot.lock() {
                    if let Some(weak) = slot.as_ref() {
                        if let Some(window) = weak.upgrade() {
                            apply_window_size(&window, aspect);
                        }
                    }
                }
            });
        }
    }

    /// Target canvas for the selected aspect (PRD section 5.1).
    pub fn canvas(&self) -> (u32, u32) {
        self.aspect
            .lock()
            .map(|a| a.canvas())
            .unwrap_or((1080, 1920))
    }

    /// Push the latest shared frame into the Clean Feed window.
    /// Call only on the event loop thread (the app state pump does this).
    pub fn pump_frame(&self) {
        if !self.open.load(Ordering::SeqCst) {
            return;
        }
        let Some(window) = self
            .weak
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .and_then(|weak| weak.upgrade())
        else {
            return;
        };
        if let Ok(frame) = self.state.frame.lock() {
            if let Some(frame) = frame.as_ref() {
                let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    &frame.rgba,
                    frame.width,
                    frame.height,
                );
                window.set_video_frame(slint::Image::from_rgba8(buffer));
            }
        }
    }
}

/// Size the window from the aspect canvas, scaled so the long side is
/// 720 logical pixels (the canvas itself is the OBS capture target;
/// the on-screen window is a preview of it at the same aspect).
fn apply_window_size(window: &CleanFeedWindow, aspect: OutputAspect) {
    let (w, h) = aspect.canvas();
    let scale = 720.0 / w.max(h) as f32;
    let size = slint::LogicalSize::new((w as f32 * scale).round(), (h as f32 * scale).round());
    window.window().set_size(slint::WindowSize::Logical(size));
}

/// Virtual camera state (Phase 2, Windows 11 Media Foundation API).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualCameraState {
    NotSupported,
    Disabled,
    Enabled,
    Error,
}

/// Returns the virtual camera state on this machine. Phase 1 always returns
/// `NotSupported` with the Clean Feed guidance; the MF implementation lands
/// in Phase 2.
pub fn virtual_camera_state() -> VirtualCameraState {
    // Phase 1: no virtual camera is registered on this machine, so there
    // is nothing for OBS to pick up as a Video Capture Device. Phase 2
    // must implement an IMFMediaSource that serves the shared frame,
    // register it with MFCreateVirtualCamera (Windows 11, `windows` crate
    // Win32_Media_MediaFoundation), and manage activation through
    // IMFVirtualCamera. Until that lands, Clean Feed is the supported
    // capture path: open it and use Window Capture in OBS.
    VirtualCameraState::NotSupported
}
