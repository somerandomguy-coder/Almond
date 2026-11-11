//! "Ask Almond": chat with the local LLM about your meetings, notes and calendar.
//!
//! A [`ChatThread`] is a synced record (collection [`COLLECTION`]); the whole thread is one
//! record, last writer wins (chat is usually on one device at a time). Answers cite where they
//! got things from ([`ChatSource`]) so the UI can link back to meetings and transcript lines.

use crate::artifact::SpanRef;
use crate::ids::Id;
use crate::time::UnixMs;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Record collection for chat threads.
pub const COLLECTION: &str = "chat";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatThread {
    pub id: Id,
    pub title: String,
    pub scope: ChatScope,
    pub messages: Vec<ChatEntry>,
    #[ts(type = "number")]
    pub created_at: UnixMs,
    #[ts(type = "number")]
    pub updated_at: UnixMs,
}

/// What the chat is about. The core builds the model's context from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum ChatScope {
    /// Home screen: everything (recent meetings, notes, upcoming events, people).
    All,
    /// One meeting's transcript, notes, summary and recipe outputs.
    Meeting { meeting_id: Id },
    /// Everything with one person.
    Person { person_id: Id },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatEntry {
    pub id: Id,
    pub role: ChatRole,
    pub text: String,
    #[serde(default)]
    pub sources: Vec<ChatSource>,
    /// Model that wrote an assistant message ("ollama:gemma4:12b").
    #[serde(default)]
    pub model: Option<String>,
    #[ts(type = "number")]
    pub at: UnixMs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ChatRole {
    User,
    Assistant,
}

/// Where an answer came from. `n` matches the `[n]` markers in the answer text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatSource {
    pub n: u32,
    pub kind: ChatSourceKind,
    /// Short label for the chip ("Budget sync · 12 Nov").
    pub label: String,
    #[serde(default)]
    pub meeting_id: Option<Id>,
    /// Transcript segment / summary bullet, when the source is that precise.
    #[serde(default)]
    pub span: Option<SpanRef>,
    /// Calendar event (`source`, `event_id`), for upcoming-event sources.
    #[serde(default)]
    pub event: Option<crate::meeting::EventRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ChatSourceKind {
    Transcript,
    Summary,
    Notes,
    RecipeOutput,
    CalendarEvent,
    Person,
}
