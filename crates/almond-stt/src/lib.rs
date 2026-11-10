//! # almond-stt — speech-to-text, diarization, model management, WER/CER (WS-B)
//!
//! | Module | What |
//! |---|---|
//! | [`whisper`] (feature `whisper`, `cuda`, `vulkan`) | whisper.cpp engine: PhoWhisper, whisper-large-v3-turbo |
//! | [`sherpa`] (feature `sherpa`) | sherpa-onnx Zipformer-vi engine + pyannote/3D-Speaker diarizer |
//! | [`streaming`] | engine-agnostic pseudo-streaming (LocalAgreement-2) for live captions |
//! | [`router`] | per-window language ID + per-language engine routing; meeting pipeline |
//! | [`transcript`] | channel prior (mic = me), echo removal, speaker assignment, transcript body |
//! | [`models`] | model registry, resumable + SHA-256-verified downloads |
//! | [`routing`] | default model per language × device class |
//! | [`metrics`] | Vietnamese-aware normalization, WER / CER |
//! | [`audio`] | WAV I/O, resampling, energy VAD |
//!
//! The crate builds with no engine features (registry, metrics, streaming core, transcript
//! assembly are always available); engines are opt-in because they compile native code.

pub use almond_types as types;

pub mod audio;
pub mod metrics;
pub mod models;
pub mod router;
pub mod routing;
pub mod streaming;
pub mod transcript;

#[cfg(feature = "sherpa")]
pub mod sherpa;
#[cfg(feature = "whisper")]
pub mod whisper;

/// Version stamped into artifact provenance (`processor_version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
