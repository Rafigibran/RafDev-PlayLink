use crate::core::{OutputAspect, PlayLinkError, Result, VideoPreset};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persistent user settings (PRD section 9). Stored as TOML in the app
/// config directory. Every field has a default so a missing or partial file
/// still loads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    // General
    pub launch_on_startup: bool,
    pub minimize_to_tray: bool,
    pub start_minimized: bool,
    pub language: String,
    pub theme: String,
    pub notifications: bool,
    pub auto_reconnect: bool,
    // Video
    pub video_preset: VideoPreset,
    pub custom_max_size: u32,
    pub custom_max_fps: u32,
    pub custom_bitrate_bps: u32,
    pub hardware_decoding: bool,
    pub orientation_auto: bool,
    pub scaling_mode: String,
    // Audio
    pub audio_forwarding: bool,
    pub audio_output_device: String,
    pub audio_volume: f32,
    pub audio_delay_ms: i32,
    pub audio_monitoring: bool,
    // Streaming
    pub clean_feed_aspect: OutputAspect,
    pub clean_feed_on_top: bool,
    pub virtual_camera_enabled: bool,
    pub disconnect_background: String,
    // Storage
    pub screenshot_folder: String,
    pub recording_folder: String,
    pub recording_format: String,
    pub max_recording_mb: u64,
    // Advanced
    pub adb_path: String,
    pub network_timeout_secs: u64,
    pub debug_logs: bool,
    // Devices
    pub preferred_device_serial: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            launch_on_startup: false,
            minimize_to_tray: false,
            start_minimized: false,
            language: "en".to_string(),
            theme: "light".to_string(),
            notifications: true,
            auto_reconnect: true,
            video_preset: VideoPreset::Balanced,
            custom_max_size: 1920,
            custom_max_fps: 30,
            custom_bitrate_bps: 8_000_000,
            hardware_decoding: true,
            orientation_auto: true,
            scaling_mode: "fit".to_string(),
            audio_forwarding: false,
            audio_output_device: "default".to_string(),
            audio_volume: 1.0,
            audio_delay_ms: 0,
            audio_monitoring: false,
            clean_feed_aspect: OutputAspect::Portrait,
            clean_feed_on_top: true,
            virtual_camera_enabled: false,
            disconnect_background: "dark".to_string(),
            screenshot_folder: default_pictures_dir(),
            recording_folder: default_videos_dir(),
            recording_format: "mp4".to_string(),
            max_recording_mb: 4096,
            adb_path: String::new(),
            network_timeout_secs: 10,
            debug_logs: false,
            preferred_device_serial: String::new(),
        }
    }
}

fn default_pictures_dir() -> String {
    dirs::picture_dir()
        .map(|p| p.join("PlayLink").to_string_lossy().into_owned())
        .unwrap_or_else(|| "PlayLink".to_string())
}

fn default_videos_dir() -> String {
    dirs::video_dir()
        .map(|p| p.join("PlayLink").to_string_lossy().into_owned())
        .unwrap_or_else(|| "PlayLink".to_string())
}

impl Settings {
    /// Directory that holds config.toml, devices.json, and downloaded
    /// components (scrcpy-server, platform-tools).
    pub fn app_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("RafDev")
            .join("PlayLink")
    }

    pub fn config_path() -> PathBuf {
        Self::app_dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| PlayLinkError::Config(format!("Cannot read {}: {}", path.display(), e)))?;
        toml::from_str(&text)
            .map_err(|e| PlayLinkError::Config(format!("Invalid config {}: {}", path.display(), e)))
    }

    pub fn save(&self) -> Result<()> {
        let dir = Self::app_dir();
        std::fs::create_dir_all(&dir)?;
        let text = toml::to_string_pretty(self)
            .map_err(|e| PlayLinkError::Config(format!("Cannot serialize settings: {}", e)))?;
        std::fs::write(Self::config_path(), text)?;
        Ok(())
    }

    /// Effective mirroring parameters for the selected preset.
    pub fn mirror_params(&self) -> (u32, u32, u32) {
        match self.video_preset {
            VideoPreset::Custom => (
                self.custom_max_size,
                self.custom_max_fps,
                self.custom_bitrate_bps,
            ),
            preset => preset.params(),
        }
    }
}
