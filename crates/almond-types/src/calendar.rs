//! Calendar contract (WS-E; phone source data via WS-G).

use crate::time::UnixMs;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CalendarSourceInfo {
    /// Stable id: "google:<email>", "phone:<device id>", "ics:<blake3 of url>".
    pub id: String,
    /// "google" | "phone" | "ics"
    pub provider: String,
    /// Account email / display name.
    pub label: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CalendarEvent {
    pub source: String,
    /// Provider id (instance id for recurring events).
    pub event_id: String,
    /// Calendar within the source (Google calendarId, Android calendar id).
    pub calendar_id: String,
    pub calendar_name: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    #[ts(type = "number")]
    pub start: UnixMs,
    #[ts(type = "number")]
    pub end: UnixMs,
    pub all_day: bool,
    pub attendees: Vec<Attendee>,
    /// Detected video-call link (Meet/Zoom/Teams).
    pub conference_url: Option<String>,
    /// "google_meet" | "zoom" | "teams" | other
    pub conference_app: Option<String>,
    pub organizer: Option<String>,
    /// "confirmed" | "tentative" | "cancelled"
    pub status: String,
    #[ts(type = "number")]
    pub updated: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Attendee {
    pub email: Option<String>,
    pub name: Option<String>,
    /// "accepted" | "declined" | "tentative" | "needs_action"
    pub response: Option<String>,
    pub is_self: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CalendarDelta {
    pub upserts: Vec<CalendarEvent>,
    /// event ids removed since the cursor
    pub deletes: Vec<String>,
    /// Opaque cursor (Google syncToken, etc.) to pass next time.
    pub next_cursor: Option<String>,
}

#[async_trait]
pub trait CalendarSource: Send + Sync {
    fn info(&self) -> CalendarSourceInfo;
    /// Incremental sync. `cursor = None` → full sync of a window around now
    /// (default: 30 days back, 90 days ahead).
    async fn sync(&self, cursor: Option<String>) -> crate::Result<CalendarDelta>;
}
