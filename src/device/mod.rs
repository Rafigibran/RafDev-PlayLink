//! ADB wrapper and device manager (PRD sections 4.1 and 8).
//!
//! All device communication goes through the `adb` command-line tool.
//! The binary is resolved in this order:
//! 1. `Settings.adb_path` when set.
//! 2. `adb` on PATH.
//! 3. `%LOCALAPPDATA%/RafDev/PlayLink/platform-tools/adb.exe` (downloaded
//!    on first run, Apache-2.0, see THIRD-PARTY-NOTICES.md).

use crate::core::{ConnectionState, ConnectionType, PlayLinkError, Result, Settings};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One Android device, as discovered through ADB.
pub use crate::core::DeviceInfo;

/// Thin wrapper around the `adb` executable.
pub struct Adb {
    exe: PathBuf,
}

impl Adb {
    /// Resolve the adb binary (see module docs). Returns
    /// [`PlayLinkError::AdbNotFound`] when nothing is found.
    pub fn locate(preferred: &str) -> Result<Self> {
        // 1. Explicit path from Settings (must exist to count).
        if !preferred.is_empty() {
            let path = PathBuf::from(preferred);
            if path.is_file() {
                return Ok(Self { exe: path });
            }
        }
        // 2. `adb` on PATH.
        if let Some(path) = find_on_path() {
            return Ok(Self { exe: path });
        }
        // 3. Downloaded platform-tools in the components directory.
        let bundled = Self::components_dir()
            .join("platform-tools")
            .join(adb_exe_name());
        if bundled.is_file() {
            return Ok(Self { exe: bundled });
        }
        Err(PlayLinkError::AdbNotFound)
    }

