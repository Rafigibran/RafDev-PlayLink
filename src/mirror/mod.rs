//! Mirror engine (PRD sections 4.2, 11, 18.1).
//!
//! The engine drives the upstream scrcpy server (Apache-2.0) on the device:
//!
//! 1. Download `scrcpy-server` once (first run) into the components dir.
//! 2. `adb push` it to `/data/local/tmp/scrcpy-server`.
//! 3. Start it with `app_process / com.genymobile.scrcpy.Server <ver>
//!    tunnel_forward=true control=true video_codec=h264 ...`.
//! 4. Connect to the forwarded video socket, then to a second connection on
//!    the same socket for control messages (scrcpy protocol: the server
//!    accepts connections in order video, then control).
//! 5. Decode the H.264 stream with OpenH264 and publish RGBA frames to
//!    [`SharedState`], where the Slint UI and Clean Feed read them.
//!
//! Video stream format (scrcpy >= 2.0): 12-byte header, codec id (4 ASCII
//! bytes, e.g. `h264`), width u32 BE, height u32 BE, then packets:
//! PTS u64 BE (`0xFFFFFFFFFFFFFFFF` = config packet), size u32 BE, payload.
//! The first config packet carries SPS/PPS.

use crate::core::{
    Frame, MirrorStats, PlayLinkError, RecordingState, Result, Settings, SharedState,
};
use crate::device::{Adb, DeviceInfo};
use openh264::decoder::Decoder;
use std::collections::VecDeque;
use std::io::{ErrorKind, Read};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// Pinned upstream scrcpy server version. The server file is downloaded from
/// `https://github.com/Genymobile/scrcpy/releases/download/v{ver}/scrcpy-server-v{ver}`.
/// If the pinned release ever disappears, `ensure_server_binary` falls back
/// to the latest release tag via the GitHub API.
pub const SCRCPY_SERVER_VERSION: &str = "3.3.3";

/// Local TCP port the scrcpy server is forwarded to.
const SCRCPY_PORT: u16 = 27183;
/// Where the server binary is pushed on the device.
const SERVER_REMOTE_PATH: &str = "/data/local/tmp/scrcpy-server";
/// Largest single video packet accepted; anything bigger means a corrupt stream.
const MAX_PACKET_BYTES: usize = 16 * 1024 * 1024;
/// Socket read timeout inside the decode loop, so the stop flag is rechecked.
const READ_TIMEOUT: Duration = Duration::from_millis(500);
/// Upper bound for the raw recording queue (about 512 MB of packets).
const MAX_RECORDED_BYTES: usize = 512 * 1024 * 1024;
/// Largest decoded frame dimension accepted (guards against corrupt streams).
const MAX_FRAME_DIMENSION: usize = 8192;

/// Mirroring parameters for one session.
#[derive(Debug, Clone, Copy)]
pub struct MirrorConfig {
    pub max_size: u32,
    pub max_fps: u32,
    pub bitrate_bps: u32,
}

impl MirrorConfig {
    pub fn from_settings(settings: &Settings) -> Self {
        let (max_size, max_fps, bitrate_bps) = settings.mirror_params();
        Self {
            max_size,
            max_fps,
            bitrate_bps,
        }
    }
}

/// Download the scrcpy-server file once (or return the cached copy).
/// The file is Apache-2.0; see THIRD-PARTY-NOTICES.md.
pub fn ensure_server_binary() -> Result<PathBuf> {
    let dir = Adb::components_dir();
    let file_name = format!("scrcpy-server-v{}", SCRCPY_SERVER_VERSION);
    let path = dir.join(&file_name);
    if is_usable_server_file(&path) {
        return Ok(path);
    }
    if path.exists() {
        // A stale, empty, or truncated cache entry: drop it and re-download.
        let _ = std::fs::remove_file(&path);
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(PlayLinkError::ServerDownloadFailed(format!(
            "Could not create the components directory {}: {}",
            dir.display(),
            e
        )));
    }
    match download_server(&path, SCRCPY_SERVER_VERSION) {
        Ok(()) => Ok(path),
        Err(first_err) => {
            tracing::warn!(
                "pinned scrcpy-server download failed ({}), trying the latest release",
                first_err
            );
            match latest_release_tag() {
                Ok(tag) => {
                    let fallback_version = tag.strip_prefix('v').unwrap_or(&tag);
                    if fallback_version == SCRCPY_SERVER_VERSION {
                        return Err(PlayLinkError::ServerDownloadFailed(first_err));
                    }
                    let alt_path = dir.join(format!("scrcpy-server-v{}", fallback_version));
                    if is_usable_server_file(&alt_path) {
                        return Ok(alt_path);
                    }
                    download_server(&alt_path, fallback_version)
                        .map(|()| alt_path)
                        .map_err(|e| {
                            PlayLinkError::ServerDownloadFailed(format!(
                                "Could not download the mirroring server (pinned v{}: {}; latest {}: {})",
                                SCRCPY_SERVER_VERSION, first_err, tag, e
                            ))
                        })
                }
                Err(api_err) => Err(PlayLinkError::ServerDownloadFailed(format!(
                    "Could not download the mirroring server ({}); also could not resolve the latest release: {}",
                    first_err, api_err
                ))),
            }
        }
    }
}

