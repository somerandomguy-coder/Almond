//! Speaker labels: who each transcript speaker is (`speaker_labels` artifacts), labelling by
//! the user, suggestions (voice, calendar, "me") and auto-labelling of new transcripts.
//!
//! **Voice matching:** a speaker matches the person with the highest cosine similarity between
//! the speaker's voice in this meeting and any of the person's voice prints (same model), if
//! it is at least the threshold (setting [`VOICE_THRESHOLD_SETTING`], default
//! [`DEFAULT_VOICE_THRESHOLD`]) and beats the runner-up person by [`DEFAULT_VOICE_MARGIN`]. Each
//! person is suggested for at most one speaker per meeting (best score first). The threshold was
//! calibrated on real recordings, see `docs/people-voice.md`.
//!
//! **Precedence:** `source: user` labels are never replaced by suggestions; suggestions only
//! fill speakers without a user label.

use super::{Voices, catalog};
use crate::Core;
use crate::capture::voice::{SpeakerVoice, cosine, meeting_audio, speaker_voices};
use almond_types::api::notifications;
use almond_types::artifact::body::{LabelSource, SpeakerLabel, SpeakerLabels, Transcript};
use almond_types::artifact::{Artifact, ArtifactDraft, ArtifactKind, Provenance};
use almond_types::event::{Event, EventBody};
use almond_types::ids::{DeviceId, SpeakerId};
use almond_types::meeting::Meeting;
use almond_types::people::{Person, VoicePrint};
use almond_types::store::Store;
use almond_types::sync::VersionVector;
use almond_types::time::now_ms;
use almond_types::{Error, Id, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Weak};

pub(crate) const KIND: &str = ArtifactKind::SPEAKER_LABELS;
/// `Provenance::processor` of `speaker_labels` artifacts.
pub const LABELS_PROCESSOR: &str = "people.labels";
/// Local setting (number): minimum cosine similarity for a voice suggestion.
pub const VOICE_THRESHOLD_SETTING: &str = "people.voice_threshold";
/// Calibrated on the 3D-Speaker ERes2Net-base embeddings (docs/people-voice.md).
pub const DEFAULT_VOICE_THRESHOLD: f32 = 0.5;
/// The best person must beat the runner-up by this much.
pub const DEFAULT_VOICE_MARGIN: f32 = 0.05;
/// Voice prints kept per person (oldest dropped first).
const MAX_PRINTS: usize = 20;
/// "me" adds a print to the self person automatically until they have this many.
const SELF_AUTO_PRINTS: usize = 3;
/// The mic channel's speaker (`almond_stt::transcript::ME`).
pub(crate) const ME: &str = "me";

/// `people.labelSpeaker` params: exactly one of `person_id`, `name` or `me: true`.
#[derive(Debug, Clone, Deserialize)]
pub struct LabelSpeakerParams {
    pub meeting_id: Id,
    pub speaker: SpeakerId,
    #[serde(default)]
    pub person_id: Option<Id>,
    /// An existing person with this name (or alias) is reused; otherwise one is created.
    #[serde(default)]
    pub name: Option<String>,
    /// "This is me": the `is_self` person (created if missing).
    #[serde(default)]
    pub me: bool,
}

// ---------------------------------------------------------------- artifacts

/// Latest artifact not superseded by another (newest `created_at` wins among those).
pub(crate) fn latest_live(arts: &[Artifact]) -> Option<&Artifact> {
    let superseded: BTreeSet<&Id> = arts.iter().filter_map(|a| a.supersedes.as_ref()).collect();
    let newest = |a: &&Artifact, b: &&Artifact| {
        (a.created_at, &a.device.0, &a.id.0).cmp(&(b.created_at, &b.device.0, &b.id.0))
    };
    arts.iter()
        .filter(|a| !superseded.contains(&a.id))
        .max_by(newest)
        .or_else(|| arts.iter().max_by(newest))
}

fn meeting(core: &Core, id: &Id) -> Result<Meeting> {
    core.store()
        .get_meeting(id)?
        .ok_or_else(|| Error::NotFound(format!("meeting {id}")))
}

