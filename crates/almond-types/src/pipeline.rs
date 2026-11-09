//! Processor contract (WS-C runs them, everyone writes them). A processor consumes artifacts
//! (and/or meeting audio) and produces new artifacts. External processors speak the same shape
//! over JSON-RPC (`processor.describe`, `processor.run`).

use crate::artifact::{Artifact, ArtifactDraft};
use crate::ids::Id;
use crate::job::Capability;
use crate::meeting::Meeting;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProcessorDescriptor {
    /// Dotted name, e.g. "transcribe.whisper", "summarize.llm", "verify.numbers".
    pub name: String,
    pub version: String,
    /// Artifact kinds required as input (empty + `needs_audio` for transcription).
    pub consumes: Vec<String>,
    pub produces: Vec<String>,
    pub needs_audio: bool,
    pub requires: Vec<Capability>,
    pub description: String,
}

/// Progress callback: fraction in [0,1] and a short message.
pub type ProgressFn = Arc<dyn Fn(f32, &str) + Send + Sync>;

/// Runtime context handed to a processor.
#[derive(Clone)]
pub struct JobCtx {
    pub job_id: Id,
    pub meeting: Meeting,
    pub params: serde_json::Value,
    pub cancel: Arc<AtomicBool>,
    /// Progress in [0,1] with a short message.
    pub progress: ProgressFn,
    /// Access to blobs (audio) on this device.
    pub blobs: Arc<dyn crate::store::BlobStore>,
}

impl JobCtx {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn check_cancel(&self) -> crate::Result<()> {
        if self.is_cancelled() {
            Err(crate::Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[async_trait]
pub trait Processor: Send + Sync {
    fn descriptor(&self) -> ProcessorDescriptor;
    async fn run(&self, ctx: &JobCtx, inputs: Vec<Artifact>) -> crate::Result<Vec<ArtifactDraft>>;
}