/// True when `path` is a non-empty scrcpy-server jar (ZIP magic `PK\x03\x04`).
fn is_usable_server_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > 0 => match std::fs::File::open(path) {
            Ok(mut file) => {
                let mut magic = [0u8; 4];
                file.read_exact(&mut magic).is_ok() && magic == *b"PK\x03\x04"
            }
            Err(_) => false,
        },
        _ => false,
    }
}

/// Download one scrcpy-server release file and verify it (non-empty, jar magic).
fn download_server(path: &Path, version: &str) -> std::result::Result<(), String> {
    let url = format!(
        "https://github.com/Genymobile/scrcpy/releases/download/v{version}/scrcpy-server-v{version}"
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("RafDev-PlayLink/1.0")
        .build()
        .map_err(|e| format!("could not build the HTTP client: {}", e))?;
    let response = client
        .get(&url)
        .send()
        .map_err(|e| format!("download request failed: {}", e))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("download failed: HTTP {} for {}", status, url));
    }
    let bytes = response
        .bytes()
        .map_err(|e| format!("could not read the download: {}", e))?;
    if bytes.is_empty() {
        return Err("downloaded file is empty".to_string());
    }
    if bytes.len() < 4 || &bytes[..4] != b"PK\x03\x04" {
        return Err("downloaded file is not a scrcpy-server jar (bad magic bytes)".to_string());
    }
    std::fs::write(path, &bytes)
        .map_err(|e| format!("could not save {}: {}", path.display(), e))?;
    Ok(())
}

/// Resolve the latest scrcpy release tag (for example `v3.3.3`) via GitHub.
fn latest_release_tag() -> std::result::Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("RafDev-PlayLink/1.0")
        .build()
        .map_err(|e| format!("could not build the HTTP client: {}", e))?;
    let response = client
        .get("https://api.github.com/repos/Genymobile/scrcpy/releases/latest")
        .send()
        .map_err(|e| format!("GitHub API request failed: {}", e))?;
    if !response.status().is_success() {
        return Err(format!("GitHub API returned HTTP {}", response.status()));
    }
    let json: serde_json::Value = response
        .json()
        .map_err(|e| format!("could not parse the GitHub API response: {}", e))?;
    json.get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "GitHub API response has no tag_name".to_string())
}

/// Parse one scrcpy video packet header from the stream.
/// Returns `(pts_us, is_config, payload_len)`. Pure function, unit-tested.
pub fn parse_video_packet_header(buf: &[u8]) -> Option<(u64, bool, usize)> {
    if buf.len() < 12 {
        return None;
    }
    let pts = u64::from_be_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]);
    let size = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
    Some((pts, pts == u64::MAX, size as usize))
}

/// Bounded queue of raw video packets kept while recording is armed.
struct RecordQueue {
    packets: VecDeque<Vec<u8>>,
    bytes: usize,
}

impl RecordQueue {
    fn new() -> Self {
        Self {
            packets: VecDeque::new(),
            bytes: 0,
        }
    }

    /// Append one raw packet, dropping the oldest packets past the cap so a
    /// long session cannot grow memory without bound.
    fn push(&mut self, packet: Vec<u8>) {
        if packet.len() > MAX_RECORDED_BYTES {
            return;
        }
        while self.bytes + packet.len() > MAX_RECORDED_BYTES {
            match self.packets.pop_front() {
                Some(old) => self.bytes -= old.len(),
                None => break,
            }
        }
        self.bytes += packet.len();
        self.packets.push_back(packet);
    }

