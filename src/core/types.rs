use serde::{Deserialize, Serialize};

/// How the phone is connected to the PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionType {
    Usb,
    Wifi,
}

/// Live state of a device connection (PRD section 4.1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Unauthorized,
    Offline,
    Error(String),
}

impl ConnectionState {
    pub fn label(&self) -> &'static str {
        match self {
            ConnectionState::Disconnected => "Disconnected",
            ConnectionState::Connecting => "Connecting...",
            ConnectionState::Connected => "Connected",
            ConnectionState::Unauthorized => "Unauthorized",
            ConnectionState::Offline => "Offline",
            ConnectionState::Error(_) => "Error",
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionState::Connected)
    }
}

/// One Android device, as discovered through ADB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub serial: String,
    pub model: String,
    pub manufacturer: String,
    pub android_version: String,
    pub sdk: u32,
    pub connection: ConnectionType,
    pub state: ConnectionState,
    /// RFC 3339 timestamp of the last successful connection, if any.
    pub last_connected: Option<String>,
    /// Preferred device (auto-connect target).
    pub preferred: bool,
}

impl DeviceInfo {
    pub fn display_name(&self) -> String {
        if self.model.is_empty() {
            self.serial.clone()
        } else {
            format!("{} {}", self.manufacturer, self.model)
        }
    }
}

/// Output quality preset (PRD section 4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoPreset {
    Low,
    Balanced,
    High,
    Custom,
}

impl VideoPreset {
    /// (max_size, max_fps, bitrate_bps) handed to the mirroring server.
    pub fn params(&self) -> (u32, u32, u32) {
        match self {
            VideoPreset::Low => (1280, 30, 4_000_000),
            VideoPreset::Balanced => (1920, 30, 8_000_000),
            VideoPreset::High => (1920, 60, 12_000_000),
            VideoPreset::Custom => (1920, 30, 8_000_000),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            VideoPreset::Low => "Low (720p30)",
            VideoPreset::Balanced => "Balanced (1080p30)",
            VideoPreset::High => "High (1080p60)",
            VideoPreset::Custom => "Custom",
        }
    }
}

/// One decoded video frame, shared between the mirror engine and the UI.
///
/// The UI polls the latest frame; the engine overwrites it. `seq` lets the
/// UI skip re-rendering an unchanged frame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// RGBA8 pixels, row-major, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    /// Presentation timestamp in milliseconds.
    pub timestamp_ms: u64,
    pub seq: u64,
}

impl Frame {
    pub fn blank(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            rgba: vec![0; (width * height * 4) as usize],
            timestamp_ms: 0,
            seq: 0,
        }
    }
}

/// Live mirroring statistics shown in the UI (PRD section 4.2).
#[derive(Debug, Clone, Default)]
pub struct MirrorStats {
    pub fps: f32,
    pub latency_ms: f32,
    pub bitrate_bps: u64,
    pub frames_total: u64,
    pub frames_dropped: u64,
    pub width: u32,
    pub height: u32,
}

/// Output aspect-ratio preset for Clean Feed (PRD section 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputAspect {
    Portrait,
    Landscape,
    Square,
    Custom,
}

impl OutputAspect {
    /// Target canvas (width, height).
    pub fn canvas(&self) -> (u32, u32) {
        match self {
            OutputAspect::Portrait => (1080, 1920),
            OutputAspect::Landscape => (1920, 1080),
            OutputAspect::Square => (1080, 1080),
            OutputAspect::Custom => (1080, 1920),
        }
    }
}

/// Recording state (PRD section 7.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RecordingState {
    #[default]
    Idle,
    Recording,
    Paused,
}

/// Events the engine sends to the UI thread. The UI polls shared state on a
/// timer; this channel is for one-shot notifications (toasts, errors).
#[derive(Debug, Clone)]
pub enum AppEvent {
    DeviceListChanged,
    ConnectionChanged(ConnectionState),
    MirrorStarted,
    MirrorStopped(String),
    Error(String),
    ScreenshotSaved(String),
    RecordingStateChanged(RecordingState),
}
