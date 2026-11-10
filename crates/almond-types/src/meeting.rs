use crate::ids::{BlobHash, Id};
use crate::lang::LangTag;
use crate::time::UnixMs;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Meeting {
    pub id: Id,
    pub title: String,
    #[ts(type = "number")]
    pub started_at: UnixMs,
    #[ts(type = "number | null")]
    pub ended_at: Option<UnixMs>,
    pub kind: MeetingKind,
    /// Link to a synced calendar event, if the meeting was started from one.
    pub calendar_event: Option<EventRef>,
    /// Languages expected/detected, most dominant first.
    pub languages: Vec<LangTag>,
    pub audio: Vec<AudioTrack>,
    /// Loro document holding the user's own notes for this meeting.
    pub notes_doc: Id,
    pub tags: Vec<String>,
    /// IANA time zone the meeting happened in (e.g. "Asia/Ho_Chi_Minh"). Used to resolve relative
    /// dates ("thứ Sáu", "tomorrow"). `None` = the device's local zone.
    #[serde(default)]
    pub tz: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum MeetingKind {
    /// Online call recorded on a desktop (mic + system audio). `app` e.g. "google_meet", "zoom".
    Online { app: Option<String> },
    /// In-person, usually recorded on the phone's mic.
    InPerson,
    /// Imported audio file.
    Imported,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EventRef {
    /// `CalendarSourceInfo::id` (e.g. "google:me@gmail.com", "phone", "ics:<hash>").
    pub source: String,
    /// Provider's event id (instance id for recurring events).
    pub event_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AudioChannel {
    /// Local microphone ("me" on desktop; the room on a phone).
    Mic,
    /// System output loopback (the other participants on an online call).
    System,
    /// Already-mixed audio (imports).
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AudioTrack {
    pub channel: AudioChannel,
    pub blob: BlobHash,
    /// "opus" | "wav" | "flac"
    pub codec: String,
    /// Sample rate of the encoded *source* (16 000 for pipeline audio), not the decoder's output
    /// rate (Ogg Opus always decodes at 48 kHz).
    pub sample_rate: u32,
    /// Offset of this track's first sample from `Meeting::started_at`.
    pub offset_ms: u32,
    pub duration_ms: u32,
}