    /// Drain everything queued so far into one concatenated buffer.
    fn drain(&mut self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes);
        while let Some(packet) = self.packets.pop_front() {
            out.extend_from_slice(&packet);
        }
        self.bytes = 0;
        out
    }
}

/// Rolling counters for one mirroring session (survives a reconnect).
#[derive(Default)]
struct StreamCounters {
    seq: u64,
    frames_total: u64,
    frames_dropped: u64,
    frame_times: VecDeque<Instant>,
    byte_window: VecDeque<(Instant, usize)>,
}

/// One live session: the sockets the decode thread reads from.
/// The server child process lives in the shared child slot so both the
/// engine and the decode thread can stop it.
struct Session {
    video: TcpStream,
    control: TcpStream,
    width: u32,
    height: u32,
}

/// Read exactly `buf.len()` bytes, tolerating read timeouts and partial
/// reads. Returns an error when the stream closes or the stop flag is set.
fn read_exact_timeout(
    stream: &mut TcpStream,
    mut buf: &mut [u8],
    stop: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    while !buf.is_empty() {
        if stop.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(
                ErrorKind::Interrupted,
                "stop requested",
            ));
        }
        match stream.read(buf) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "stream closed",
                ))
            }
            Ok(n) => {
                let tmp = buf;
                buf = &mut tmp[n..];
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Resolve the adb executable the same way `Adb::locate` does: the
/// configured path first, then `adb` on PATH, then the downloaded
/// platform-tools copy in the components directory.
fn resolve_adb_exe(preferred: &str) -> PathBuf {
    if !preferred.is_empty() {
        let path = PathBuf::from(preferred);
        if path.is_file() {
            return path;
        }
    }
    if let Some(found) = find_on_path() {
        return found;
    }
    let mut path = Adb::components_dir();
    path.push("platform-tools");
    path.push(if cfg!(windows) { "adb.exe" } else { "adb" });
    path
}

/// Look for `adb` on PATH.
fn find_on_path() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "adb.exe" } else { "adb" };
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Spawn `adb -s <serial> shell ... app_process ... com.genymobile.scrcpy.Server`.
/// The server runs in the foreground of that shell; the returned child must
/// be killed to stop it.
fn spawn_server(adb_exe: &Path, serial: &str, cfg: MirrorConfig) -> Result<Child> {
    let args: Vec<String> = vec![
        "-s".to_string(),
        serial.to_string(),
        "shell".to_string(),
        format!("CLASSPATH={}", SERVER_REMOTE_PATH),
        "app_process".to_string(),
        "/".to_string(),
        "com.genymobile.scrcpy.Server".to_string(),
        SCRCPY_SERVER_VERSION.to_string(),
        "tunnel_forward=true".to_string(),
        "control=true".to_string(),
        "video_codec=h264".to_string(),
        format!("video_bit_rate={}", cfg.bitrate_bps),
        format!("max_size={}", cfg.max_size),
        format!("max_fps={}", cfg.max_fps),
        "audio=false".to_string(),
    ];
    Command::new(adb_exe)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            PlayLinkError::ServerStartFailed(format!(
                "Could not run the adb shell for the mirroring server: {}",
                e
            ))
        })
}

