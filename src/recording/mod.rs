//! Screenshot and screen recording (PRD section 7).
//!
//! Screenshot: encode the latest decoded frame as PNG (or JPG) with a
//! timestamped filename (`PlayLink_Screenshot_2026-11-09_10-30-00.png`).
//!
//! Recording: the mirror engine hands raw H.264 access units to the
//! recorder, which muxes them into MP4 (H.264 video, AAC when audio is
//! included in Phase 2). If muxing fails, the raw stream is preserved as
//! `.h264` and the error is surfaced without stopping mirroring.

use crate::core::{PlayLinkError, RecordingState, Result, SharedState};
use chrono::Local;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Timestamped filename for a screenshot, e.g.
/// `PlayLink_Screenshot_2026-11-09_10-30-00.png`.
pub fn screenshot_filename(extension: &str) -> String {
    let ext = extension.trim_start_matches('.');
    let ext = if ext.is_empty() { "png" } else { ext };
    format!(
        "PlayLink_Screenshot_{}.{}",
        Local::now().format("%Y-%m-%d_%H-%M-%S"),
        ext
    )
}

/// Save the latest frame from shared state as a PNG in `folder`.
/// An empty `folder` falls back to the screenshot folder from settings.
pub fn save_screenshot(state: &SharedState, folder: &str) -> Result<PathBuf> {
    let frame = state
        .frame
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().cloned())
        .ok_or_else(|| PlayLinkError::ScreenshotFailed("No frame available yet".to_string()))?;

    let folder = if folder.trim().is_empty() {
        state
            .settings
            .lock()
            .map(|s| s.screenshot_folder.clone())
            .unwrap_or_else(|_| "PlayLink".to_string())
    } else {
        folder.to_string()
    };
    std::fs::create_dir_all(&folder)?;
    let path = PathBuf::from(&folder).join(screenshot_filename("png"));

    let image =
        image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba).ok_or_else(|| {
            PlayLinkError::ScreenshotFailed("Frame buffer has an unexpected size".to_string())
        })?;
    image::DynamicImage::ImageRgba8(image)
        .save_with_format(&path, image::ImageFormat::Png)
        .map_err(|e| {
            PlayLinkError::ScreenshotFailed(format!("Could not write {}: {}", path.display(), e))
        })?;
    Ok(path)
}

/// Owns one recording session.
pub struct RecordingEngine {
    state: Arc<SharedState>,
    recording: RecordingState,
    output_path: Option<PathBuf>,
    started_at: Option<Instant>,
    /// Recorded access units: (presentation timestamp in microseconds,
    /// Annex B bytes). Paused time is subtracted from the timestamps in
    /// [`RecordingEngine::feed_video`] so the muxed timeline stays
    /// continuous across pauses.
    packets: Vec<(u64, Vec<u8>)>,
    /// Instant the current pause started, if paused.
    paused_at: Option<Instant>,
    /// Total paused time in microseconds, excluding the current pause.
    paused_us: u64,
}

impl RecordingEngine {
    pub fn new(state: Arc<SharedState>) -> Self {
        Self {
            state,
            recording: RecordingState::Idle,
            output_path: None,
            started_at: None,
            packets: Vec::new(),
            paused_at: None,
            paused_us: 0,
        }
    }

    /// Start recording to `folder` with timestamped MP4 naming. An empty
    /// `folder` falls back to the recording folder from settings.
    pub fn start(&mut self, folder: &str) -> Result<PathBuf> {
        if self.recording != RecordingState::Idle {
            return Err(PlayLinkError::RecordingFailed(
                "Recording is already in progress".to_string(),
            ));
        }
        let folder = if folder.trim().is_empty() {
            self.state
                .settings
                .lock()
                .map(|s| s.recording_folder.clone())
                .unwrap_or_else(|_| "PlayLink".to_string())
        } else {
            folder.to_string()
        };
        std::fs::create_dir_all(&folder)?;
        let path = PathBuf::from(&folder).join(format!(
            "PlayLink_Recording_{}.mp4",
            Local::now().format("%Y-%m-%d_%H-%M-%S")
        ));
        self.output_path = Some(path.clone());
        self.started_at = Some(Instant::now());
        self.packets.clear();
        self.paused_at = None;
        self.paused_us = 0;
        self.recording = RecordingState::Recording;
        self.set_shared_state(RecordingState::Recording, 0);
        Ok(path)
    }