pub(crate) fn current_transcript(core: &Core, meeting: &Id) -> Result<Option<Artifact>> {
    let arts = core
        .store()
        .list_artifacts(meeting, Some(ArtifactKind::TRANSCRIPT))?;
    Ok(latest_live(&arts).cloned())
}

/// The meeting's current labels artifact and body.
pub(crate) fn current_labels(
    core: &Core,
    meeting: &Id,
) -> Result<Option<(Artifact, SpeakerLabels)>> {
    let arts = core.store().list_artifacts(meeting, Some(KIND))?;
    Ok(latest_live(&arts).and_then(|a| a.decode::<SpeakerLabels>().ok().map(|b| (a.clone(), b))))
}

/// The labels that apply to `transcript` (empty when the current labels are for an older one).
fn labels_for(
    core: &Core,
    meeting: &Id,
    transcript: &Id,
) -> Result<(Option<Artifact>, Vec<SpeakerLabel>)> {
    Ok(match current_labels(core, meeting)? {
        Some((a, b)) if &b.transcript == transcript => (Some(a), b.labels),
        Some((a, _)) => (Some(a), vec![]),
        None => (None, vec![]),
    })
}

fn write_labels(
    core: &Core,
    meeting: &Id,
    transcript: &Id,
    labels: Vec<SpeakerLabel>,
    prev: Option<&Artifact>,
    model: Option<String>,
) -> Result<SpeakerLabels> {
    let body = SpeakerLabels {
        transcript: transcript.clone(),
        labels,
    };
    let mut d = ArtifactDraft::new(
        meeting.clone(),
        &body,
        Provenance {
            processor: LABELS_PROCESSOR.into(),
            processor_version: env!("CARGO_PKG_VERSION").into(),
            model,
            params_hash: None,
            inputs: vec![transcript.clone()],
        },
    )?;
    let clock = &core.people.clock;
    if let Some(p) = prev {
        clock.observe(p.created_at);
        d.supersedes = Some(p.id.clone());
    }
    let a = d.into_artifact(Id::new(), clock.now(), core.device_id().clone());
    core.store().append_local(EventBody::ArtifactAdd(a))?;
    core.processing.kick();
    Ok(body)
}

fn speakers_of(t: &Transcript) -> Vec<SpeakerId> {
    let mut v: Vec<SpeakerId> = t.speakers.iter().map(|s| s.id.clone()).collect();
    for s in &t.segments {
        if let Some(id) = &s.speaker
            && !v.contains(id)
        {
            v.push(id.clone());
        }
    }
    v
}