/// Kill the server child held in the shared slot, if any.
fn kill_slot_child(child_slot: &Arc<Mutex<Option<Child>>>) {
    if let Ok(mut slot) = child_slot.lock() {
        if let Some(mut child) = slot.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Tear down a half-built session: kill the server child, remove the forward.
fn abandon_session(adb: &Adb, serial: &str, child_slot: &Arc<Mutex<Option<Child>>>) {
    kill_slot_child(child_slot);
    let _ = adb.remove_forward(serial, SCRCPY_PORT);
}

/// Connect to the forwarded video socket, retrying briefly while the
/// freshly started server begins listening.
fn connect_video(port: u16, stop: &Arc<AtomicBool>) -> Result<TcpStream> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                if stop.load(Ordering::Relaxed) {
                    return Err(PlayLinkError::StreamEnded(
                        "Mirroring stopped while connecting to the video stream".to_string(),
                    ));
                }
                if Instant::now() >= deadline {
                    return Err(PlayLinkError::StreamEnded(format!(
                        "Could not connect to the video stream on port {}: {}",
                        port, e
                    )));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Run steps 1 to 6 of a session: download the server, push it, start it,
/// forward the socket, connect video, read the header, connect control.
/// The server child is installed in `child_slot` right after spawn so the
/// engine can always kill it, even if a later step fails.
fn establish_session(
    adb: &Adb,
    adb_exe: &Path,
    serial: &str,
    cfg: MirrorConfig,
    stop: &Arc<AtomicBool>,
    child_slot: &Arc<Mutex<Option<Child>>>,
) -> Result<Session> {
    let server_path = ensure_server_binary()?;
    if stop.load(Ordering::Relaxed) {
        return Err(PlayLinkError::StreamEnded("Mirroring stopped".to_string()));
    }
    adb.push(serial, &server_path, SERVER_REMOTE_PATH)?;
    if stop.load(Ordering::Relaxed) {
        return Err(PlayLinkError::StreamEnded("Mirroring stopped".to_string()));
    }
    let mut child = spawn_server(adb_exe, serial, cfg)?;
    if stop.load(Ordering::Relaxed) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(PlayLinkError::StreamEnded("Mirroring stopped".to_string()));
    }
    if let Ok(mut slot) = child_slot.lock() {
        *slot = Some(child);
    }
    let port = match adb.forward(serial, SCRCPY_PORT, "localabstract:scrcpy") {
        Ok(port) => port,
        Err(e) => {
            abandon_session(adb, serial, child_slot);
            return Err(e);
        }
    };
    if stop.load(Ordering::Relaxed) {
        abandon_session(adb, serial, child_slot);
        return Err(PlayLinkError::StreamEnded("Mirroring stopped".to_string()));
    }
    let mut video = match connect_video(port, stop) {
        Ok(stream) => stream,
        Err(e) => {
            abandon_session(adb, serial, child_slot);
            return Err(e);
        }
    };
    if let Err(e) = video.set_read_timeout(Some(Duration::from_secs(10))) {
        abandon_session(adb, serial, child_slot);
        return Err(PlayLinkError::StreamEnded(format!(
            "Could not set a read timeout on the video stream: {}",
            e
        )));
    }
    let mut header = [0u8; 12];
    if let Err(e) = read_exact_timeout(&mut video, &mut header, stop) {
        abandon_session(adb, serial, child_slot);
        return Err(PlayLinkError::StreamEnded(format!(
            "Could not read the video header: {}",
            e
        )));
    }
    if let Err(e) = video.set_read_timeout(Some(READ_TIMEOUT)) {
        abandon_session(adb, serial, child_slot);
        return Err(PlayLinkError::StreamEnded(format!(
            "Could not set a read timeout on the video stream: {}",
            e
        )));
    }
    let codec_id = &header[0..4];
    if codec_id != b"h264" {
        abandon_session(adb, serial, child_slot);
        return Err(PlayLinkError::ServerStartFailed(format!(
            "Unexpected video codec '{}', expected h264",
            String::from_utf8_lossy(codec_id)
        )));
    }
    let width = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let height = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    if width == 0 || height == 0 {
        abandon_session(adb, serial, child_slot);
        return Err(PlayLinkError::ServerStartFailed(
            "Server reported a zero video size".to_string(),
        ));
    }
    if stop.load(Ordering::Relaxed) {
        abandon_session(adb, serial, child_slot);
        return Err(PlayLinkError::StreamEnded("Mirroring stopped".to_string()));
    }
    let control = match TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))) {
        Ok(stream) => stream,
        Err(e) => {
            abandon_session(adb, serial, child_slot);
            return Err(PlayLinkError::ServerStartFailed(format!(
                "Could not connect to the control socket: {}",
                e
            )));
        }
    };
    let _ = video.set_nodelay(true);
    let _ = control.set_nodelay(true);
    Ok(Session {
        video,
        control,
        width,
        height,
    })
}

/// Clear the shared mirroring flag.
fn set_mirroring_false(state: &Arc<SharedState>) {
    if let Ok(mut mirroring) = state.mirroring.lock() {
        *mirroring = false;
    }
}

