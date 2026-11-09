//! Local API types (docs/API.md). The same methods are exposed as Tauri commands (UI),
//! JSON-RPC 2.0 over a localhost WebSocket (CLI, scripts, external processors) and later MCP.
//! Method names are `namespace.verb`; Tauri command names replace '.' with '_'.

use crate::artifact::Artifact;
use crate::calendar::{CalendarEvent, CalendarSourceInfo};
use crate::ids::{DeviceId, Id};
use crate::job::{Job, JobStatus};
use crate::meeting::{AudioChannel, Meeting, MeetingKind};
use crate::sync::{PeerInfo, SyncReport};
use crate::time::UnixMs;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const API_VERSION: u32 = 1;

/// All request/response methods. Keep in sync with docs/API.md.
pub mod methods {
    pub const MEETINGS_LIST: &str = "meetings.list";
    pub const MEETINGS_GET: &str = "meetings.get";
    pub const MEETINGS_CREATE: &str = "meetings.create";
    pub const MEETINGS_UPDATE: &str = "meetings.update";
    pub const MEETINGS_DELETE: &str = "meetings.delete";
    pub const RECORDING_START: &str = "recording.start";
    pub const RECORDING_STOP: &str = "recording.stop";
    pub const RECORDING_STATUS: &str = "recording.status";
    pub const AUDIO_DEVICES: &str = "audio.devices";
    pub const ARTIFACTS_LIST: &str = "artifacts.list";
    pub const ARTIFACTS_GET: &str = "artifacts.get";
    pub const ARTIFACTS_RERUN: &str = "artifacts.rerun";
    pub const NOTES_GET: &str = "notes.get";
    pub const NOTES_APPLY_UPDATE: &str = "notes.applyUpdate";
    pub const SEARCH_QUERY: &str = "search.query";
    pub const CALENDAR_SOURCES: &str = "calendar.sources";
    pub const CALENDAR_ADD_GOOGLE: &str = "calendar.addGoogle";
    pub const CALENDAR_ADD_ICS: &str = "calendar.addIcs";
    pub const CALENDAR_IMPORT_PHONE: &str = "calendar.importPhone";
    pub const CALENDAR_REMOVE: &str = "calendar.remove";
    pub const CALENDAR_UPCOMING: &str = "calendar.upcoming";
    pub const CALENDAR_REFRESH: &str = "calendar.refresh";
    pub const MAIL_CONTEXT: &str = "mail.context";
    pub const ACTIONS_LIST: &str = "actions.list";
    pub const ACTIONS_PROPOSE: &str = "actions.propose";
    pub const ACTIONS_EXECUTE: &str = "actions.execute";
    pub const PIPELINE_PROCESSORS: &str = "pipeline.processors";
    pub const PIPELINE_REGISTER_EXTERNAL: &str = "pipeline.registerExternal";
    pub const JOBS_LIST: &str = "jobs.list";
    pub const JOBS_CANCEL: &str = "jobs.cancel";
    pub const VERIFY_RUN: &str = "verify.run";
    pub const VERIFY_VERIFIERS: &str = "verify.verifiers";
    pub const SYNC_STATUS: &str = "sync.status";
    pub const SYNC_NOW: &str = "sync.now";
    pub const SYNC_PAIR_BEGIN: &str = "sync.pairBegin";
    pub const SYNC_PAIR_ACCEPT: &str = "sync.pairAccept";
    pub const SYNC_UNPAIR: &str = "sync.unpair";
    pub const SYNC_ADD_ADDR: &str = "sync.addAddr";
    pub const MODELS_LIST: &str = "models.list";
    pub const MODELS_DOWNLOAD: &str = "models.download";
    pub const MODELS_SELECT: &str = "models.select";
    pub const SETTINGS_GET: &str = "settings.get";
    pub const SETTINGS_SET: &str = "settings.set";
}