    pub fn pause(&mut self) {
        if self.recording == RecordingState::Recording {
            self.recording = RecordingState::Paused;
            self.paused_at = Some(Instant::now());
            self.set_shared_state(RecordingState::Paused, self.elapsed_secs());
        }
    }

    pub fn resume(&mut self) {
        if self.recording == RecordingState::Paused {
            if let Some(paused_at) = self.paused_at.take() {
                self.paused_us = self
                    .paused_us
                    .saturating_add(paused_at.elapsed().as_micros() as u64);
            }
            self.recording = RecordingState::Recording;
            self.set_shared_state(RecordingState::Recording, self.elapsed_secs());
        }
    }

    /// Stop and finalize the file. Returns the finished path.
    /// If MP4 muxing fails (for example no SPS/PPS in the stream), the raw
    /// Annex B stream is preserved as `.h264` next to the target path and
    /// that path is returned instead.
    pub fn stop(&mut self) -> Result<PathBuf> {
        let Some(path) = self.output_path.take() else {
            return Err(PlayLinkError::RecordingFailed(
                "No recording in progress".to_string(),
            ));
        };
        let packets = std::mem::take(&mut self.packets);
        self.recording = RecordingState::Idle;
        self.started_at = None;
        self.paused_at = None;
        self.paused_us = 0;
        self.set_shared_state(RecordingState::Idle, 0);

        if packets.is_empty() {
            return Err(PlayLinkError::RecordingFailed(
                "No video frames were captured".to_string(),
            ));
        }
        // Frame size for the sample entry: latest decoded frame, falling
        // back to the portrait canvas when no frame arrived yet.
        let (width, height) = self
            .state
            .frame
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|f| (f.width, f.height)))
            .unwrap_or((1080, 1920));
        match mux_h264_to_mp4(&packets, width, height) {
            Ok(bytes) => {
                std::fs::write(&path, bytes)?;
                Ok(path)
            }
            Err(e) => {
                let raw_path = path.with_extension("h264");
                let mut raw = Vec::new();
                for (_, data) in &packets {
                    raw.extend_from_slice(data);
                }
                std::fs::write(&raw_path, raw)?;
                self.state.set_status(format!(
                    "Recording saved as raw H.264 (MP4 muxing failed: {})",
                    e
                ));
                Ok(raw_path)
            }
        }
    }

    /// Feed one H.264 access unit (with its presentation timestamp) to the
    /// muxer. Called from the mirror engine thread. Packets arriving while
    /// paused or idle are dropped.
    pub fn feed_video(&mut self, data: &[u8], pts_us: u64) {
        if self.recording != RecordingState::Recording {
            return;
        }
        self.packets
            .push((pts_us.saturating_sub(self.paused_us), data.to_vec()));
    }

    pub fn state(&self) -> RecordingState {
        self.recording
    }

    /// Recorded seconds, excluding paused time.
    pub fn elapsed_secs(&self) -> u64 {
        let Some(started_at) = self.started_at else {
            return 0;
        };
        let mut elapsed = started_at.elapsed();
        elapsed = elapsed.saturating_sub(Duration::from_micros(self.paused_us));
        if let Some(paused_at) = self.paused_at {
            elapsed = elapsed.saturating_sub(paused_at.elapsed());
        }
        let secs = elapsed.as_secs();
        if let Ok(mut slot) = self.state.recording_elapsed_secs.lock() {
            *slot = secs;
        }
        secs
    }

    fn set_shared_state(&self, recording: RecordingState, elapsed_secs: u64) {
        if let Ok(mut slot) = self.state.recording.lock() {
            *slot = recording;
        }
        if let Ok(mut slot) = self.state.recording_elapsed_secs.lock() {
            *slot = elapsed_secs;
        }
    }
}