/// Decode loop: read packets, feed the decoder, publish RGBA frames.
/// Returns `Ok` when a clean stop was requested, `Err` when the stream
/// ended or broke. Never panics on malformed packets; they are logged
/// and skipped.
#[allow(clippy::too_many_arguments)]
fn decode_stream(
    video: &mut TcpStream,
    decoder: &mut Decoder,
    state: &Arc<SharedState>,
    stop: &Arc<AtomicBool>,
    record_queue: &Arc<Mutex<RecordQueue>>,
    counters: &mut StreamCounters,
    first_tx: &mpsc::Sender<Result<()>>,
    first_sent: &mut bool,
    session_start: Instant,
) -> Result<()> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut header = [0u8; 12];
        if let Err(e) = read_exact_timeout(video, &mut header, stop) {
            return Err(PlayLinkError::StreamEnded(format!(
                "Video stream ended: {}",
                e
            )));
        }
        let Some((pts_us, _is_config, payload_len)) = parse_video_packet_header(&header) else {
            tracing::warn!("skipping a short video packet header");
            continue;
        };
        if payload_len > MAX_PACKET_BYTES {
            return Err(PlayLinkError::StreamEnded(format!(
                "Oversize video packet ({} bytes), the stream looks corrupt",
                payload_len
            )));
        }
        let mut payload = vec![0u8; payload_len];
        if let Err(e) = read_exact_timeout(video, &mut payload, stop) {
            return Err(PlayLinkError::StreamEnded(format!(
                "Video stream ended: {}",
                e
            )));
        }
        // Keep the raw packet (header plus payload) while recording is armed.
        if state
            .recording
            .lock()
            .map(|r| *r != RecordingState::Idle)
            .unwrap_or(false)
        {
            let mut raw = Vec::with_capacity(12 + payload.len());
            raw.extend_from_slice(&header);
            raw.extend_from_slice(&payload);
            if let Ok(mut queue) = record_queue.lock() {
                queue.push(raw);
            }
        }
        match decoder.decode(&payload) {
            Ok(Some(yuv)) => {
                let (width, height) = yuv.dimension_rgb();
                if width == 0
                    || height == 0
                    || width > MAX_FRAME_DIMENSION
                    || height > MAX_FRAME_DIMENSION
                {
                    tracing::warn!(
                        "skipping a frame with implausible size {}x{}",
                        width,
                        height
                    );
                    continue;
                }
                // OpenH264's own BT.601 full-range YUV to RGBA conversion.
                let mut rgba = vec![0u8; width * height * 4];
                yuv.write_rgba8(&mut rgba);
                counters.seq += 1;
                counters.frames_total += 1;
                // PTS is stream-relative, so latency is measured against
                // time since the session started, clamped at zero.
                let pts_ms = pts_us / 1000;
                let latency_ms = session_start
                    .elapsed()
                    .as_millis()
                    .saturating_sub(pts_ms as u128) as f32;
                let now = Instant::now();
                counters.frame_times.push_back(now);
                counters.byte_window.push_back((now, 12 + payload.len()));
                while counters
                    .frame_times
                    .front()
                    .map_or(false, |t| now.duration_since(*t) > Duration::from_secs(1))
                {
                    counters.frame_times.pop_front();
                }
                while counters.byte_window.front().map_or(false, |(t, _)| {
                    now.duration_since(*t) > Duration::from_secs(1)
                }) {
                    counters.byte_window.pop_front();
                }
                let bitrate_bps = counters
                    .byte_window
                    .iter()
                    .map(|(_, b)| *b as u64)
                    .sum::<u64>()
                    * 8;
                let stats = MirrorStats {
                    fps: counters.frame_times.len() as f32,
                    latency_ms,
                    bitrate_bps,
                    frames_total: counters.frames_total,
                    frames_dropped: counters.frames_dropped,
                    width: width as u32,
                    height: height as u32,
                };
                if let Ok(mut slot) = state.stats.lock() {
                    *slot = stats;
                }
                state.set_frame(Frame {
                    width: width as u32,
                    height: height as u32,
                    rgba,
                    timestamp_ms: pts_ms,
                    seq: counters.seq,
                });
                if !*first_sent {
                    let _ = first_tx.send(Ok(()));
                    *first_sent = true;
                    tracing::info!("first video frame decoded ({}x{})", width, height);
                }
            }
            Ok(None) => {
                // Config packet consumed, no frame yet.
            }
            Err(e) => {
                counters.frames_dropped += 1;
                tracing::warn!("skipping a video packet the decoder rejected: {}", e);
            }
        }
    }
}