    /// Directory used for downloaded components (scrcpy-server,
    /// platform-tools) and persisted device data.
    pub fn components_dir() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("RafDev")
            .join("PlayLink")
    }

    /// `adb version` (used for diagnostics).
    pub fn version(&self) -> Result<String> {
        let out = self.run_ok(&["version"])?;
        Ok(out.lines().next().unwrap_or_default().trim().to_string())
    }

    /// Start (or confirm) the adb server.
    pub fn ensure_server(&self) -> Result<()> {
        self.run_ok(&["start-server"])?;
        Ok(())
    }

    /// `adb devices -l`: all attached devices with model metadata.
    pub fn devices(&self) -> Result<Vec<DeviceInfo>> {
        let output = self.run_ok(&["devices", "-l"])?;
        let mut devices = Vec::new();
        for (serial, state, model) in parse_devices_output(&output) {
            // getprop only works on authorized, connected devices; for
            // unauthorized/offline rows keep the parsed data as-is.
            let mut info = if state == ConnectionState::Connected {
                self.device_props(&serial)
                    .unwrap_or_else(|_| blank_device(&serial, state.clone(), model.clone()))
            } else {
                blank_device(&serial, state.clone(), model.clone())
            };
            info.state = state;
            if info.model.is_empty() {
                info.model = model;
            }
            devices.push(info);
        }
        Ok(devices)
    }

    /// Fill in model / Android version / SDK for one serial via getprop.
    pub fn device_props(&self, serial: &str) -> Result<DeviceInfo> {
        let sdk = self
            .getprop(serial, "ro.build.version.sdk")
            .parse::<u32>()
            .unwrap_or(0);
        Ok(DeviceInfo {
            serial: serial.to_string(),
            model: self.getprop(serial, "ro.product.model"),
            manufacturer: self.getprop(serial, "ro.product.manufacturer"),
            android_version: self.getprop(serial, "ro.build.version.release"),
            sdk,
            connection: connection_type_for(serial),
            state: ConnectionState::Connected,
            last_connected: None,
            preferred: false,
        })
    }

    /// Run `adb -s <serial> shell <args>` and return stdout.
    pub fn shell(&self, serial: &str, args: &[&str]) -> Result<String> {
        let mut full = vec!["-s", serial, "shell"];
        full.extend_from_slice(args);
        self.run_ok(&full)
    }

    /// `adb -s <serial> push <local> <remote>`.
    pub fn push(&self, serial: &str, local: &Path, remote: &str) -> Result<()> {
        let local = local.to_string_lossy().into_owned();
        self.run_ok(&["-s", serial, "push", &local, remote])?;
        Ok(())
    }

    /// `adb -s <serial> forward tcp:<port> <remote>`; returns the port used.
    pub fn forward(&self, serial: &str, local_port: u16, remote: &str) -> Result<u16> {
        let local = format!("tcp:{}", local_port);
        let out = self.run_ok(&["-s", serial, "forward", &local, remote])?;
        Ok(out.trim().parse::<u16>().unwrap_or(local_port))
    }

    pub fn remove_forward(&self, serial: &str, local_port: u16) -> Result<()> {
        let local = format!("tcp:{}", local_port);
        self.run_ok(&["-s", serial, "forward", "--remove", &local])?;
        Ok(())
    }

    /// `adb -s <serial> tcpip <port>` (switch a USB device to TCP mode).
    pub fn tcpip(&self, serial: &str, port: u16) -> Result<()> {
        self.run_ok(&["-s", serial, "tcpip", &port.to_string()])?;
        Ok(())
    }

    /// `adb connect <host:port>`.
    pub fn connect(&self, host_port: &str) -> Result<String> {
        let out = self.run_ok(&["connect", host_port])?;
        let out = out.trim().to_string();
        // `adb connect` exits 0 even when the device refuses the
        // connection, so check the message itself.
        if out.starts_with("connected") || out.starts_with("already connected") {
            Ok(out)
        } else {
            Err(PlayLinkError::AdbFailed(format!(
                "Could not connect to {}: {}",
                host_port, out
            )))
        }
    }

    /// `adb pair <host:port> <code>` (Android 11+ wireless debugging).
    pub fn pair(&self, host_port: &str, code: &str) -> Result<String> {
        let out = self.run_ok(&["pair", host_port, code])?;
        let out = out.trim().to_string();
        if out.starts_with("Successfully paired") {
            Ok(out)
        } else {
            Err(PlayLinkError::AdbFailed(format!(
                "Could not pair with {}: {}",
                host_port, out
            )))
        }
    }

    /// `adb disconnect <host:port>`.
    pub fn disconnect(&self, host_port: &str) -> Result<()> {
        self.run_ok(&["disconnect", host_port])?;
        Ok(())
    }

    /// Run adb with the given arguments, returning raw output.
    fn run(&self, args: &[&str]) -> Result<std::process::Output> {
        Command::new(&self.exe)
            .args(args)
            .output()
            .map_err(PlayLinkError::Io)
    }

    /// Run adb and return stdout, or AdbFailed on a non-zero exit.
    fn run_ok(&self, args: &[&str]) -> Result<String> {
        let output = self.run(args)?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(PlayLinkError::AdbFailed(format!(
                "adb {} exited with {}: {}",
                args.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    /// One getprop value (empty string when the device does not answer).
    fn getprop(&self, serial: &str, prop: &str) -> String {
        self.shell(serial, &["getprop", prop])
            .map(|value| value.trim().to_string())
            .unwrap_or_default()
    }
}

/// Saved-device record (persisted as JSON in the app directory).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedDevice {
    pub serial: String,
    pub model: String,
    pub connection: ConnectionType,
    pub last_connected: String,
    pub preferred: bool,
}

/// Discovers devices, keeps the known-device list, and owns the currently
/// selected device.
pub struct DeviceManager {
    adb: Adb,
    known: Vec<SavedDevice>,
}

impl DeviceManager {
    pub fn new(adb: Adb) -> Self {
        Self {
            adb,
            known: Self::load_known(),
        }
    }

    pub fn adb(&self) -> &Adb {
        &self.adb
    }

    /// Refresh the live device list, merging saved-device metadata
    /// (preferred flag, last connected). Returns the live list.
    pub fn refresh(&mut self) -> Result<Vec<DeviceInfo>> {
        let mut live = self.adb.devices()?;
        for device in &mut live {
            if let Some(saved) = self.known.iter().find(|k| k.serial == device.serial) {
                device.preferred = saved.preferred;
                device.last_connected = if saved.last_connected.is_empty() {
                    None
                } else {
                    Some(saved.last_connected.clone())
                };
            }
        }
        Ok(live)
    }

    pub fn known_devices(&self) -> &[SavedDevice] {
        &self.known
    }

    /// Mark a device as preferred (auto-connect target).
    pub fn set_preferred(&mut self, serial: &str) -> Result<()> {
        for device in &mut self.known {
            device.preferred = false;
        }
        match self.known.iter_mut().find(|d| d.serial == serial) {
            Some(device) => device.preferred = true,
            None => self.known.push(SavedDevice {
                serial: serial.to_string(),
                model: String::new(),
                connection: connection_type_for(serial),
                last_connected: String::new(),
                preferred: true,
            }),
        }
        self.save_known()
    }

    /// Forget a saved device (PRD section 8).
    pub fn forget(&mut self, serial: &str) -> Result<()> {
        self.known.retain(|d| d.serial != serial);
        self.save_known()
    }

    pub fn load_known() -> Vec<SavedDevice> {
        let text = match std::fs::read_to_string(Self::known_path()) {
            Ok(text) => text,
            Err(_) => return Vec::new(),
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save_known(&self) -> Result<()> {
        let path = Self::known_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(&self.known)
            .map_err(|e| PlayLinkError::Config(format!("Cannot serialize known devices: {}", e)))?;
        std::fs::write(&path, text)?;
        Ok(())
    }

    /// Path of the persisted known-device list.
    fn known_path() -> PathBuf {
        Settings::app_dir().join("devices.json")
    }
}

/// Parse the output of `adb devices -l` into (serial, state, model) rows.
/// Pure function, unit-tested.
///
/// Rows whose state is not `device`, `unauthorized`, or `offline` are
/// skipped (for example `no permissions` rows and the header line).
pub fn parse_devices_output(output: &str) -> Vec<(String, ConnectionState, String)> {
    let mut rows = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else {
            continue;
        };
        let Some(state_token) = parts.next() else {
            continue;
        };
        let state = match state_token {
            "device" => ConnectionState::Connected,
            "unauthorized" => ConnectionState::Unauthorized,
            "offline" => ConnectionState::Offline,
            _ => continue,
        };
        let mut model = String::new();
        for token in parts {
            if let Some(value) = token.strip_prefix("model:") {
                model = value.to_string();
                break;
            }
        }
        rows.push((serial.to_string(), state, model));
    }
    rows
}

/// Executable name for the current platform.
fn adb_exe_name() -> &'static str {
    if cfg!(windows) {
        "adb.exe"
    } else {
        "adb"
    }
}

/// Search PATH for an adb binary.
fn find_on_path() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(adb_exe_name());
        if candidate.is_file() {
            return Some(candidate);
        }
        // On Windows also accept a bare `adb` (PATHEXT would resolve it).
        if cfg!(windows) {
            let plain = dir.join("adb");
            if plain.is_file() {
                return Some(plain);
            }
        }
    }
    None
}