/// Minimal MP4 muxer for a raw H.264 stream (avcC from SPS/PPS, sample
/// tables built incrementally). Pure function over recorded packets;
/// unit-tested against a known-good stream.
///
/// Layout is `ftyp`, `moov`, `mdat` with a single chunk and a 1 MHz
/// timescale, so packet timestamps in microseconds map directly to media
/// ticks. Returns an error when no SPS/PPS is found, so the caller can
/// fall back to writing the raw `.h264` stream.
pub fn mux_h264_to_mp4(packets: &[(u64, Vec<u8>)], width: u32, height: u32) -> Result<Vec<u8>> {
    // 1. Harvest SPS/PPS and build length-prefixed (AVCC) samples.
    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;
    // (pts_us, AVCC sample bytes, is IDR)
    let mut samples: Vec<(u64, Vec<u8>, bool)> = Vec::new();
    for (pts_us, data) in packets {
        let nals = split_annexb(data);
        let mut is_vcl = false;
        let mut is_idr = false;
        let mut avcc: Vec<u8> = Vec::new();
        for nal in &nals {
            if nal.is_empty() {
                continue;
            }
            match nal[0] & 0x1f {
                7 => {
                    if sps.is_none() {
                        sps = Some(nal.to_vec());
                    }
                }
                8 => {
                    if pps.is_none() {
                        pps = Some(nal.to_vec());
                    }
                }
                _ => {}
            }
            match nal[0] & 0x1f {
                1..=5 => {
                    is_vcl = true;
                    if nal[0] & 0x1f == 5 {
                        is_idr = true;
                    }
                }
                _ => {}
            }
            avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            avcc.extend_from_slice(nal);
        }
        if is_vcl && !avcc.is_empty() {
            samples.push((*pts_us, avcc, is_idr));
        }
    }
    let (Some(sps), Some(pps)) = (sps, pps) else {
        return Err(PlayLinkError::RecordingFailed(
            "No SPS/PPS found in the H.264 stream, cannot build an MP4 sample entry".to_string(),
        ));
    };
    if sps.len() < 4 || pps.is_empty() {
        return Err(PlayLinkError::RecordingFailed(
            "Malformed SPS/PPS in the H.264 stream".to_string(),
        ));
    }
    if samples.is_empty() {
        return Err(PlayLinkError::RecordingFailed(
            "No video samples in the H.264 stream".to_string(),
        ));
    }

    // 2. Sample timing: deltas between successive presentation timestamps.
    const DEFAULT_FRAME_DELTA_US: u64 = 1_000_000 / 30;
    let first_pts = samples[0].0;
    let mut deltas: Vec<u64> = samples
        .windows(2)
        .map(|w| w[1].0.saturating_sub(w[0].0).max(1))
        .collect();
    let last_delta = deltas.last().copied().unwrap_or(DEFAULT_FRAME_DELTA_US);
    deltas.push(last_delta);
    let duration_us = samples
        .last()
        .map(|s| s.0)
        .unwrap_or(0)
        .saturating_sub(first_pts)
        .saturating_add(last_delta);
    let duration = duration_us.min(u32::MAX as u64) as u32;
    // Group consecutive equal deltas into stts runs.
    let mut stts_runs: Vec<(u32, u32)> = Vec::new();
    for d in &deltas {
        let d = (*d).min(u32::MAX as u64) as u32;
        if let Some(last) = stts_runs.last_mut() {
            if last.1 == d {
                last.0 += 1;
                continue;
            }
        }
        stts_runs.push((1, d));
    }
    let sync_samples: Vec<u32> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.2)
        .map(|(i, _)| i as u32 + 1)
        .collect();

    // 3. Sample tables.
    let stsd = {
        let mut avcc = Vec::new();
        avcc.push(1); // configurationVersion
        avcc.push(sps[1]); // AVCProfileIndication
        avcc.push(sps[2]); // profile_compatibility
        avcc.push(sps[3]); // AVCLevelIndication
        avcc.push(0xFF); // lengthSizeMinusOne = 3 (4-byte NAL lengths)
        avcc.push(0xE1); // numOfSequenceParameterSets = 1
        avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&sps);
        avcc.push(1); // numOfPictureParameterSets = 1
        avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&pps);

        let mut avc1 = Vec::new();
        avc1.extend_from_slice(&[0u8; 6]); // reserved
        avc1.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        avc1.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
        avc1.extend_from_slice(&0u16.to_be_bytes()); // reserved
        avc1.extend_from_slice(&[0u8; 12]); // pre_defined[3]
        avc1.extend_from_slice(&(width as u16).to_be_bytes());
        avc1.extend_from_slice(&(height as u16).to_be_bytes());
        avc1.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // horizresolution 72 dpi
        avc1.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // vertresolution 72 dpi
        avc1.extend_from_slice(&0u32.to_be_bytes()); // reserved
        avc1.extend_from_slice(&1u16.to_be_bytes()); // frame_count
        avc1.extend_from_slice(&[0u8; 32]); // compressorname
        avc1.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
        avc1.extend_from_slice(&0xFFFFu16.to_be_bytes()); // pre_defined (-1)
        avc1.extend_from_slice(&bx(b"avcC", &avcc));

        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // version/flags
        body.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        body.extend_from_slice(&bx(b"avc1", &avc1));
        bx(b"stsd", &body)
    };
    let stts = {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // version/flags
        body.extend_from_slice(&(stts_runs.len() as u32).to_be_bytes());
        for (count, delta) in &stts_runs {
            body.extend_from_slice(&count.to_be_bytes());
            body.extend_from_slice(&delta.to_be_bytes());
        }
        bx(b"stts", &body)
    };
    let stss = if sync_samples.is_empty() {
        None
    } else {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // version/flags
        body.extend_from_slice(&(sync_samples.len() as u32).to_be_bytes());
        for n in &sync_samples {
            body.extend_from_slice(&n.to_be_bytes());
        }
        Some(bx(b"stss", &body))
    };
    let stsc = {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // version/flags
        body.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        body.extend_from_slice(&1u32.to_be_bytes()); // first_chunk
        body.extend_from_slice(&(samples.len() as u32).to_be_bytes()); // samples_per_chunk
        body.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index
        bx(b"stsc", &body)
    };
    let stsz = {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // version/flags
        body.extend_from_slice(&0u32.to_be_bytes()); // sample_size (0 = varied)
        body.extend_from_slice(&(samples.len() as u32).to_be_bytes());
        for (_, data, _) in &samples {
            body.extend_from_slice(&(data.len() as u32).to_be_bytes());
        }
        bx(b"stsz", &body)
    };

    // 4. Movie structure (chunk offset depends on the moov size, so build
    // moov once with a placeholder offset, measure it, then rebuild).
    let build_moov = |chunk_offset: u32| -> Vec<u8> {
        let mut stco_body = Vec::new();
        stco_body.extend_from_slice(&0u32.to_be_bytes()); // version/flags
        stco_body.extend_from_slice(&1u32.to_be_bytes()); // entry_count
        stco_body.extend_from_slice(&chunk_offset.to_be_bytes());
        let stco = bx(b"stco", &stco_body);

        let mut stbl_children = vec![stsd.clone(), stts.clone()];
        if let Some(stss) = &stss {
            stbl_children.push(stss.clone());
        }
        stbl_children.push(stsc.clone());
        stbl_children.push(stsz.clone());
        stbl_children.push(stco);
        let stbl = bx_children(b"stbl", &stbl_children);
        let minf = bx_children(b"minf", &[vmhd(), smhd(), dinf(), stbl]);
        let mdia = bx_children(b"mdia", &[mdhd(TIMESCALE, duration), hdlr(), minf]);
        let trak = bx_children(b"trak", &[tkhd(duration, width, height), mdia]);
        bx_children(b"moov", &[mvhd(TIMESCALE, duration), trak])
    };

    let mut ftyp_body = Vec::new();
    ftyp_body.extend_from_slice(b"isom"); // major_brand
    ftyp_body.extend_from_slice(&512u32.to_be_bytes()); // minor_version
    for brand in [b"isom", b"iso2", b"avc1", b"mp41"] {
        ftyp_body.extend_from_slice(brand);
    }
    let ftyp = bx(b"ftyp", &ftyp_body);

    let moov_probe = build_moov(0);
    let chunk_offset = (ftyp.len() + moov_probe.len() + 8) as u32;
    let moov = build_moov(chunk_offset);
    debug_assert_eq!(moov.len(), moov_probe.len());

    let mut mdat_payload = Vec::new();
    for (_, data, _) in &samples {
        mdat_payload.extend_from_slice(data);
    }
    let mdat = bx(b"mdat", &mdat_payload);

    let mut out = Vec::with_capacity(ftyp.len() + moov.len() + mdat.len());
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov);
    out.extend_from_slice(&mdat);
    Ok(out)
}

