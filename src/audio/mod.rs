//! Audio engine (PRD section 6).
//!
//! Phase 1: receive the scrcpy audio stream (Opus packets) on its own
//! connection, expose volume / delay / monitoring settings and audio status.
//! Decoding to a desktop output device is Phase 2 work behind the
//! [`AudioDecoder`] trait: wire a real Opus decoder (for example via the
//! `audiopus` or `symphonia` crates) and WASAPI playback here. Until then
//! the engine reports audio as unavailable and the UI shows the PRD section
//! 14 guidance instead of claiming audio works.

use crate::core::Result;

/// Audio pipeline status shown in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioStatus {
    Disabled,
    Unavailable,
    Streaming,
    Error,
}

/// Decodes Opus packets into PCM. Implement for the real decoder in Phase 2.
pub trait AudioDecoder: Send {
    fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>>;
}

/// Receives the audio stream and (Phase 2) plays it back.
pub struct AudioEngine {
    status: AudioStatus,
    /// 0.0 (silent) to 1.0 (full volume). Stored in Phase 1; applied to
    /// the output stream once WASAPI playback is wired in Phase 2.
    volume: f32,
    /// Output delay in milliseconds. Stored in Phase 1; applied to the
    /// playback buffer once WASAPI playback is wired in Phase 2.
    delay_ms: i32,
}

impl AudioEngine {
    pub fn new() -> Self {
        Self {
            status: AudioStatus::Disabled,
            volume: 1.0,
            delay_ms: 0,
        }
    }

    pub fn status(&self) -> AudioStatus {
        self.status
    }

    /// Start receiving audio for the given session. No-op (status
    /// `Unavailable`) until a decoder is wired in.
    ///
    /// Phase 2 wiring (exact step): add `audiopus = "0.2"` to Cargo.toml,
    /// implement [`AudioDecoder`] for a new `OpusDecoder` struct by calling
    /// `audiopus::Decoder::new(48_000, audiopus::Channels::Stereo)` once
    /// and `decoder.decode(Some(packet), &mut pcm, false)` per Opus packet,
    /// then in this `start()` open the scrcpy audio connection, spawn a
    /// thread that reads Opus packets, decodes them with that
    /// [`AudioDecoder`], and writes the PCM to the default output device
    /// through WASAPI (`windows` crate, `IAudioClient::Initialize` plus
    /// `IAudioRenderClient`), and only then set
    /// `self.status = AudioStatus::Streaming` once the first PCM buffer
    /// is queued for playback.
    pub fn start(&mut self) -> Result<()> {
        // No decoder is wired in Phase 1, so there is nothing to receive
        // or play back. Report Unavailable honestly instead of pretending
        // audio is streaming; the UI shows the PRD section 14 guidance.
        self.status = AudioStatus::Unavailable;
        Ok(())
    }

    pub fn stop(&mut self) {
        self.status = AudioStatus::Disabled;
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    pub fn set_delay_ms(&mut self, delay_ms: i32) {
        self.delay_ms = delay_ms;
    }
}
