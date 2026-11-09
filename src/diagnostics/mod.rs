//! Diagnostics engine (PRD sections 9, 14): logging, error reporting,
//! performance counters, diagnostic export.

use std::path::PathBuf;

/// Initialise `tracing` logging. `debug` enables verbose engine logs.
pub fn init_logging(debug: bool) {
    let level = if debug { "debug" } else { "info" };
    let filter = format!("rafdev_playlink={},playlink={}", level, level);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Path of the current session log file.
pub fn log_path() -> PathBuf {
    crate::core::Settings::app_dir().join("playlink.log")
}

/// Export a diagnostics bundle (log tail + versions + device list) for
/// bug reports. Returns the bundle path.
pub fn export_diagnostics() -> std::io::Result<PathBuf> {
    let path = crate::core::Settings::app_dir().join("diagnostics.txt");
    let mut text = String::new();
    text.push_str(&format!("RafDev PlayLink {}\n", env!("CARGO_PKG_VERSION")));
    text.push_str(&format!("OS: {}\n", std::env::consts::OS));
    text.push_str(&format!("Arch: {}\n", std::env::consts::ARCH));
    if let Ok(log) = std::fs::read_to_string(log_path()) {
        text.push_str("\n--- log tail ---\n");
        let tail: Vec<&str> = log.lines().rev().take(200).collect();
        for line in tail.iter().rev() {
            text.push_str(line);
            text.push('\n');
        }
    }
    std::fs::create_dir_all(crate::core::Settings::app_dir())?;
    std::fs::write(&path, text)?;
    Ok(path)
}