/// 1 MHz media timescale: packet timestamps in microseconds map directly.
const TIMESCALE: u32 = 1_000_000;

/// One MP4 box: 4-byte big-endian size, 4-byte type, payload.
fn bx(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(payload);
    out
}

/// One MP4 container box holding child boxes.
fn bx_children(tag: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
    let mut payload = Vec::new();
    for child in children {
        payload.extend_from_slice(child);
    }
    bx(tag, &payload)
}

/// Unity transform matrix (ISO/IEC 14496-12).
fn unity_matrix(out: &mut Vec<u8>) {
    for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        out.extend_from_slice(&v.to_be_bytes());
    }
}

fn mvhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes()); // version/flags
    body.extend_from_slice(&0u32.to_be_bytes()); // creation_time
    body.extend_from_slice(&0u32.to_be_bytes()); // modification_time
    body.extend_from_slice(&timescale.to_be_bytes());
    body.extend_from_slice(&duration.to_be_bytes());
    body.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    body.extend_from_slice(&0x0100u16.to_be_bytes()); // volume
    body.extend_from_slice(&0u16.to_be_bytes()); // reserved
    body.extend_from_slice(&[0u8; 8]); // reserved
    unity_matrix(&mut body);
    body.extend_from_slice(&[0u8; 24]); // pre_defined
    body.extend_from_slice(&2u32.to_be_bytes()); // next_track_ID
    bx(b"mvhd", &body)
}

