use crate::core::{ConnectionState, Frame, MirrorStats, RecordingState, Settings};
use crate::device::DeviceInfo;
use std::sync::{Arc, Mutex};

/// State shared between the engine worker threads and the Slint UI thread.
///
/// The UI polls this on a timer (60 Hz for frames, 2 Hz for stats). Workers
/// update it behind a mutex. Nothing here blocks the UI thread for long.
#[derive(Debug, Default)]
pub struct SharedState {
    pub settings: Mutex<Settings>,
    pub devices: Mutex<Vec<DeviceInfo>>,
    pub selected_serial: Mutex<Option<String>>,
    pub connection: Mutex<ConnectionState>,
    /// Latest decoded frame. `None` until the first frame arrives.
    pub frame: Mutex<Option<Frame>>,
    pub stats: Mutex<MirrorStats>,
    pub mirroring: Mutex<bool>,
    pub recording: Mutex<RecordingState>,
    pub recording_elapsed_secs: Mutex<u64>,
    pub clean_feed_open: Mutex<bool>,
    pub virtual_camera_on: Mutex<bool>,
    pub audio_forwarding_on: Mutex<bool>,
    /// Last one-shot message for the UI (toast / status line).
    pub status_message: Mutex<Option<String>>,
}

impl SharedState {
    pub fn new(settings: Settings) -> Arc<Self> {
        Arc::new(Self {
            settings: Mutex::new(settings),
            devices: Mutex::new(Vec::new()),
            selected_serial: Mutex::new(None),
            connection: Mutex::new(ConnectionState::Disconnected),
            frame: Mutex::new(None),
            stats: Mutex::new(MirrorStats::default()),
            mirroring: Mutex::new(false),
            recording: Mutex::new(RecordingState::Idle),
            recording_elapsed_secs: Mutex::new(0),
            clean_feed_open: Mutex::new(false),
            virtual_camera_on: Mutex::new(false),
            audio_forwarding_on: Mutex::new(false),
            status_message: Mutex::new(None),
        })
    }

    pub fn set_status(&self, msg: impl Into<String>) {
        if let Ok(mut slot) = self.status_message.lock() {
            *slot = Some(msg.into());
        }
    }

    pub fn take_status(&self) -> Option<String> {
        self.status_message.lock().ok()?.take()
    }

    pub fn set_frame(&self, frame: Frame) {
        if let Ok(mut slot) = self.frame.lock() {
            *slot = Some(frame);
        }
    }

    pub fn is_mirroring(&self) -> bool {
        self.mirroring.lock().map(|g| *g).unwrap_or(false)
    }
}
