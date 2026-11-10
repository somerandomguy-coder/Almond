//! Verification contract (WS-H). Verifiers check claims against evidence and produce verdicts.
//! They are wrapped as `Processor`s (`claims` -> `verdicts`) by almond-verify.
//! Neuro-symbolic verifiers use `Claim::logical_form` and put derivations in `Verdict::trace`.

use crate::artifact::Artifact;
use crate::artifact::body::{Claim, Transcript, Verdict};
use crate::calendar::CalendarEvent;
use crate::meeting::Meeting;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VerifierDescriptor {
    /// e.g. "verify.citation_exists", "verify.numbers", "verify.calendar", "verify.nli"
    pub name: String,
    pub version: String,
    /// "symbolic" | "neural" | "hybrid"
    pub family: String,
    /// `logical_form_kind`s this verifier understands (empty = natural language only).
    pub logical_forms: Vec<String>,
    pub description: String,
}

/// Everything a verifier may look at. Optional pieces may be absent.
#[derive(Debug, Clone)]
pub struct VerifyContext {
    pub meeting: Meeting,
    /// (artifact, decoded) for the transcript the claims cite.
    pub transcript: Option<(Artifact, Transcript)>,
    pub calendar_event: Option<CalendarEvent>,
    /// The claims artifact being verified.
    pub claims_artifact: Artifact,
    /// Any other artifacts (summary, earlier meetings' facts, ...).
    pub extra: Vec<Artifact>,
    /// Job params (time zone override, thresholds, verifier subset, ...).
    pub params: serde_json::Value,
}

#[async_trait]
pub trait Verifier: Send + Sync {
    fn descriptor(&self) -> VerifierDescriptor;
    async fn verify(&self, claims: &[Claim], ctx: &VerifyContext) -> crate::Result<Vec<Verdict>>;
}