fn tkhd(duration: u32, width: u32, height: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0x0000_0007u32.to_be_bytes()); // version 0, flags enabled|in_movie|in_preview
    body.extend_from_slice(&0u32.to_be_bytes()); // creation_time
    body.extend_from_slice(&0u32.to_be_bytes()); // modification_time
    body.extend_from_slice(&1u32.to_be_bytes()); // track_ID
    body.extend_from_slice(&0u32.to_be_bytes()); // reserved
    body.extend_from_slice(&duration.to_be_bytes());
    body.extend_from_slice(&[0u8; 8]); // reserved
    body.extend_from_slice(&0u16.to_be_bytes()); // layer
    body.extend_from_slice(&0u16.to_be_bytes()); // alternate_group
    body.extend_from_slice(&0u16.to_be_bytes()); // volume
    body.extend_from_slice(&0u16.to_be_bytes()); // reserved
    unity_matrix(&mut body);
    body.extend_from_slice(&(width << 16).to_be_bytes()); // fixed-point width
    body.extend_from_slice(&(height << 16).to_be_bytes()); // fixed-point height
    bx(b"tkhd", &body)
}

fn mdhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes()); // version/flags
    body.extend_from_slice(&0u32.to_be_bytes()); // creation_time
    body.extend_from_slice(&0u32.to_be_bytes()); // modification_time
    body.extend_from_slice(&timescale.to_be_bytes());
    body.extend_from_slice(&duration.to_be_bytes());
    body.extend_from_slice(&0x55C4u16.to_be_bytes()); // language "und"
    body.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    bx(b"mdhd", &body)
}

fn hdlr() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes()); // version/flags
    body.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    body.extend_from_slice(b"vide"); // handler_type
    body.extend_from_slice(&[0u8; 12]); // reserved
    body.extend_from_slice(b"VideoHandler\0");
    bx(b"hdlr", &body)
}

fn vmhd() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // version 0, flags 1
    body.extend_from_slice(&0u16.to_be_bytes()); // graphicsmode
    body.extend_from_slice(&[0u8; 6]); // opcolor
    bx(b"vmhd", &body)
}

fn smhd() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_be_bytes()); // version/flags
    body.extend_from_slice(&0u16.to_be_bytes()); // balance
    body.extend_from_slice(&0u16.to_be_bytes()); // reserved
    bx(b"smhd", &body)
}

fn dinf() -> Vec<u8> {
    let mut dref_body = Vec::new();
    dref_body.extend_from_slice(&0u32.to_be_bytes()); // version/flags
    dref_body.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    let mut url_body = Vec::new();
    url_body.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // version 0, flags 1 (self-contained)
    dref_body.extend_from_slice(&bx(b"url ", &url_body));
    bx_children(b"dinf", &[bx(b"dref", &dref_body)])
}