/// Server → client push notifications (Tauri events / JSON-RPC notifications).
pub mod notifications {
    /// payload: RecordingStatus
    pub const RECORDING_STATUS: &str = "recording.status";
    /// payload: TranscriptUpdate
    pub const TRANSCRIPT_UPDATE: &str = "transcript.update";
    /// payload: JobProgress
    pub const JOB_PROGRESS: &str = "job.progress";
    /// payload: Artifact (new artifact available locally, from any device)
    pub const ARTIFACT_ADDED: &str = "artifact.added";
    /// payload: SyncProgress
    pub const SYNC_PROGRESS: &str = "sync.progress";
    /// payload: MeetingReminder
    pub const CALENDAR_REMINDER: &str = "calendar.reminder";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MeetingDetail {
    pub meeting: Meeting,
    /// Latest non-superseded artifact per kind first, then older versions.
    pub artifacts: Vec<Artifact>,
    pub calendar_event: Option<CalendarEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CreateMeetingParams {
    pub title: Option<String>,
    pub kind: MeetingKind,
    pub calendar_event: Option<crate::meeting::EventRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpdateMeetingParams {
    pub id: Id,
    pub title: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StartRecordingParams {
    /// Existing meeting, or None to create one.
    pub meeting_id: Option<Id>,
    pub create: Option<CreateMeetingParams>,
    pub channels: Vec<AudioChannel>,
    /// Device id per channel; None = default.
    pub mic_device: Option<String>,
    pub system_device: Option<String>,
    /// Live captions on/off.
    pub live_transcript: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RecordingState {
    Idle,
    Recording,
    Paused,
    Finalizing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RecordingStatus {
    pub state: RecordingState,
    pub meeting_id: Option<Id>,
    #[ts(type = "number | null")]
    pub started_at: Option<UnixMs>,
    pub elapsed_ms: u32,
    /// RMS level per channel in [0,1] for VU meters.
    pub levels: Vec<(AudioChannel, f32)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TranscriptUpdate {
    pub meeting_id: Id,
    pub update: crate::stt::SttUpdate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct JobProgress {
    pub job: Job,
    pub status: JobStatus,
    pub device: Option<DeviceId>,
    pub progress: f32,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RerunParams {
    pub meeting_id: Id,
    pub processor: String,
    pub inputs: Vec<Id>,
    pub params: serde_json::Value,
    /// Allow another (more capable) device to run it on next sync.
    pub allow_offload: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NotesSnapshot {
    pub doc: Id,
    /// Loro updates, base64, apply in order (or a single snapshot).
    pub updates_b64: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpcomingEvent {
    pub event: CalendarEvent,
    /// Meeting already linked to this event, if any.
    pub meeting_id: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MeetingReminder {
    pub event: CalendarEvent,
    pub minutes_before: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CalendarSourcesResult {
    pub sources: Vec<CalendarSourceInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SyncStatus {
    pub this_device: crate::sync::DeviceInfo,
    pub peers: Vec<PeerInfo>,
    pub syncing: bool,
    pub auto_sync_while_open: bool,
    pub last_reports: Vec<SyncReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SyncProgress {
    pub peer: DeviceId,
    /// "connecting" | "events" | "blobs" | "done" | "error"
    pub phase: String,
    pub done: u32,
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PairBeginResult {
    /// Encoded `PairingTicket` for the QR code.
    pub qr_payload: String,
    /// Same ticket as six words for desktop↔desktop typing.
    pub words: Vec<String>,
    #[ts(type = "number")]
    pub expires_at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelSlot {
    /// "stt" | "stt_live" | "llm_summary" | "llm_verify" | "embed"
    pub task: String,
    /// None = all languages.
    pub lang: Option<String>,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelEntry {
    pub id: String,
    /// "stt" | "llm" | "embed" | "diarize" | "vad"
    pub kind: String,
    pub installed: bool,
    #[ts(type = "number | null")]
    pub size_bytes: Option<u64>,
    pub languages: Vec<String>,
    pub note: Option<String>,
}

/// JSON-RPC error codes beyond the standard ones.
pub mod error_codes {
    pub const NOT_FOUND: i32 = -32004;
    pub const INVALID: i32 = -32602;
    pub const AUTH_REQUIRED: i32 = -32001;
    pub const UNSUPPORTED: i32 = -32005;
    pub const INTERNAL: i32 = -32603;
}
