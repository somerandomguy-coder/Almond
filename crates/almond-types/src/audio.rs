//! Audio capture contract (WS-A). Backends: cpal mic, WASAPI loopback, PipeWire monitor,
//! Android (JNI bridge), file import.

use crate::meeting::AudioChannel;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Canonical pipeline format: 16 kHz, mono, f32 in [-1, 1].
pub const PIPELINE_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AudioDeviceInfo {
    /// Backend-unique id, stable across restarts where possible.
    pub id: String,
    pub name: String,
    pub channel: AudioChannel,
    /// "cpal" | "wasapi_loopback" | "pipewire" | "pulse" | "android" | "file"
    pub backend: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CaptureConfig {
    pub device_id: Option<String>,
    /// Frame size delivered by `AudioStream::next` (ms of audio). Default 100.
    pub frame_ms: u32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            device_id: None,
            frame_ms: 100,
        }
    }
}

/// A chunk of resampled pipeline audio.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFrame {
    pub channel: AudioChannel,
    /// Position of the first sample relative to the start of the capture, in ms.
    pub t0_ms: u64,
    /// 16 kHz mono f32 samples.
    pub samples: Vec<f32>,
}

#[async_trait]
pub trait AudioBackend: Send + Sync {
    fn name(&self) -> &str;
    async fn list_devices(&self) -> crate::Result<Vec<AudioDeviceInfo>>;
    async fn open(
        &self,
        channel: AudioChannel,
        cfg: CaptureConfig,
    ) -> crate::Result<Box<dyn AudioStream>>;
}

#[async_trait]
pub trait AudioStream: Send {
    /// Next frame; `None` when the stream ended (device gone / stopped). During silence on a
    /// loopback device, backends must still emit zero-filled frames so channels stay aligned.
    async fn next(&mut self) -> Option<crate::Result<AudioFrame>>;
    async fn stop(&mut self) -> crate::Result<()>;
}
