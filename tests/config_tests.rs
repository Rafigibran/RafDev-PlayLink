//! Settings defaults and TOML round-trip (integration tests).
//!
//! These tests exercise only the public `Settings` API: the defaults from
//! `Settings::default()` and a serialize/deserialize round-trip through
//! TOML, which is the on-disk config format.

use rafdev_playlink::core::{OutputAspect, Settings, VideoPreset};

#[test]
fn default_settings_have_expected_values() {
    let settings = Settings::default();

    // General
    assert!(!settings.launch_on_startup);
    assert!(!settings.minimize_to_tray);
    assert!(!settings.start_minimized);
    assert_eq!(settings.language, "en");
    assert_eq!(settings.theme, "light");
    assert!(settings.notifications);
    assert!(settings.auto_reconnect);

    // Video
    assert_eq!(settings.video_preset, VideoPreset::Balanced);
    assert_eq!(settings.custom_max_size, 1920);
    assert_eq!(settings.custom_max_fps, 30);
    assert_eq!(settings.custom_bitrate_bps, 8_000_000);
    assert!(settings.hardware_decoding);
    assert!(settings.orientation_auto);
    assert_eq!(settings.scaling_mode, "fit");

    // Audio
    assert!(!settings.audio_forwarding);
    assert_eq!(settings.audio_output_device, "default");
    assert!((settings.audio_volume - 1.0).abs() < f32::EPSILON);
    assert_eq!(settings.audio_delay_ms, 0);
    assert!(!settings.audio_monitoring);

    // Streaming
    assert_eq!(settings.clean_feed_aspect, OutputAspect::Portrait);
    assert!(settings.clean_feed_on_top);
    assert!(!settings.virtual_camera_enabled);
    assert_eq!(settings.disconnect_background, "dark");

    // Storage: folders default under the user Pictures / Videos dirs.
    assert!(settings.screenshot_folder.ends_with("PlayLink"));
    assert!(settings.recording_folder.ends_with("PlayLink"));
    assert_eq!(settings.recording_format, "mp4");
    assert_eq!(settings.max_recording_mb, 4096);

    // Advanced / devices
    assert!(settings.adb_path.is_empty());
    assert_eq!(settings.network_timeout_secs, 10);
    assert!(!settings.debug_logs);
    assert!(settings.preferred_device_serial.is_empty());
}

#[test]
fn default_settings_toml_round_trip() {
    let original = Settings::default();
    let text = toml::to_string_pretty(&original).expect("settings serialize to TOML");
    let parsed: Settings = toml::from_str(&text).expect("settings deserialize from TOML");

    assert_eq!(parsed.launch_on_startup, original.launch_on_startup);
    assert_eq!(parsed.minimize_to_tray, original.minimize_to_tray);
    assert_eq!(parsed.start_minimized, original.start_minimized);
    assert_eq!(parsed.language, original.language);
    assert_eq!(parsed.theme, original.theme);
    assert_eq!(parsed.notifications, original.notifications);
    assert_eq!(parsed.auto_reconnect, original.auto_reconnect);
    assert_eq!(parsed.video_preset, original.video_preset);
    assert_eq!(parsed.custom_max_size, original.custom_max_size);
    assert_eq!(parsed.custom_max_fps, original.custom_max_fps);
    assert_eq!(parsed.custom_bitrate_bps, original.custom_bitrate_bps);
    assert_eq!(parsed.hardware_decoding, original.hardware_decoding);
    assert_eq!(parsed.orientation_auto, original.orientation_auto);
    assert_eq!(parsed.scaling_mode, original.scaling_mode);
    assert_eq!(parsed.audio_forwarding, original.audio_forwarding);
    assert_eq!(parsed.audio_output_device, original.audio_output_device);
    assert!((parsed.audio_volume - original.audio_volume).abs() < f32::EPSILON);
    assert_eq!(parsed.audio_delay_ms, original.audio_delay_ms);
    assert_eq!(parsed.audio_monitoring, original.audio_monitoring);
    assert_eq!(parsed.clean_feed_aspect, original.clean_feed_aspect);
    assert_eq!(parsed.clean_feed_on_top, original.clean_feed_on_top);
    assert_eq!(
        parsed.virtual_camera_enabled,
        original.virtual_camera_enabled
    );
    assert_eq!(parsed.disconnect_background, original.disconnect_background);
    assert_eq!(parsed.screenshot_folder, original.screenshot_folder);
    assert_eq!(parsed.recording_folder, original.recording_folder);
    assert_eq!(parsed.recording_format, original.recording_format);
    assert_eq!(parsed.max_recording_mb, original.max_recording_mb);
    assert_eq!(parsed.adb_path, original.adb_path);
    assert_eq!(parsed.network_timeout_secs, original.network_timeout_secs);
    assert_eq!(parsed.debug_logs, original.debug_logs);
    assert_eq!(
        parsed.preferred_device_serial,
        original.preferred_device_serial
    );
}

#[test]
fn modified_settings_toml_round_trip() {
    let mut settings = Settings::default();
    settings.video_preset = VideoPreset::High;
    settings.custom_max_fps = 60;
    settings.audio_forwarding = true;
    settings.audio_volume = 0.5;
    settings.adb_path = "C:\\platform-tools\\adb.exe".to_string();
    settings.preferred_device_serial = "RF8M32KABCD".to_string();
    settings.clean_feed_aspect = OutputAspect::Landscape;

    let text = toml::to_string_pretty(&settings).expect("settings serialize to TOML");
    let parsed: Settings = toml::from_str(&text).expect("settings deserialize from TOML");

    assert_eq!(parsed.video_preset, VideoPreset::High);
    assert_eq!(parsed.custom_max_fps, 60);
    assert!(parsed.audio_forwarding);
    assert!((parsed.audio_volume - 0.5).abs() < f32::EPSILON);
    assert_eq!(parsed.adb_path, "C:\\platform-tools\\adb.exe");
    assert_eq!(parsed.preferred_device_serial, "RF8M32KABCD");
    assert_eq!(parsed.clean_feed_aspect, OutputAspect::Landscape);
}