/// Wi-Fi serials look like host:port (for example 192.168.1.5:5555);
/// anything else is a USB serial.
fn connection_type_for(serial: &str) -> ConnectionType {
    let looks_like_host_port =
        serial.contains(':') && serial.chars().any(|c| !c.is_ascii_digit() && c != ':');
    if looks_like_host_port {
        ConnectionType::Wifi
    } else {
        ConnectionType::Usb
    }
}

/// DeviceInfo with only the data parsed from `adb devices -l`.
fn blank_device(serial: &str, state: ConnectionState, model: String) -> DeviceInfo {
    DeviceInfo {
        serial: serial.to_string(),
        model,
        manufacturer: String::new(),
        android_version: String::new(),
        sdk: 0,
        connection: connection_type_for(serial),
        state,
        last_connected: None,
        preferred: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "List of devices attached\n\
        emulator-5554          device product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:emulator64_x86_64_arm64 transport_id:1\n\
        192.168.1.5:5555       device product:panther model:Pixel_7 device:panther transport_id:2\n\
        RF8M32KABCD            unauthorized\n\
        HT7A1B2C3D4E           offline\n\
        XYZ000                 device\n\
        BAD999                 no permissions\n\
        \n";

    #[test]
    fn parses_devices_with_models() {
        let rows = parse_devices_output(SAMPLE);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].0, "emulator-5554");
        assert_eq!(rows[0].1, ConnectionState::Connected);
        assert_eq!(rows[0].2, "sdk_gphone64_x86_64");
        assert_eq!(rows[1].0, "192.168.1.5:5555");
        assert_eq!(rows[1].1, ConnectionState::Connected);
        assert_eq!(rows[1].2, "Pixel_7");
    }

    #[test]
    fn parses_unauthorized_and_offline_rows() {
        let rows = parse_devices_output(SAMPLE);
        assert_eq!(rows[2].0, "RF8M32KABCD");
        assert_eq!(rows[2].1, ConnectionState::Unauthorized);
        assert_eq!(rows[2].2, "");
        assert_eq!(rows[3].0, "HT7A1B2C3D4E");
        assert_eq!(rows[3].1, ConnectionState::Offline);
        assert_eq!(rows[3].2, "");
    }

    #[test]
    fn missing_model_is_empty_string() {
        let rows = parse_devices_output(SAMPLE);
        assert_eq!(rows[4].0, "XYZ000");
        assert_eq!(rows[4].1, ConnectionState::Connected);
        assert_eq!(rows[4].2, "");
    }

    #[test]
    fn skips_header_empty_lines_and_unknown_states() {
        assert!(parse_devices_output("List of devices attached\n").is_empty());
        assert!(parse_devices_output("").is_empty());
        assert!(parse_devices_output("ABC123\tno permissions\n").is_empty());
        assert!(parse_devices_output("onlyaserial\n").is_empty());
    }

    #[test]
    fn connection_type_detection() {
        assert_eq!(
            connection_type_for("192.168.1.5:5555"),
            ConnectionType::Wifi
        );
        assert_eq!(connection_type_for("RF8M32KABCD"), ConnectionType::Usb);
        assert_eq!(connection_type_for("emulator-5554"), ConnectionType::Usb);
    }
}
