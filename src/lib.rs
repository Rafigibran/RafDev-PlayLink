//! RafDev PlayLink: Android screen mirroring, device control, and
//! live-streaming output for Windows.
//!
//! Phase 1 MVP (PRD v1.0): Rust + Slint UI, ADB device manager, scrcpy
//! server-backed mirroring, mouse/keyboard control, Clean Feed for OBS,
//! screenshot, recording, settings, diagnostics.

pub mod app;
pub mod audio;
pub mod core;
pub mod device;
pub mod diagnostics;
pub mod input;
pub mod mirror;
pub mod recording;
pub mod streaming;
