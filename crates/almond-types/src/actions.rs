//! Actions: user-confirmed side effects after a meeting (Gmail follow-up draft, calendar
//! follow-up event, Markdown export, ...). Proposals are stored as `action_proposals` artifacts;
//! execution always requires an explicit user click and records an `action_result` artifact.

use crate::artifact::{Artifact, ArtifactBody, ArtifactKind};
use crate::meeting::Meeting;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActionDescriptor {
    /// e.g. "gmail.draft_follow_up", "calendar.create_follow_up", "export.markdown"
    pub id: String,
    pub title: String,
    /// Account/capability requirements, e.g. "google:gmail.compose".
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActionProposal {
    pub id: String,
    pub action: String,
    pub title: String,
    /// Human-readable preview (e.g. draft email body) shown before executing.
    pub preview: String,
    /// Action-specific payload, editable by the user before execution.
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]
#[ts(export)]
pub struct ActionProposals {
    pub proposals: Vec<ActionProposal>,
}
impl ArtifactBody for ActionProposals {
    const KIND: &'static str = ArtifactKind::ACTION_PROPOSALS;
    const SCHEMA: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActionResult {
    pub proposal_id: String,
    pub ok: bool,
    pub message: String,
    /// e.g. link to the created draft.
    pub url: Option<String>,
}
impl ArtifactBody for ActionResult {
    const KIND: &'static str = ArtifactKind::ACTION_RESULT;
    const SCHEMA: u32 = 1;
}

#[async_trait]
pub trait Action: Send + Sync {
    fn descriptor(&self) -> ActionDescriptor;
    async fn propose(
        &self,
        meeting: &Meeting,
        artifacts: &[Artifact],
    ) -> crate::Result<Vec<ActionProposal>>;
    async fn execute(&self, proposal: &ActionProposal) -> crate::Result<ActionResult>;
}