/// Owns one mirroring session: server process, video socket, decoder thread,
/// and the control socket handed to [`crate::input::InputController`].
pub struct MirrorEngine {
    adb: Adb,
    state: Arc<SharedState>,
    running: bool,
    serial: Option<String>,
    stop_flag: Option<Arc<AtomicBool>>,
    child_slot: Option<Arc<Mutex<Option<Child>>>>,
    control_slot: Option<Arc<Mutex<Option<(TcpStream, u16, u16)>>>>,
    decode_handle: Option<std::thread::JoinHandle<()>>,
    record_queue: Arc<Mutex<RecordQueue>>,
}

impl MirrorEngine {
    pub fn new(adb: Adb, state: Arc<SharedState>) -> Self {
        Self {
            adb,
            state,
            running: false,
            serial: None,
            stop_flag: None,
            child_slot: None,
            control_slot: None,
            decode_handle: None,
            record_queue: Arc::new(Mutex::new(RecordQueue::new())),
        }
    }

    /// Start mirroring `device` with `cfg`. Blocks only until the first
    /// frame is decoded (or an error is returned); decoding then continues
    /// on a worker thread.
    pub fn start(&mut self, device: &DeviceInfo, cfg: MirrorConfig) -> Result<()> {
        if self.running {
            self.stop();
        }
        if let Some(handle) = self.decode_handle.take() {
            let _ = handle.join();
        }
        let serial = device.serial.clone();
        let adb_path = self
            .state
            .settings
            .lock()
            .map(|s| s.adb_path.clone())
            .unwrap_or_default();
        let adb_exe = resolve_adb_exe(&adb_path);
        self.adb.ensure_server()?;
        let stop = Arc::new(AtomicBool::new(false));
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let control_slot: Arc<Mutex<Option<(TcpStream, u16, u16)>>> = Arc::new(Mutex::new(None));
        let (first_tx, first_rx) = mpsc::channel::<Result<()>>();
        let session = match establish_session(&self.adb, &adb_exe, &serial, cfg, &stop, &child_slot)
        {
            Ok(session) => session,
            Err(e) => {
                self.state
                    .set_status(format!("Could not start mirroring: {}", e));
                return Err(e);
            }
        };
        let Session {
            video,
            control,
            width,
            height,
        } = session;
        if let Ok(mut slot) = control_slot.lock() {
            *slot = Some((
                control,
                width.min(u16::MAX as u32) as u16,
                height.min(u16::MAX as u32) as u16,
            ));
        }
        if let Ok(mut mirroring) = self.state.mirroring.lock() {
            *mirroring = true;
        }
        if let Ok(mut slot) = self.state.stats.lock() {
            slot.width = width;
            slot.height = height;
        }
        self.stop_flag = Some(stop.clone());
        self.child_slot = Some(child_slot.clone());
        self.control_slot = Some(control_slot.clone());
        self.serial = Some(serial.clone());
        self.running = true;
        let state = self.state.clone();
        let record_queue = self.record_queue.clone();
        let handle = match std::thread::Builder::new()
            .name("playlink-decode".to_string())
            .spawn(move || {
                decode_thread_main(
                    video,
                    state,
                    stop,
                    record_queue,
                    child_slot,
                    control_slot,
                    adb_path,
                    adb_exe,
                    serial,
                    cfg,
                    first_tx,
                )
            }) {
            Ok(handle) => handle,
            Err(e) => {
                self.teardown();
                return Err(PlayLinkError::Other(e.into()));
            }
        };
        self.decode_handle = Some(handle);
        let timeout_secs = self
            .state
            .settings
            .lock()
            .map(|s| s.network_timeout_secs.clamp(3, 30))
            .unwrap_or(10);
        match first_rx.recv_timeout(Duration::from_secs(timeout_secs)) {
            Ok(Ok(())) => {
                tracing::info!("mirroring started for {}", device.serial);
                Ok(())
            }
            Ok(Err(e)) => {
                self.teardown();
                Err(e)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.teardown();
                Err(PlayLinkError::StreamEnded(
                    "Timed out waiting for the first video frame".to_string(),
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.teardown();
                Err(PlayLinkError::StreamEnded(
                    "Decoder thread ended before the first frame".to_string(),
                ))
            }
        }
    }

    /// Stop the session, kill the server, remove the forward.
    pub fn stop(&mut self) {
        if !self.running && self.decode_handle.is_none() {
            return;
        }
        tracing::info!("stopping mirroring");
        self.teardown();
    }

    pub fn is_running(&self) -> bool {
        self.running && self.state.is_mirroring()
    }

    /// Current stats snapshot for the UI.
    pub fn stats(&self) -> MirrorStats {
        self.state
            .stats
            .lock()
            .map(|stats| stats.clone())
            .unwrap_or_default()
    }

    /// Raw H.264 bytes of the current stream, for the recording engine.
    /// Returns `None` when recording is not armed.
    ///
    /// Each packet is stored as its 12-byte scrcpy header (PTS u64 BE,
    /// size u32 BE) followed by the payload, concatenated in stream
    /// order, so the blob parses with [`parse_video_packet_header`].
    pub fn take_recorded_chunk(&mut self) -> Option<Vec<u8>> {
        let recording = self
            .state
            .recording
            .lock()
            .map(|r| *r)
            .unwrap_or(RecordingState::Idle);
        if recording == RecordingState::Idle {
            return None;
        }
        self.record_queue.lock().ok().map(|mut queue| queue.drain())
    }

    /// Hand the control socket to the input controller. Returns `None`
    /// when no session is running (the socket is then already gone).
    /// Screen dimensions come from the video header at session start.
    pub fn input_controller(&mut self) -> Option<crate::input::InputController> {
        if !self.is_running() {
            return None;
        }
        let slot = self.control_slot.as_ref()?;
        let (stream, width, height) = slot.lock().ok()?.take()?;
        Some(crate::input::InputController::new(stream, width, height))
    }

    /// Signal the decode thread to exit and clean up the whole session.
    /// Safe to call when nothing is running.
    fn teardown(&mut self) {
        if let Some(flag) = &self.stop_flag {
            flag.store(true, Ordering::Relaxed);
        }
        if let Some(slot) = &self.child_slot {
            kill_slot_child(slot);
        }
        if let Some(slot) = &self.control_slot {
            if let Ok(mut guard) = slot.lock() {
                *guard = None;
            }
        }
        if let Some(serial) = &self.serial {
            let _ = self.adb.remove_forward(serial, SCRCPY_PORT);
        }
        if let Some(handle) = self.decode_handle.take() {
            let _ = handle.join();
        }
        self.running = false;
        self.stop_flag = None;
        self.child_slot = None;
        self.control_slot = None;
        self.serial = None;
        set_mirroring_false(&self.state);
    }
}

impl Drop for MirrorEngine {
    /// Dropping the engine stops mirroring instead of leaking a server
    /// process on the device.
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Decode thread entry point: decode until asked to stop, retry once on a
/// dropped stream when auto-reconnect is on, then shut the session down.
#[allow(clippy::too_many_arguments)]
fn decode_thread_main(
    mut video: TcpStream,
    state: Arc<SharedState>,
    stop: Arc<AtomicBool>,
    record_queue: Arc<Mutex<RecordQueue>>,
    child_slot: Arc<Mutex<Option<Child>>>,
    control_slot: Arc<Mutex<Option<(TcpStream, u16, u16)>>>,
    adb_path: String,
    adb_exe: PathBuf,
    serial: String,
    cfg: MirrorConfig,
    first_tx: mpsc::Sender<Result<()>>,
) {
    let mut decoder = match Decoder::new() {
        Ok(decoder) => decoder,
        Err(e) => {
            let err =
                PlayLinkError::DecodeFailed(format!("Could not create the H.264 decoder: {}", e));
            tracing::error!("{}", err);
            let _ = first_tx.send(Err(err));
            state.set_status("Could not create the video decoder.");
            set_mirroring_false(&state);
            return;
        }
    };
    let mut counters = StreamCounters::default();
    let mut first_sent = false;
    let mut retries = 0u32;
    loop {
        let session_start = Instant::now();
        match decode_stream(
            &mut video,
            &mut decoder,
            &state,
            &stop,
            &record_queue,
            &mut counters,
            &first_tx,
            &mut first_sent,
            session_start,
        ) {
            Ok(()) => break,
            Err(reason) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let auto_reconnect = state
                    .settings
                    .lock()
                    .map(|s| s.auto_reconnect)
                    .unwrap_or(false);
                if auto_reconnect && retries == 0 {
                    retries += 1;
                    tracing::warn!("video stream ended, retrying once: {}", reason);
                    state.set_status("Connection to the phone lost, retrying once...");
                    kill_slot_child(&child_slot);
                    if let Ok(adb) = Adb::locate(&adb_path) {
                        let _ = adb.remove_forward(&serial, SCRCPY_PORT);
                    }
                    std::thread::sleep(Duration::from_secs(1));
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let retry = Adb::locate(&adb_path).and_then(|adb| {
                        establish_session(&adb, &adb_exe, &serial, cfg, &stop, &child_slot)
                    });
                    match retry {
                        Ok(new_session) => {
                            tracing::info!(
                                "reconnected to the video stream ({}x{})",
                                new_session.width,
                                new_session.height
                            );
                            if let Ok(mut slot) = control_slot.lock() {
                                *slot = Some((
                                    new_session.control,
                                    new_session.width.min(u16::MAX as u32) as u16,
                                    new_session.height.min(u16::MAX as u32) as u16,
                                ));
                            }
                            video = new_session.video;
                            match Decoder::new() {
                                Ok(fresh) => decoder = fresh,
                                Err(e) => {
                                    let err = PlayLinkError::DecodeFailed(format!(
                                        "Could not recreate the H.264 decoder after reconnect: {}",
                                        e
                                    ));
                                    if !first_sent {
                                        let _ = first_tx.send(Err(err));
                                    }
                                    state.set_status("Video decoding failed after reconnect.");
                                    break;
                                }
                            }
                            continue;
                        }
                        Err(e) => {
                            let msg = format!(
                                "Video stream ended ({}), and the reconnect failed: {}",
                                reason, e
                            );
                            tracing::error!("{}", msg);
                            if !first_sent {
                                let _ = first_tx.send(Err(PlayLinkError::StreamEnded(msg.clone())));
                            }
                            state.set_status(msg);
                            break;
                        }
                    }
                }
                let msg = format!("Video stream ended: {}", reason);
                tracing::warn!("{}", msg);
                if !first_sent {
                    let _ = first_tx.send(Err(PlayLinkError::StreamEnded(msg.clone())));
                }
                state.set_status(msg);
                break;
            }
        }
    }
    kill_slot_child(&child_slot);
    if let Ok(adb) = Adb::locate(&adb_path) {
        let _ = adb.remove_forward(&serial, SCRCPY_PORT);
    }
    if let Ok(mut slot) = control_slot.lock() {
        *slot = None;
    }
    set_mirroring_false(&state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_header_normal_packet() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&1_234_567u64.to_be_bytes());
        buf.extend_from_slice(&42u32.to_be_bytes());
        buf.extend_from_slice(&[0u8; 42]);
        assert_eq!(
            parse_video_packet_header(&buf),
            Some((1_234_567, false, 42))
        );
    }

    #[test]
    fn packet_header_config_packet() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&u64::MAX.to_be_bytes());
        buf.extend_from_slice(&30u32.to_be_bytes());
        assert_eq!(parse_video_packet_header(&buf), Some((u64::MAX, true, 30)));
    }

    #[test]
    fn packet_header_short_buffer() {
        assert_eq!(parse_video_packet_header(&[]), None);
        assert_eq!(parse_video_packet_header(&[0u8; 11]), None);
    }

    #[test]
    fn record_queue_drops_oldest_past_cap() {
        let mut queue = RecordQueue::new();
        let packet = vec![0u8; 1024 * 1024];
        for _ in 0..(MAX_RECORDED_BYTES / (1024 * 1024) + 10) {
            queue.push(packet.clone());
        }
        assert!(queue.bytes <= MAX_RECORDED_BYTES);
        assert!(!queue.packets.is_empty());
        let before = queue.bytes;
        let drained = queue.drain();
        assert_eq!(drained.len(), before);
        assert!(queue.packets.is_empty());
        assert_eq!(queue.bytes, 0);
    }
}
