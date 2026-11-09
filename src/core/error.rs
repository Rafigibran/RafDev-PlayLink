use thiserror::Error;

/// Every error the application can surface to the user.
///
/// UI code must map each variant to the actionable guidance in PRD section 14
/// (what happened + what to do next). Technical detail goes to diagnostics.
#[derive(Debug, Error)]
pub enum PlayLinkError {
    #[error("ADB executable not found. Install Android platform-tools or set the ADB path in Settings > Advanced.")]
    AdbNotFound,

    #[error("ADB command failed: {0}")]
    AdbFailed(String),

    #[error(
        "Android device not found. Check the USB cable, USB mode, and that USB Debugging is on."
    )]
    DeviceNotFound,

    #[error("Device is unauthorized. Approve the USB debugging prompt on the phone.")]
    DeviceUnauthorized,

    #[error("Device is offline. Reconnect the cable or restart the wireless connection.")]
    DeviceOffline,

    #[error("Could not download the mirroring server: {0}")]
    ServerDownloadFailed(String),

    #[error("Could not start the mirroring server on the device: {0}")]
    ServerStartFailed(String),

    #[error("Video stream ended unexpectedly: {0}")]
    StreamEnded(String),

    #[error("Video decoding failed: {0}")]
    DecodeFailed(String),

    #[error("Recording failed: {0}")]
    RecordingFailed(String),

    #[error("Screenshot failed: {0}")]
    ScreenshotFailed(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Unsupported on this setup: {0}")]
    Unsupported(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, PlayLinkError>;
