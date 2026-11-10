//! ADB wrapper and device manager (PRD sections 4.1 and 8).
//!
//! All device communication goes through the `adb` command-line tool.
//! The binary is resolved in this order:
//! 1. `Settings.adb_path` when set.
//! 2. `adb` on PATH.
//! 3. The platform-tools copy in the components directory, downloaded
//!    from Google on first run when it is missing:
//!    - Windows: `%LOCALAPPDATA%/RafDev/PlayLink/platform-tools/adb.exe`
//!    - macOS: `~/Library/Application Support/RafDev/PlayLink/platform-tools/adb`
//!    - Linux: `~/.local/share/RafDev/PlayLink/platform-tools/adb`
//!    (Apache-2.0, see THIRD-PARTY-NOTICES.md).

use crate::core::{ConnectionState, ConnectionType, PlayLinkError, Result, Settings};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A platform-tools download is attempted at most once per process run:
///
/// a failed first run (no network, Google unreachable) must not make every
/// later device refresh re-download a ~15 MB archive.
static PLATFORM_TOOLS_DOWNLOAD_ATTEMPTED: AtomicBool = AtomicBool::new(false);

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
        //    On first run the copy is missing, so try downloading it once
        //    (Google's platform-tools archive for this OS) and re-check.
        let bundled = Self::components_dir()
            .join("platform-tools")
            .join(adb_exe_name());
        if bundled.is_file() {
            return Ok(Self { exe: bundled });
        }
        if !PLATFORM_TOOLS_DOWNLOAD_ATTEMPTED.swap(true, Ordering::SeqCst) {
            if download_platform_tools().is_ok() && bundled.is_file() {
                return Ok(Self { exe: bundled });
            }
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

/// Google's platform-tools archive for the current OS.
fn platform_tools_url() -> &'static str {
    if cfg!(windows) {
        "https://dl.google.com/android/repository/platform-tools-latest-windows.zip"
    } else if cfg!(target_os = "macos") {
        "https://dl.google.com/android/repository/platform-tools-latest-darwin.zip"
    } else {
        "https://dl.google.com/android/repository/platform-tools-latest-linux.zip"
    }
}

/// Download the platform-tools archive for this OS and extract it into the
/// components directory, so `adb` lands at
/// `<components>/platform-tools/adb[.exe]`. Returns an error string on any
/// failure; the caller treats that as "adb still not found".
fn download_platform_tools() -> std::result::Result<(), String> {
    let url = platform_tools_url();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .user_agent("RafDev-PlayLink/1.0")
        .build()
        .map_err(|e| format!("could not build the HTTP client: {}", e))?;
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("platform-tools download request failed: {}", e))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "platform-tools download failed: HTTP {} for {}",
            status, url
        ));
    }
    let bytes = response
        .bytes()
        .map_err(|e| format!("could not read the platform-tools download: {}", e))?;
    if bytes.len() < 4 || &bytes[..4] != b"PK\x03\x04" {
        return Err(
            "downloaded file is not a platform-tools zip (bad magic bytes)".to_string(),
        );
    }
    let dir = Adb::components_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {}", dir.display(), e))?;
    let zip_path = dir.join("platform-tools.zip");
    std::fs::write(&zip_path, &bytes)
        .map_err(|e| format!("could not save {}: {}", zip_path.display(), e))?;
    let result = extract_platform_tools_zip(&zip_path, &dir);
    let _ = std::fs::remove_file(&zip_path);
    result
}

/// Extract a platform-tools zip into `dest` (the components directory).
/// The archive has one top-level `platform-tools/` folder; entries are
/// validated with `enclosed_name` so a hostile archive cannot write
/// outside `dest`. On Unix the `adb` binary is made executable.
fn extract_platform_tools_zip(zip_path: &Path, dest: &Path) -> std::result::Result<(), String> {
    let file = std::fs::File::open(zip_path)
        .map_err(|e| format!("could not open {}: {}", zip_path.display(), e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("could not read the platform-tools zip: {}", e))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("could not read zip entry {}: {}", i, e))?;
        let Some(out_path) = entry.enclosed_name().map(|p| dest.join(p)) else {
            continue;
        };
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| format!("could not create {}: {}", out_path.display(), e))?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("could not create {}: {}", parent.display(), e))?;
            }
            let mut out = std::fs::File::create(&out_path)
                .map_err(|e| format!("could not write {}: {}", out_path.display(), e))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| format!("could not extract {}: {}", out_path.display(), e))?;
        }
    }
    let adb = dest.join("platform-tools").join(adb_exe_name());
    if !adb.is_file() {
        return Err(format!(
            "platform-tools zip did not contain {}",
            adb.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&adb, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
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

    #[test]
    fn platform_tools_url_matches_os() {
        let url = platform_tools_url();
        assert!(url.starts_with("https://dl.google.com/android/repository/platform-tools-latest-"));
        assert!(url.ends_with(".zip"));
        if cfg!(windows) {
            assert!(url.contains("-windows.zip"));
        } else if cfg!(target_os = "macos") {
            assert!(url.contains("-darwin.zip"));
        } else {
            assert!(url.contains("-linux.zip"));
        }
    }

    #[test]
    fn adb_exe_name_matches_os() {
        if cfg!(windows) {
            assert_eq!(adb_exe_name(), "adb.exe");
        } else {
            assert_eq!(adb_exe_name(), "adb");
        }
    }

    /// Build a minimal platform-tools-shaped zip in memory.
    fn fake_platform_tools_zip(with_adb: bool) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options =
                zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            use std::io::Write;
            if with_adb {
                zip.start_file("platform-tools/adb", options).unwrap();
                zip.write_all(b"fake-adb").unwrap();
            }
            zip.start_file("platform-tools/NOTICE.txt", options).unwrap();
            zip.write_all(b"notice").unwrap();
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn extracts_platform_tools_zip_and_marks_adb_executable() {
        let dir = std::env::temp_dir().join(format!(
            "playlink-test-extract-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("platform-tools.zip");
        std::fs::write(&zip_path, fake_platform_tools_zip(true)).unwrap();
        let dest = dir.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        assert!(extract_platform_tools_zip(&zip_path, &dest).is_ok());
        let adb = dest.join("platform-tools").join(adb_exe_name());
        assert!(adb.is_file());
        assert_eq!(std::fs::read(&adb).unwrap(), b"fake-adb");
        assert!(dest.join("platform-tools").join("NOTICE.txt").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&adb).unwrap().permissions().mode() & 0o111, 0o111);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zip_without_adb_is_an_error() {
        let dir = std::env::temp_dir().join(format!(
            "playlink-test-noadb-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("platform-tools.zip");
        std::fs::write(&zip_path, fake_platform_tools_zip(false)).unwrap();
        let dest = dir.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        assert!(extract_platform_tools_zip(&zip_path, &dest).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
