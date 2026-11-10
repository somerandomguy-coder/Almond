//! Speech-to-text + diarization contract (WS-B). Engines: whisper.cpp (whisper-rs), sherpa-onnx.

use crate::artifact::body::Segment;
use crate::audio::AudioFrame;
use crate::ids::SpeakerId;
use crate::lang::LangTag;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SttModelInfo {
    /// e.g. "whisper:phowhisper-large", "sherpa:zipformer-vi-int8"
    pub id: String,
    pub engine: String,
    pub languages: Vec<LangTag>,
    pub streaming: bool,
    #[ts(type = "number | null")]
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub installed: bool,
    #[serde(default)]
    pub role: ModelRole,
}

/// What a speech model is for (lets the UI list diarization models separately).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ModelRole {
    #[default]
    Stt,
    /// Diarization: speech/speaker-change segmentation.
    Segmentation,
    /// Diarization: speaker embedding.
    Embedding,
}

/// Notification `models.downloadProgress` while a model is being downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelDownloadProgress {
    pub id: String,
    #[ts(type = "number")]
    pub done_bytes: u64,
    #[ts(type = "number | null")]
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]
#[ts(export)]
pub struct SttOptions {
    /// None = auto-detect per segment.
    pub language: Option<LangTag>,
    /// Restrict auto-detection to these (e.g. ["vi","en"]).
    pub allowed_languages: Vec<LangTag>,
    /// Domain vocabulary / names to bias towards (Whisper initial prompt).
    pub prompt: Option<String>,
    pub word_timestamps: bool,
}

/// Partial (unstable) or final text from a streaming session.
///
/// Protocol: updates carry `segment.idx`. A partial (`is_final = false`) for idx N **replaces**
/// the previous partial for N. Exactly one final arrives per idx, and after it no further updates
/// for N arrive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SttUpdate {
    pub segment: Segment,
    /// `false` = may still change; UI shows it greyed.
    pub is_final: bool,
}

#[async_trait]
pub trait SttEngine: Send + Sync {
    fn model(&self) -> SttModelInfo;
    /// Offline transcription of a whole buffer (16 kHz mono f32). `t_offset_ms` is added to
    /// segment times; `channel` stamped on every segment.
    async fn transcribe(
        &self,
        samples: &[f32],
        t_offset_ms: u64,
        channel: crate::meeting::AudioChannel,
        opts: &SttOptions,
    ) -> crate::Result<Vec<Segment>>;
    /// Start a (pseudo-)streaming session for live captions.
    fn start_stream(&self, opts: SttOptions) -> crate::Result<Box<dyn SttStream>>;
}

#[async_trait]
pub trait SttStream: Send {
    async fn push(&mut self, frame: AudioFrame) -> crate::Result<Vec<SttUpdate>>;
    async fn finish(&mut self) -> crate::Result<Vec<SttUpdate>>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SpeakerTurn {
    pub t0_ms: u32,
    pub t1_ms: u32,
    pub speaker: SpeakerId,
}

#[async_trait]
pub trait Diarizer: Send + Sync {
    /// `num_speakers` hint (e.g. from calendar attendee count).
    async fn diarize(
        &self,
        samples: &[f32],
        num_speakers: Option<u32>,
    ) -> crate::Result<Vec<SpeakerTurn>>;
}
