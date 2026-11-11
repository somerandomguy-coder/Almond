//! Feature module "people": a catalog of the people you meet, with their voices recognised so
//! transcripts show names (docs/FEATURES-2.md, "Feature `people`").
//!
//! | Piece | Where |
//! |---|---|
//! | `people.list` / `get` / `save` / `delete` / `merge`, per-person aggregation | [`catalog`] |
//! | `people.labelSpeaker` / `unlabelSpeaker` / `suggestions`, auto-labelling of new transcripts | [`labels`] |
//! | voice prints (speaker embeddings) | [`crate::capture::voice`] |
//!
//! Data:
//! - People are synced records (`almond_types::people::Person`, collection `"person"`). The
//!   person with `is_self` is the user; it is created on first use with the fixed id `"self"`
//!   (so two devices creating it converge on one record).
//! - Who each transcript speaker is lives in `speaker_labels` artifacts: the latest
//!   non-superseded one per meeting wins, and every change supersedes it.
//! - Labelling a speaker (`source: user`) adds a `VoicePrint` (that speaker's mean embedding in
//!   that meeting) to the person; new transcripts are then matched against every person's prints
//!   (`source: voice`, confidence = cosine similarity), calendar attendees by email
//!   (`source: calendar`), and "me" (the mic channel) is the `is_self` person.
//! - API responses carry voice prints without their vectors (`vector: []`).

mod catalog;
mod labels;

pub use catalog::{PersonDetail, PersonMeeting, PersonSummary, SavePerson};
pub use labels::{
    DEFAULT_VOICE_MARGIN, DEFAULT_VOICE_THRESHOLD, LABELS_PROCESSOR, LabelSpeakerParams,
    VOICE_THRESHOLD_SETTING, match_voices, merge_labels,
};

use crate::capture::voice::{SpeakerVoice, VoiceEmbedder};
use crate::error::ApiResult;
use crate::router::{NoParams, Router};
use crate::{Core, Shared};
use almond_types::ids::SpeakerId;
use almond_types::store::Store;
use almond_types::time::HlcClock;
use almond_types::{Id, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

/// Voice prints of the speakers of one transcript.
pub(crate) type Voices = Arc<BTreeMap<SpeakerId, SpeakerVoice>>;

/// Transcripts whose speaker voices stay cached (labelling several speakers of one meeting
/// decodes its audio once).
const VOICE_CACHE: usize = 6;

pub struct State {
    models_dir: PathBuf,
    over: RwLock<Option<Arc<dyn VoiceEmbedder>>>,
    native: Mutex<Option<Arc<dyn VoiceEmbedder>>>,
    voices: Mutex<VecDeque<(Id, Voices)>>,
    /// Serialises read-modify-write of labels and people on this device.
    write: tokio::sync::Mutex<()>,
    clock: HlcClock,
}

impl State {
    pub(crate) async fn init(shared: &Shared) -> Result<State> {
        Ok(State {
            models_dir: shared.cfg.models_dir.clone(),
            over: Default::default(),
            native: Default::default(),
            voices: Default::default(),
            write: Default::default(),
            clock: HlcClock::new(),
        })
    }

    pub(crate) async fn shutdown(&self) {}

    /// Use this embedder instead of the native 3D-Speaker one (tests with a mock embedder);
    /// `None` restores the native one.
    pub fn set_voice_embedder(&self, e: Option<Arc<dyn VoiceEmbedder>>) {
        *self.over.write().unwrap_or_else(|p| p.into_inner()) = e;
        self.voices
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// The voice embedder (loaded on first use). `Unsupported` without the `sherpa` feature.
    pub fn voice_embedder(&self) -> Result<Arc<dyn VoiceEmbedder>> {
        if let Some(e) = self.over.read().unwrap_or_else(|p| p.into_inner()).clone() {
            return Ok(e);
        }
        let mut n = self.native.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(e) = n.clone() {
            return Ok(e);
        }
        let e = crate::capture::voice::native_embedder(&self.models_dir)?;
        *n = Some(e.clone());
        Ok(e)
    }

    fn cached_voices(&self, transcript: &Id) -> Option<Voices> {
        self.voices
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|(id, _)| id == transcript)
            .map(|(_, v)| v.clone())
    }

    fn cache_voices(&self, transcript: Id, v: Voices) {
        let mut c = self.voices.lock().unwrap_or_else(|p| p.into_inner());
        c.retain(|(id, _)| id != &transcript);
        c.push_back((transcript, v));
        while c.len() > VOICE_CACHE {
            c.pop_front();
        }
    }
}

#[derive(Debug, Deserialize)]
struct IdParams {
    id: Id,
}

#[derive(Debug, Deserialize)]
struct MergeParams {
    keep: Id,
    remove: Id,
}

#[derive(Debug, Deserialize)]
struct MeetingParams {
    meeting_id: Id,
}

#[derive(Debug, Deserialize)]
struct UnlabelParams {
    meeting_id: Id,
    speaker: SpeakerId,
}

pub(crate) fn register(r: &mut Router) {
    r.add("people.list", |core, _: NoParams| async move {
        ApiResult::Ok(catalog::list(&core)?)
    });
    r.add("people.get", |core, p: IdParams| async move {
        ApiResult::Ok(catalog::detail(&core, &p.id)?)
    });
    r.add("people.save", |core, p: SavePerson| async move {
        ApiResult::Ok(catalog::save(&core, p).await?)
    });
    r.add("people.delete", |core, p: IdParams| async move {
        catalog::delete(&core, &p.id).await?;
        ApiResult::Ok(serde_json::Value::Null)
    });
    r.add("people.merge", |core, p: MergeParams| async move {
        ApiResult::Ok(catalog::merge(&core, &p.keep, &p.remove).await?)
    });
    r.add(
        "people.labelSpeaker",
        |core, p: LabelSpeakerParams| async move {
            ApiResult::Ok(labels::label_speaker(&core, p).await?)
        },
    );
    r.add(
        "people.unlabelSpeaker",
        |core, p: UnlabelParams| async move {
            ApiResult::Ok(labels::unlabel_speaker(&core, &p.meeting_id, &p.speaker).await?)
        },
    );
    r.add("people.suggestions", |core, p: MeetingParams| async move {
        ApiResult::Ok(labels::suggestions(&core, &p.meeting_id).await?)
    });
}

pub(crate) fn start(core: Arc<Core>) {
    // Follow the event log from now on (taken here, before `Core::open` returns, so no new
    // transcript is missed); `artifact.added` only wakes the loop early.
    let vv = match core.store().version_vector() {
        Ok(vv) => vv,
        Err(e) => {
            tracing::error!("people: cannot read the event log: {e}");
            return;
        }
    };
    let rx = core.notifier.subscribe();
    let weak = Arc::downgrade(&core);
    tokio::spawn(labels::auto_label_loop(weak, vv, rx));
}
