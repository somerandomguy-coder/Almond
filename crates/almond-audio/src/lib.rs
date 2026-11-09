//! # almond-audio (WS-A) — audio capture
//!
//! Produces time-aligned 16 kHz mono f32 frames per channel ([`AudioChannel::Mic`] = me,
//! [`AudioChannel::System`] = the other side of an online call) and archives them as crash-safe
//! chunked Ogg Opus files handed to a [`BlobStore`](almond_types::store::BlobStore).
//!
//! * Sources: [`CpalBackend`] (mic everywhere; WASAPI loopback on Windows; PulseAudio/PipeWire
//!   default-sink monitor on Linux), [`PushAudioSource`] (Android JNI hook / any external PCM
//!   producer), [`open_wav`] (file import).
//! * All sources share one device-independent [`pipeline`]: capture-timestamp alignment to a
//!   shared [`CaptureClock`] (zero-fill gaps, trim drift), wall-clock silence fill for quiet
//!   loopback, resampling ([`StreamResampler`]) and framing.
//! * [`vad`]: energy VAD (+ WebRTC VAD behind the `webrtc-vad` feature).
//! * [`recorder`]: chunked Opus/WAV writer, merge + recovery after a crash.
//! * [`capture`]: runs several channels into a recorder and fans frames out to consumers (STT).
//!
//! [`AudioChannel::Mic`]: almond_types::meeting::AudioChannel::Mic
//! [`AudioChannel::System`]: almond_types::meeting::AudioChannel::System

pub use almond_types as types;

pub mod capture;
pub mod clock;
#[cfg(not(target_os = "android"))]
pub mod cpal_backend;
pub mod file;
pub mod level;
pub mod pipeline;
pub mod push;
pub mod recorder;
pub mod resample;
pub mod vad;

pub use capture::{CaptureSession, CaptureSummary};
pub use clock::CaptureClock;
#[cfg(not(target_os = "android"))]
pub use cpal_backend::CpalBackend;
pub use file::open_wav;
pub use pipeline::{AlignConfig, PipelineStream, RawSender, StatsSnapshot, StreamStats};
pub use push::PushAudioSource;
pub use recorder::{Codec, RecorderConfig, SessionRecorder, recover_session, unfinished_sessions};
pub use resample::StreamResampler;
