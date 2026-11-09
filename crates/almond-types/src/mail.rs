//! Mail context contract (WS-E). Read-only in Phase 1; drafting is an `Action` later.

use crate::time::UnixMs;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MailThreadSummary {
    pub account: String,
    pub thread_id: String,
    pub subject: String,
    pub participants: Vec<String>,
    pub snippet: String,
    #[ts(type = "number")]
    pub last_message_at: UnixMs,
    /// Deep link to open the thread in a browser.
    pub url: Option<String>,
}

#[async_trait]
pub trait MailSource: Send + Sync {
    fn account(&self) -> &str;
    /// Recent threads involving any of these email addresses.
    async fn threads_with(
        &self,
        emails: &[String],
        limit: u32,
    ) -> crate::Result<Vec<MailThreadSummary>>;
}
