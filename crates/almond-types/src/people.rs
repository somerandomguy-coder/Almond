//! People catalog: who spoke in which meeting, recognised by voice across meetings.
//!
//! A [`Person`] is a synced record (collection [`COLLECTION`], id = `Person::id`). Speakers in a
//! transcript (`Segment::speaker`, e.g. "S2") are tied to people by a `speaker_labels` artifact
//! ([`crate::artifact::body::SpeakerLabels`]); labelling a speaker also adds a [`VoicePrint`] to
//! the person so later meetings can suggest the name automatically.

use crate::ids::{Id, SpeakerId};
use crate::time::UnixMs;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Record collection for people.
pub const COLLECTION: &str = "person";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Person {
    pub id: Id,
    /// Display name ("Lan Nguyễn").
    pub name: String,
    /// Lower-case emails, used to match calendar attendees.
    #[serde(default)]
    pub emails: Vec<String>,
    /// Other names they go by (nicknames, how the transcript or calendar spells them).
    #[serde(default)]
    pub aliases: Vec<String>,
    /// The user themself ("me"). At most one person should have it.
    #[serde(default)]
    pub is_self: bool,
    /// Free-form notes about the person.
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub voiceprints: Vec<VoicePrint>,
    #[ts(type = "number")]
    pub created_at: UnixMs,
}

/// A speaker embedding taken from one meeting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VoicePrint {
    /// Embedding model, e.g. "sherpa:3dspeaker-eres2net-base". Only compare prints of one model.
    pub model: String,
    /// L2-normalised embedding (mean of the speaker's segments).
    pub vector: Vec<f32>,
    pub meeting_id: Id,
    pub speaker: SpeakerId,
    /// Seconds of speech averaged into the vector.
    pub speech_s: f32,
    #[ts(type = "number")]
    pub created_at: UnixMs,
}