/// Split an Annex B access unit into NAL units (start codes stripped).
/// Handles both 3-byte (`00 00 01`) and 4-byte (`00 00 00 01`) start codes.
fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut nals: Vec<&[u8]> = Vec::new();
    let mut i = 0;
    let mut nal_start: Option<usize> = None;
    while i + 2 < data.len() {
        let is_three = data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1;
        let is_four = i + 3 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1;
        if is_three || is_four {
            let code_len = if is_four { 4 } else { 3 };
            if let Some(start) = nal_start {
                if i > start {
                    nals.push(&data[start..i]);
                }
            }
            nal_start = Some(i + code_len);
            i += code_len;
        } else {
            i += 1;
        }
    }
    if let Some(start) = nal_start {
        if start < data.len() {
            // Drop trailing zero bytes after the last NAL (alignment bytes,
            // not part of the RBSP).
            let mut end = data.len();
            while end > start && data[end - 1] == 0 {
                end -= 1;
            }
            if end > start {
                nals.push(&data[start..end]);
            }
        }
    }
    nals
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three synthetic access units: SPS + PPS + IDR, then two non-IDR
    /// slices. The NAL payloads are header-plausible but not decodable;
    /// the muxer only reads NAL headers and SPS/PPS bytes.
    fn synthetic_packets() -> Vec<(u64, Vec<u8>)> {
        let sps: &[u8] = &[
            0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0x84,
        ];
        let pps: &[u8] = &[0x68, 0xee, 0x3c, 0x80];
        let idr: &[u8] = &[
            0x65, 0x88, 0x84, 0x00, 0x10, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x0f, 0x16, 0x2e, 0x48,
        ];
        let slice: &[u8] = &[0x41, 0x9a, 0x24, 0x00, 0x10, 0x00, 0x00, 0x03, 0x00, 0x80];
        let au = |nals: &[&[u8]]| -> Vec<u8> {
            let mut out = Vec::new();
            for nal in nals {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(nal);
            }
            out
        };
        vec![
            (0, au(&[sps, pps, idr])),
            (33_333, au(&[slice])),
            (66_666, au(&[slice])),
        ]
    }

    /// Walk the top-level boxes, returning (type, size) pairs. Asserts the
    /// sizes are in range and cover the file exactly.
    fn top_level_boxes(data: &[u8]) -> Vec<([u8; 4], usize)> {
        let mut out = Vec::new();
        let mut off = 0;
        while off + 8 <= data.len() {
            let size = u32::from_be_bytes(data[off..off + 4].try_into().unwrap()) as usize;
            assert!(
                size >= 8 && off + size <= data.len(),
                "box size out of range"
            );
            let tag: [u8; 4] = data[off + 4..off + 8].try_into().unwrap();
            out.push((tag, size));
            off += size;
        }
        assert_eq!(off, data.len(), "no trailing bytes after the last box");
        out
    }

    #[test]
    fn mux_produces_ftyp_moov_mdat_with_consistent_sizes() {
        let mp4 = mux_h264_to_mp4(&synthetic_packets(), 1080, 1920).expect("muxing succeeds");
        assert_eq!(&mp4[4..8], b"ftyp", "output starts with an ftyp box");
        let boxes = top_level_boxes(&mp4);
        assert_eq!(&boxes[0].0, b"ftyp", "first box is ftyp");
        assert!(boxes.iter().any(|(t, _)| t == b"moov"), "moov present");
        assert!(boxes.iter().any(|(t, _)| t == b"mdat"), "mdat present");
        let total: usize = boxes.iter().map(|(_, s)| s).sum();
        assert_eq!(total, mp4.len(), "box sizes add up to the file size");
        // The avcC sample entry made it into the movie box.
        assert!(
            mp4.windows(4).any(|w| w == b"avcC"),
            "moov contains an avcC box"
        );
    }

    #[test]
    fn mux_fails_without_sps_pps() {
        let packets = vec![(0u64, vec![0, 0, 0, 1, 0x65, 0x88, 0x84])];
        assert!(mux_h264_to_mp4(&packets, 1080, 1920).is_err());
    }

    #[test]
    fn mux_fails_on_empty_input() {
        assert!(mux_h264_to_mp4(&[], 1080, 1920).is_err());
    }
}
