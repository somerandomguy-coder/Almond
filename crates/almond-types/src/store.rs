//! Local persistence contract (WS-D). SQLite in production; in-memory in `mock`.
//!
//! All mutations go through events: `append_local` (this device authored it) or `apply_remote`
//! (received via sync). Materialized views (meetings, artifacts, calendar, jobs) are derived.
//! Methods are synchronous (SQLite); async callers should use `spawn_blocking`.

use crate::artifact::Artifact;
use crate::calendar::CalendarEvent;
use crate::event::{Event, EventBody};
use crate::ids::{BlobHash, DeviceId, Id};
use crate::job::{Job, JobStatus};
use crate::meeting::Meeting;
use crate::sync::VersionVector;
use crate::time::UnixMs;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]
#[ts(export)]
pub struct MeetingQuery {
    pub text: Option<String>,
    #[ts(type = "number | null")]
    pub from: Option<UnixMs>,
    #[ts(type = "number | null")]
    pub to: Option<UnixMs>,
    pub tag: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SearchHit {
    pub meeting_id: Id,
    pub artifact_id: Option<Id>,
    pub kind: String,
    /// Snippet with matches wrapped in «» .
    pub snippet: String,
    pub segment: Option<u32>,
    pub score: f32,
}

pub trait Store: Send + Sync {
    fn device_id(&self) -> DeviceId;

    // ---- event log ----
    /// Author a local event (assigns seq + HLC), persist, and apply to views.
    fn append_local(&self, body: EventBody) -> crate::Result<Event>;
    /// Apply events received from a peer. Idempotent; returns how many were new.
    /// Must accept unknown kinds (store + relay, don't materialize).
    fn apply_remote(&self, events: Vec<Event>) -> crate::Result<usize>;
    fn version_vector(&self) -> crate::Result<VersionVector>;
    /// Events the holder of `have` is missing, in (device, seq) order, at most `limit`.
    fn events_missing(&self, have: &VersionVector, limit: usize) -> crate::Result<Vec<Event>>;

    // ---- views ----
    fn get_meeting(&self, id: &Id) -> crate::Result<Option<Meeting>>;
    fn list_meetings(&self, q: &MeetingQuery) -> crate::Result<Vec<Meeting>>;
    fn get_artifact(&self, id: &Id) -> crate::Result<Option<Artifact>>;
    /// Latest-first; `kind = None` for all kinds. Includes superseded artifacts.
    fn list_artifacts(&self, meeting: &Id, kind: Option<&str>) -> crate::Result<Vec<Artifact>>;
    /// Concatenated Loro updates (base64-decoded, in apply order) for a notes doc.
    fn notes_updates(&self, doc: &Id) -> crate::Result<Vec<Vec<u8>>>;
    fn calendar_events(&self, from: UnixMs, to: UnixMs) -> crate::Result<Vec<CalendarEvent>>;
    fn jobs(&self, status: Option<JobStatus>) -> crate::Result<Vec<(Job, JobStatus)>>;
    fn search(&self, text: &str, limit: u32) -> crate::Result<Vec<SearchHit>>;

    // ---- local-only key/value (never synced: cursors, settings) ----
    fn kv_get(&self, key: &str) -> crate::Result<Option<String>>;
    fn kv_set(&self, key: &str, value: &str) -> crate::Result<()>;
}

/// Content-addressed blob storage (audio, exports). Hash = BLAKE3 hex.
pub trait BlobStore: Send + Sync {
    fn put(&self, bytes: &[u8]) -> crate::Result<BlobHash>;
    /// Stream a file into the store without loading it all (for recordings).
    fn put_file(&self, path: &std::path::Path) -> crate::Result<BlobHash>;
    fn get(&self, hash: &BlobHash) -> crate::Result<Option<Vec<u8>>>;
    fn has(&self, hash: &BlobHash) -> crate::Result<bool>;
    /// Local filesystem path if stored as a file (lets engines mmap/stream).
    fn path(&self, hash: &BlobHash) -> crate::Result<Option<std::path::PathBuf>>;
}
