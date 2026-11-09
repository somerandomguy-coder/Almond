//! Jobs: replicated work requests so that any capable device can run a processor
//! (e.g. the phone requests `summarize.llm`, the PC claims it on its next sync).

use crate::ids::{DeviceId, Id};
use crate::time::{Hlc, UnixMs};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Capability string. Conventions:
/// `gpu`, `llm:<model>` (e.g. `llm:qwen3.5:9b`), `llm` (any), `stt:<lang>` (e.g. `stt:vi`),
/// `stt-large`, `diarize`, `embed:<model>`, `verify:<name>`. Open set.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Capability(pub String);

impl Capability {
    pub fn new(s: impl Into<String>) -> Self {
        Capability(s.into())
    }
    /// `self` (offered) satisfies `req` (required) if equal, or if `req` is a bare prefix
    /// like `llm` and `self` is `llm:<x>`.
    pub fn satisfies(&self, req: &Capability) -> bool {
        self.0 == req.0 || self.0.starts_with(&format!("{}:", req.0))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Job {
    pub id: Id,
    pub meeting_id: Id,
    /// `ProcessorDescriptor::name` to run.
    pub processor: String,
    pub inputs: Vec<Id>,
    /// Free-form processor params (prompt template, output language, ...).
    pub params: serde_json::Value,
    pub requires: Vec<Capability>,
    pub requested_by: DeviceId,
    pub requested_at: Hlc,
    /// Higher runs first.
    pub priority: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct JobLease {
    pub job_id: Id,
    pub device: DeviceId,
    pub claimed_at: Hlc,
    /// Claim expires at this wall time if no `JobFinished` arrives; others may then reclaim.
    /// Conflicting concurrent claims resolve to the lowest (claimed_at, device).
    #[ts(type = "number")]
    pub expires_at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct JobOutcome {
    pub job_id: Id,
    pub device: DeviceId,
    pub status: JobStatus,
    /// Artifacts produced (they are also sent as `artifact.add` events).
    pub outputs: Vec<Id>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum JobStatus {
    Pending,
    Claimed,
    Done,
    Failed,
    Cancelled,
}