/// Replace (or drop, `to = None`) a person in every meeting's current labels. Call under the
/// write lock.
pub(crate) fn replace_person(core: &Core, from: &Id, to: Option<&Id>) -> Result<()> {
    for m in core.store().list_meetings(&Default::default())? {
        let Some((a, b)) = current_labels(core, &m.id)? else {
            continue;
        };
        if !b.labels.iter().any(|l| &l.person == from) {
            continue;
        }
        let mut out: Vec<SpeakerLabel> = vec![];
        for mut l in b.labels {
            if &l.person == from {
                match to {
                    Some(t) => l.person = t.clone(),
                    None => continue,
                }
            }
            if !out.iter().any(|x| x.speaker == l.speaker) {
                out.push(l);
            }
        }
        write_labels(
            core,
            &m.id,
            &b.transcript,
            out,
            Some(&a),
            a.provenance.model.clone(),
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------- voices

/// Voices of all speakers of `transcript` (cached). `Ok(None)` when voice matching is not
/// available here (no embedder, audio not on this device).
async fn voices(core: &Arc<Core>, m: &Meeting, transcript: &Artifact) -> Result<Option<Voices>> {
    if let Some(v) = core.people.cached_voices(&transcript.id) {
        return Ok(Some(v));
    }
    let embedder = match core.people.voice_embedder() {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!("voice matching unavailable: {e}");
            return Ok(None);
        }
    };
    let body: Transcript = transcript.decode()?;
    let m = m.clone();
    let blobs = core.blobs().clone();
    let r = tokio::task::spawn_blocking(move || -> Result<BTreeMap<SpeakerId, SpeakerVoice>> {
        let (audio, base) = meeting_audio(blobs.as_ref(), &m)?;
        speaker_voices(embedder.as_ref(), &audio, base, &body, &[])
    })
    .await
    .map_err(Error::other)?;
    match r {
        Ok(v) => {
            let v = Arc::new(v);
            core.people.cache_voices(transcript.id.clone(), v.clone());
            Ok(Some(v))
        }
        Err(Error::NotFound(e)) => {
            tracing::info!("voice prints for meeting {}: {e}", transcript.meeting_id);
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Voice suggestions: speaker → best person (pure; see the module docs for the rule).
pub fn match_voices(
    voices: &BTreeMap<SpeakerId, SpeakerVoice>,
    people: &[Person],
    speakers: &[SpeakerId],
    threshold: f32,
    margin: f32,
) -> Vec<SpeakerLabel> {
    let mut cands: Vec<(f32, SpeakerId, Id)> = vec![];
    for sp in speakers {
        let Some(v) = voices.get(sp) else { continue };
        let mut scores: Vec<(f32, &Id)> = people
            .iter()
            .filter_map(|p| {
                p.voiceprints
                    .iter()
                    .filter(|vp| vp.model == v.model)
                    .map(|vp| cosine(&v.vector, &vp.vector))
                    .fold(None, |m: Option<f32>, s| Some(m.map_or(s, |m| m.max(s))))
                    .map(|s| (s, &p.id))
            })
            .collect();
        scores.sort_by(|a, b| b.0.total_cmp(&a.0));
        let Some(&(best, who)) = scores.first() else {
            continue;
        };
        let second = scores.get(1).map(|s| s.0).unwrap_or(f32::NEG_INFINITY);
        if best >= threshold && best - second >= margin {
            cands.push((best, sp.clone(), who.clone()));
        }
    }
    cands.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut used: BTreeSet<Id> = BTreeSet::new();
    let mut out = vec![];
    for (score, sp, who) in cands {
        if used.insert(who.clone()) {
            out.push(SpeakerLabel {
                speaker: sp,
                person: who,
                source: LabelSource::Voice,
                confidence: Some(score.clamp(0.0, 1.0)),
            });
        }
    }
    out
}

/// Existing labels plus suggestions: user labels always stay; a suggestion replaces an older
/// suggestion for the same speaker.
pub fn merge_labels(
    existing: &[SpeakerLabel],
    suggestions: Vec<SpeakerLabel>,
) -> Vec<SpeakerLabel> {
    let mut out: Vec<SpeakerLabel> = existing.to_vec();
    for s in suggestions {
        match out.iter_mut().find(|l| l.speaker == s.speaker) {
            Some(l) if l.source == LabelSource::User => {}
            Some(l) => *l = s,
            None => out.push(s),
        }
    }
    out
}

fn threshold(core: &Core) -> f32 {
    core.setting_as::<f32>(VOICE_THRESHOLD_SETTING)
        .ok()
        .flatten()
        .filter(|t| t.is_finite())
        .unwrap_or(DEFAULT_VOICE_THRESHOLD)
}

/// Add (or replace) the print of `speaker` in `meeting` on `p`; true if `p` changed.
fn add_print(p: &mut Person, meeting: &Id, speaker: &SpeakerId, v: &SpeakerVoice) -> bool {
    p.voiceprints
        .retain(|x| !(&x.meeting_id == meeting && &x.speaker == speaker));
    p.voiceprints.push(VoicePrint {
        model: v.model.clone(),
        vector: v.vector.clone(),
        meeting_id: meeting.clone(),
        speaker: speaker.clone(),
        speech_s: v.speech_s,
        created_at: now_ms(),
    });
    while p.voiceprints.len() > MAX_PRINTS {
        p.voiceprints.remove(0);
    }
    true
}

fn remove_print(core: &Core, person: &Id, meeting: &Id, speaker: &SpeakerId) -> Result<()> {
    if let Ok(mut p) = catalog::get(core, person) {
        let n = p.voiceprints.len();
        p.voiceprints
            .retain(|x| !(&x.meeting_id == meeting && &x.speaker == speaker));
        if p.voiceprints.len() != n {
            catalog::put(core, &p)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- suggestions

/// Suggested labels for the speakers of `transcript` without a user label: "me" → the self
/// person, voice matches, then one unambiguous calendar attendee. `create` allows creating the
/// self person and attendee people. Call under the write lock.
async fn suggest(
    core: &Arc<Core>,
    m: &Meeting,
    transcript: &Artifact,
    existing: &[SpeakerLabel],
    create: bool,
) -> Result<(Vec<SpeakerLabel>, Option<Voices>)> {
    let body: Transcript = transcript.decode()?;
    let speakers = speakers_of(&body);
    let user: BTreeSet<&SpeakerId> = existing
        .iter()
        .filter(|l| l.source == LabelSource::User)
        .map(|l| &l.speaker)
        .collect();
    let mut out: Vec<SpeakerLabel> = vec![];

    // "me" = the mic channel = the user.
    let me = SpeakerId(ME.into());
    if speakers.contains(&me) && !user.contains(&me) {
        let selfp = if create {
            Some(catalog::self_person(core)?)
        } else {
            catalog::all(core)?.into_iter().find(|p| p.is_self)
        };
        if let Some(p) = selfp {
            out.push(SpeakerLabel {
                speaker: me.clone(),
                person: p.id,
                source: LabelSource::User,
                confidence: None,
            });
        }
    }

    // Voice.
    let remote: Vec<SpeakerId> = speakers
        .iter()
        .filter(|s| s.0 != ME && !user.contains(s))
        .cloned()
        .collect();
    let people = catalog::all(core)?;
    let mut voices_used = None;
    if !remote.is_empty()
        && people.iter().any(|p| !p.voiceprints.is_empty())
        && let Some(v) = voices(core, m, transcript).await?
    {
        out.extend(match_voices(
            &v,
            &people,
            &remote,
            threshold(core),
            DEFAULT_VOICE_MARGIN,
        ));
        voices_used = Some(v);
    }

    // Calendar: one attendee left and one speaker left.
    if let Some(ev) = catalog::linked_event(core, m)? {
        let labelled: BTreeSet<&Id> = existing.iter().chain(&out).map(|l| &l.person).collect();
        let taken: BTreeSet<&SpeakerId> = existing
            .iter()
            .filter(|l| l.source == LabelSource::User)
            .chain(&out)
            .map(|l| &l.speaker)
            .collect();
        let open: Vec<&SpeakerId> = remote.iter().filter(|s| !taken.contains(s)).collect();
        let mut attendees = vec![];
        for a in ev.attendees.iter().filter(|a| !a.is_self) {
            let Some(email) = a.email.as_deref().filter(|e| e.contains('@')) else {
                continue;
            };
            match catalog::find_by_email(&people, email) {
                Some(p) if labelled.contains(&p.id) || p.is_self => {}
                found => attendees.push((a, found.cloned(), email)),
            }
        }
        if let ([(a, found, email)], [speaker]) = (attendees.as_slice(), open.as_slice()) {
            let person = match found {
                Some(p) => Some(p.clone()),
                None if create => {
                    let name = a
                        .name
                        .clone()
                        .filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| email.split('@').next().unwrap_or(email).to_string());
                    let p = catalog::new_person(&name, Some(email));
                    catalog::put(core, &p)?;
                    Some(p)
                }
                None => None,
            };
            if let Some(p) = person {
                out.push(SpeakerLabel {
                    speaker: (*speaker).clone(),
                    person: p.id,
                    source: LabelSource::Calendar,
                    confidence: None,
                });
            }
        }
    }
    Ok((out, voices_used))
}

/// `people.suggestions`: labels the core would suggest now for speakers without a user label
/// (nothing is written; people are not created).
pub(crate) async fn suggestions(core: &Arc<Core>, meeting_id: &Id) -> Result<Vec<SpeakerLabel>> {
    let m = meeting(core, meeting_id)?;
    let Some(t) = current_transcript(core, meeting_id)? else {
        return Ok(vec![]);
    };
    let _g = core.people.write.lock().await;
    let (_, existing) = labels_for(core, meeting_id, &t.id)?;
    Ok(suggest(core, &m, &t, &existing, false).await?.0)
}

// ---------------------------------------------------------------- user labels

pub(crate) async fn label_speaker(
    core: &Arc<Core>,
    p: LabelSpeakerParams,
) -> Result<SpeakerLabels> {
    let name = p.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if [p.person_id.is_some(), name.is_some(), p.me]
        .iter()
        .filter(|x| **x)
        .count()
        != 1
    {
        return Err(Error::Invalid(
            "give exactly one of person_id, name or me: true".into(),
        ));
    }
    let m = meeting(core, &p.meeting_id)?;
    let t = current_transcript(core, &p.meeting_id)?
        .ok_or_else(|| Error::Invalid("the meeting has no transcript yet".into()))?;
    let body: Transcript = t.decode()?;
    if !speakers_of(&body).contains(&p.speaker) {
        return Err(Error::NotFound(format!(
            "speaker {} in the transcript of meeting {}",
            p.speaker.0, p.meeting_id
        )));
    }
    let _g = core.people.write.lock().await;
    let mut person = if p.me {
        catalog::self_person(core)?
    } else if let Some(id) = &p.person_id {
        catalog::get(core, id)?
    } else {
        let name = name.unwrap_or_default();
        let people = catalog::all(core)?;
        match catalog::find_by_name(&people, name) {
            Some(x) => x.clone(),
            None => {
                let x = catalog::new_person(name, None);
                catalog::put(core, &x)?;
                x
            }
        }
    };
    let (prev, mut labels) = labels_for(core, &p.meeting_id, &t.id)?;
    // A wrong user label taught that person this voice: take the print back.
    if let Some(old) = labels.iter().find(|l| l.speaker == p.speaker)
        && old.source == LabelSource::User
        && old.person != person.id
    {
        remove_print(core, &old.person, &p.meeting_id, &p.speaker)?;
    }
    labels.retain(|l| l.speaker != p.speaker);
    labels.push(SpeakerLabel {
        speaker: p.speaker.clone(),
        person: person.id.clone(),
        source: LabelSource::User,
        confidence: None,
    });
    let out = write_labels(core, &p.meeting_id, &t.id, labels, prev.as_ref(), None)?;

    // Teach the person this voice.
    match voices(core, &m, &t).await {
        Ok(Some(v)) => {
            if let Some(sv) = v.get(&p.speaker) {
                // Re-read: `self_person` / `get` above may be stale after other writes.
                person = catalog::get(core, &person.id).unwrap_or(person);
                add_print(&mut person, &p.meeting_id, &p.speaker, sv);
                catalog::put(core, &person)?;
            } else {
                tracing::info!(
                    "speaker {} has too little speech for a voice print",
                    p.speaker.0
                );
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("voice print for {}: {e}", p.speaker.0),
    }
    Ok(out)
}

pub(crate) async fn unlabel_speaker(
    core: &Arc<Core>,
    meeting_id: &Id,
    speaker: &SpeakerId,
) -> Result<SpeakerLabels> {
    let t = current_transcript(core, meeting_id)?
        .ok_or_else(|| Error::Invalid("the meeting has no transcript yet".into()))?;
    let _g = core.people.write.lock().await;
    let (prev, mut labels) = labels_for(core, meeting_id, &t.id)?;
    if let Some(old) = labels.iter().find(|l| &l.speaker == speaker)
        && old.source == LabelSource::User
    {
        remove_print(core, &old.person, meeting_id, speaker)?;
    }
    labels.retain(|l| &l.speaker != speaker);
    write_labels(core, meeting_id, &t.id, labels, prev.as_ref(), None)
}

// ---------------------------------------------------------------- auto-labelling

/// Label a new transcript's speakers automatically (once per transcript).
pub(crate) async fn auto_label(core: &Arc<Core>, transcript: &Artifact) -> Result<()> {
    let _g = core.people.write.lock().await;
    let mid = &transcript.meeting_id;
    // Done already (here or on another device), or superseded meanwhile?
    let done = core
        .store()
        .list_artifacts(mid, Some(KIND))?
        .iter()
        .filter_map(|a| a.decode::<SpeakerLabels>().ok())
        .any(|b| b.transcript == transcript.id);
    let current = current_transcript(core, mid)?;
    if done || current.as_ref().map(|a| &a.id) != Some(&transcript.id) {
        return Ok(());
    }
    let Some(m) = core.store().get_meeting(mid)? else {
        return Ok(());
    };
    let (prev, existing) = labels_for(core, mid, &transcript.id)?;
    let (sugg, voices) = suggest(core, &m, transcript, &existing, true).await?;

    // Learn the user's own voice from the first meetings (helps on mixed-channel recordings).
    let me = SpeakerId(ME.into());
    if sugg.iter().any(|l| l.speaker == me) {
        let mut selfp = catalog::self_person(core)?;
        if selfp.voiceprints.len() < SELF_AUTO_PRINTS {
            let v = match voices {
                Some(v) => Some(v),
                None => voices_for_self(core, &m, transcript).await,
            };
            if let Some(sv) = v.as_ref().and_then(|v| v.get(&me)) {
                add_print(&mut selfp, mid, &me, sv);
                catalog::put(core, &selfp)?;
            }
        }
    }
    if sugg.is_empty() {
        return Ok(());
    }
    let model = sugg
        .iter()
        .any(|l| l.source == LabelSource::Voice)
        .then(|| core.people.voice_embedder().ok().map(|e| e.model()))
        .flatten();
    let labels = merge_labels(&existing, sugg);
    write_labels(core, mid, &transcript.id, labels, prev.as_ref(), model)?;
    Ok(())
}

async fn voices_for_self(core: &Arc<Core>, m: &Meeting, t: &Artifact) -> Option<Voices> {
    match voices(core, m, t).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("voice print for me: {e}");
            None
        }
    }
}

/// Event-log cursor: the contiguous prefix per device plus events seen beyond it.
struct Cursor {
    vv: VersionVector,
    seen: HashSet<(DeviceId, u64)>,
}

impl Cursor {
    fn mark(&mut self, ev: &Event) {
        self.seen.insert((ev.device.clone(), ev.seq));
        loop {
            let next = (ev.device.clone(), self.vv.get(&ev.device) + 1);
            if !self.seen.remove(&next) {
                break;
            }
            self.vv.observe(&next.0, next.1);
        }
    }
}

/// New transcripts this device produced, from the event log after the cursor.
fn new_transcripts(core: &Core, cur: &mut Cursor) -> Result<Vec<Artifact>> {
    let me = core.device_id().clone();
    let mut out = vec![];
    loop {
        let evs = core.store().events_missing(&cur.vv, 500)?;
        let fresh: Vec<Event> = evs
            .into_iter()
            .filter(|e| !cur.seen.contains(&(e.device.clone(), e.seq)))
            .collect();
        if fresh.is_empty() {
            return Ok(out);
        }
        for ev in &fresh {
            cur.mark(ev);
            if let Ok(Some(EventBody::ArtifactAdd(a))) = EventBody::decode(ev)
                && a.kind.is(ArtifactKind::TRANSCRIPT)
                // The device that transcribed has the audio and the speech models; one
                // device labels.
                && a.device == me
            {
                out.push(a);
            }
        }
    }
}

/// Labels new transcripts of this device. Follows the event log from `vv` (taken when the core
/// started, so nothing written after start-up is missed); `artifact.added` wakes it early.
pub(crate) async fn auto_label_loop(
    weak: Weak<Core>,
    vv: VersionVector,
    mut rx: tokio::sync::broadcast::Receiver<crate::Notification>,
) {
    let mut cur = Cursor {
        vv,
        seen: HashSet::new(),
    };
    loop {
        tokio::select! {
            r = rx.recv() => {
                if matches!(r, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                    return;
                }
                if !matches!(&r, Ok(n) if n.name == notifications::ARTIFACT_ADDED) {
                    continue;
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
        }
        let Some(core) = weak.upgrade() else { return };
        let todo = match new_transcripts(&core, &mut cur) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("people: reading the event log: {e}");
                continue;
            }
        };
        for a in todo {
            if let Err(e) = auto_label(&core, &a).await {
                tracing::warn!("labelling the speakers of meeting {}: {e}", a.meeting_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: &str, prints: &[&[f32]]) -> Person {
        let mut p = catalog::new_person(id, None);
        p.id = Id(id.into());
        p.voiceprints = prints
            .iter()
            .map(|v| VoicePrint {
                model: "m".into(),
                vector: v.to_vec(),
                meeting_id: Id("x".into()),
                speaker: SpeakerId("S1".into()),
                speech_s: 10.0,
                created_at: 0,
            })
            .collect();
        p
    }

    fn voice(v: &[f32]) -> SpeakerVoice {
        SpeakerVoice {
            model: "m".into(),
            vector: v.to_vec(),
            speech_s: 10.0,
        }
    }

    fn sp(s: &str) -> SpeakerId {
        SpeakerId(s.into())
    }

    #[test]
    fn voice_matching_threshold_margin_and_one_person_per_meeting() {
        let people = [
            person("lan", &[&[1.0, 0.0, 0.0]]),
            person("david", &[&[0.0, 1.0, 0.0], &[0.0, 0.9, 0.1]]),
        ];
        let voices: BTreeMap<_, _> = [
            (sp("S1"), voice(&[0.95, 0.05, 0.0])), // lan
            (sp("S2"), voice(&[0.0, 0.0, 1.0])),   // nobody
            (sp("S3"), voice(&[0.1, 1.0, 0.0])),   // david
            (sp("S4"), voice(&[0.9, 0.1, 0.0])),   // lan again, weaker: dropped
            (sp("S5"), voice(&[0.7, 0.7, 0.0])),   // ambiguous: no margin
        ]
        .into_iter()
        .collect();
        let all: Vec<_> = voices.keys().cloned().collect();
        let got = match_voices(&voices, &people, &all, 0.5, 0.05);
        let pairs: Vec<(&str, &str)> = got
            .iter()
            .map(|l| (l.speaker.0.as_str(), l.person.0.as_str()))
            .collect();
        assert_eq!(pairs, vec![("S1", "lan"), ("S3", "david")]);
        assert!(got.iter().all(|l| l.source == LabelSource::Voice));
        assert!(got[0].confidence.unwrap() > 0.99);
        // a different model is never compared
        let other: BTreeMap<_, _> = [(
            sp("S1"),
            SpeakerVoice {
                model: "other".into(),
                ..voice(&[1.0, 0.0, 0.0])
            },
        )]
        .into_iter()
        .collect();
        assert!(match_voices(&other, &people, &[sp("S1")], 0.5, 0.05).is_empty());
    }

    #[test]
    fn user_labels_win_over_suggestions() {
        let l = |s: &str, p: &str, src| SpeakerLabel {
            speaker: sp(s),
            person: Id(p.into()),
            source: src,
            confidence: None,
        };
        let existing = vec![
            l("S1", "lan", LabelSource::User),
            l("S2", "minh", LabelSource::Voice),
        ];
        let merged = merge_labels(
            &existing,
            vec![
                l("S1", "david", LabelSource::Voice),
                l("S2", "david", LabelSource::Voice),
                l("S3", "an", LabelSource::Calendar),
            ],
        );
        assert_eq!(
            merged,
            vec![
                l("S1", "lan", LabelSource::User),
                l("S2", "david", LabelSource::Voice),
                l("S3", "an", LabelSource::Calendar),
            ]
        );
    }
}
