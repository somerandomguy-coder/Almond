//! The replicated event log. Every state change on any device is an `Event`; devices sync by
//! exchanging events they haven't seen (see [`crate::sync::VersionVector`]).
//!
//! `kind` + JSON `body` (not a Rust enum) so that devices running older versions store and relay
//! event kinds they don't understand. Use [`EventBody`] for typed construction/decoding.

use crate::artifact::Artifact;
use crate::calendar::CalendarEvent;
use crate::ids::{DeviceId, Id};
use crate::job::{Job, JobLease, JobOutcome};
use crate::meeting::Meeting;
use crate::time::Hlc;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Event {
    /// Author device.
    pub device: DeviceId,
    /// Per-author monotonically increasing sequence number, starting at 1, no gaps.
    #[ts(type = "number")]
    pub seq: u64,
    pub hlc: Hlc,
    pub kind: EventKind,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EventKind(pub String);

impl EventKind {
    pub const MEETING_UPSERT: &'static str = "meeting.upsert";
    pub const MEETING_DELETE: &'static str = "meeting.delete";
    pub const ARTIFACT_ADD: &'static str = "artifact.add";
    pub const NOTES_UPDATE: &'static str = "notes.update";
    pub const CALENDAR_UPSERT: &'static str = "calendar.upsert";
    pub const CALENDAR_DELETE: &'static str = "calendar.delete";
    pub const JOB_REQUESTED: &'static str = "job.requested";
    pub const JOB_CLAIMED: &'static str = "job.claimed";
    pub const JOB_FINISHED: &'static str = "job.finished";
    pub const DEVICE_INFO: &'static str = "device.info";
    pub const BLOB_AVAILABLE: &'static str = "blob.available";
}

/// Typed view of known event bodies. Unknown kinds decode to `None` from [`EventBody::decode`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventBody {
    /// Full meeting record; LWW by (hlc, device) per meeting id.
    MeetingUpsert(Meeting),
    MeetingDelete {
        id: Id,
    },
    ArtifactAdd(Artifact),
    /// Loro update bytes (base64) for a meeting's notes doc.
    NotesUpdate {
        doc: Id,
        update_b64: String,
    },
    CalendarUpsert(CalendarEvent),
    CalendarDelete {
        source: String,
        event_id: String,
    },
    JobRequested(Job),
    JobClaimed(JobLease),
    JobFinished(JobOutcome),
    DeviceInfo(crate::sync::DeviceInfo),
    BlobAvailable {
        hash: crate::ids::BlobHash,
        size: u64,
        mime: String,
    },
}

impl EventBody {
    pub fn kind(&self) -> &'static str {
        use EventBody::*;
        match self {
            MeetingUpsert(_) => EventKind::MEETING_UPSERT,
            MeetingDelete { .. } => EventKind::MEETING_DELETE,
            ArtifactAdd(_) => EventKind::ARTIFACT_ADD,
            NotesUpdate { .. } => EventKind::NOTES_UPDATE,
            CalendarUpsert(_) => EventKind::CALENDAR_UPSERT,
            CalendarDelete { .. } => EventKind::CALENDAR_DELETE,
            JobRequested(_) => EventKind::JOB_REQUESTED,
            JobClaimed(_) => EventKind::JOB_CLAIMED,
            JobFinished(_) => EventKind::JOB_FINISHED,
            DeviceInfo(_) => EventKind::DEVICE_INFO,
            BlobAvailable { .. } => EventKind::BLOB_AVAILABLE,
        }
    }

    pub fn to_json(&self) -> crate::Result<serde_json::Value> {
        use EventBody::*;
        Ok(match self {
            MeetingUpsert(m) => serde_json::to_value(m)?,
            MeetingDelete { id } => serde_json::json!({ "id": id }),
            ArtifactAdd(a) => serde_json::to_value(a)?,
            NotesUpdate { doc, update_b64 } => {
                serde_json::json!({ "doc": doc, "update_b64": update_b64 })
            }
            CalendarUpsert(e) => serde_json::to_value(e)?,
            CalendarDelete { source, event_id } => {
                serde_json::json!({ "source": source, "event_id": event_id })
            }
            JobRequested(j) => serde_json::to_value(j)?,
            JobClaimed(l) => serde_json::to_value(l)?,
            JobFinished(o) => serde_json::to_value(o)?,
            DeviceInfo(d) => serde_json::to_value(d)?,
            BlobAvailable { hash, size, mime } => {
                serde_json::json!({ "hash": hash, "size": size, "mime": mime })
            }
        })
    }

    /// Decode a stored/received event. `Ok(None)` = unknown kind (keep and relay it anyway).
    pub fn decode(ev: &Event) -> crate::Result<Option<EventBody>> {
        use serde_json::from_value as fv;
        let b = ev.body.clone();
        #[derive(Deserialize)]
        struct IdOnly {
            id: Id,
        }
        #[derive(Deserialize)]
        struct Notes {
            doc: Id,
            update_b64: String,
        }
        #[derive(Deserialize)]
        struct CalDel {
            source: String,
            event_id: String,
        }
        #[derive(Deserialize)]
        struct Blob {
            hash: crate::ids::BlobHash,
            size: u64,
            mime: String,
        }
        Ok(Some(match ev.kind.0.as_str() {
            EventKind::MEETING_UPSERT => EventBody::MeetingUpsert(fv(b)?),
            EventKind::MEETING_DELETE => EventBody::MeetingDelete {
                id: fv::<IdOnly>(b)?.id,
            },
            EventKind::ARTIFACT_ADD => EventBody::ArtifactAdd(fv(b)?),
            EventKind::NOTES_UPDATE => {
                let n: Notes = fv(b)?;
                EventBody::NotesUpdate {
                    doc: n.doc,
                    update_b64: n.update_b64,
                }
            }
            EventKind::CALENDAR_UPSERT => EventBody::CalendarUpsert(fv(b)?),
            EventKind::CALENDAR_DELETE => {
                let c: CalDel = fv(b)?;
                EventBody::CalendarDelete {
                    source: c.source,
                    event_id: c.event_id,
                }
            }
            EventKind::JOB_REQUESTED => EventBody::JobRequested(fv(b)?),
            EventKind::JOB_CLAIMED => EventBody::JobClaimed(fv(b)?),
            EventKind::JOB_FINISHED => EventBody::JobFinished(fv(b)?),
            EventKind::DEVICE_INFO => EventBody::DeviceInfo(fv(b)?),
            EventKind::BLOB_AVAILABLE => {
                let x: Blob = fv(b)?;
                EventBody::BlobAvailable {
                    hash: x.hash,
                    size: x.size,
                    mime: x.mime,
                }
            }
            _ => return Ok(None),
        }))
    }
}
