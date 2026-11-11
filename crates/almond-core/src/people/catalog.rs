//! The people catalog: records, the self person, aggregation per person (meetings by speaker
//! labels and by calendar attendee email, talk time, last met), save / delete / merge.

use super::labels;
use crate::Core;
use almond_types::artifact::body::{LabelSource, SpeakerLabels, Transcript};
use almond_types::calendar::CalendarEvent;
use almond_types::ids::SpeakerId;
use almond_types::meeting::Meeting;
use almond_types::people::{COLLECTION, Person, VoicePrint};
use almond_types::store::{MeetingQuery, Store};
use almond_types::time::{UnixMs, now_ms};
use almond_types::{Error, Id, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Id of the `is_self` person when this app creates it.
pub(crate) const SELF_ID: &str = "self";

/// `people.list` item.
#[derive(Debug, Clone, Serialize)]
pub struct PersonSummary {
    /// Voice prints without vectors.
    pub person: Person,
    pub meeting_count: u32,
    pub talk_time_s: f32,
    pub last_met: Option<UnixMs>,
}

/// One meeting with a person.
#[derive(Debug, Clone, Serialize)]
pub struct PersonMeeting {
    pub meeting_id: Id,
    pub title: String,
    pub started_at: UnixMs,
    /// Transcript speakers labelled as this person.
    pub speakers: Vec<SpeakerId>,
    /// Seconds of transcript speech by those speakers.
    pub talk_time_s: f32,
    /// Strongest label source among those speakers (`user` > `voice` > `calendar`); `None` when
    /// the meeting is linked only through a calendar attendee email.
    pub source: Option<LabelSource>,
    /// The person's email is among the linked calendar event's attendees.
    pub attendee: bool,
}

/// `people.get`.
#[derive(Debug, Clone, Serialize)]
pub struct PersonDetail {
    pub person: Person,
    /// Newest first.
    pub meetings: Vec<PersonMeeting>,
    pub talk_time_s: f32,
    pub last_met: Option<UnixMs>,
}

/// `people.save` params: create (no `id`) or update. Fields left out keep their value.
#[derive(Debug, Clone, Deserialize)]
pub struct SavePerson {
    #[serde(default)]
    pub id: Option<Id>,
    pub name: String,
    #[serde(default)]
    pub emails: Option<Vec<String>>,
    #[serde(default)]
    pub aliases: Option<Vec<String>>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub is_self: Option<bool>,
    /// The prints to keep (matched by `meeting_id` + `speaker` + `created_at`; vectors are
    /// ignored): leaving one out deletes it. New prints come only from labelling speakers.
    #[serde(default)]
    pub voiceprints: Option<Vec<VoicePrint>>,
}

pub(crate) fn all(core: &Core) -> Result<Vec<Person>> {
    core.records::<Person>(COLLECTION)
}

pub(crate) fn get(core: &Core, id: &Id) -> Result<Person> {
    core.record::<Person>(COLLECTION, id.as_str())?
        .ok_or_else(|| Error::NotFound(format!("person {id}")))
}

pub(crate) fn put(core: &Core, p: &Person) -> Result<()> {
    core.put_record(COLLECTION, p.id.as_str(), p)
}

/// The person without voice vectors (API responses).
pub(crate) fn stripped(mut p: Person) -> Person {
    for v in &mut p.voiceprints {
        v.vector = vec![];
    }
    p
}

fn norm_email(e: &str) -> String {
    e.trim().to_lowercase()
}

fn dedup(v: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for s in v {
        let s = s.trim().to_string();
        if !s.is_empty() && !out.iter().any(|x| x.to_lowercase() == s.to_lowercase()) {
            out.push(s);
        }
    }
    out
}

/// A person whose name or alias is `name` (case-insensitive).
pub(crate) fn find_by_name<'a>(people: &'a [Person], name: &str) -> Option<&'a Person> {
    let n = name.trim().to_lowercase();
    people.iter().find(|p| {
        p.name.trim().to_lowercase() == n || p.aliases.iter().any(|a| a.trim().to_lowercase() == n)
    })
}

pub(crate) fn find_by_email<'a>(people: &'a [Person], email: &str) -> Option<&'a Person> {
    let e = norm_email(email);
    people
        .iter()
        .find(|p| p.emails.iter().any(|x| norm_email(x) == e))
}

/// A new person record (not stored yet).
pub(crate) fn new_person(name: &str, email: Option<&str>) -> Person {
    Person {
        id: Id::new(),
        name: name.trim().to_string(),
        emails: email.map(norm_email).into_iter().collect(),
        aliases: vec![],
        is_self: false,
        notes: String::new(),
        voiceprints: vec![],
        created_at: now_ms(),
    }
}

/// Name for a new self person: the device owner from "Nam's PC", else the OS user (desktop),
/// else "Me".
fn default_self_name(core: &Core) -> String {
    let dev = core.cfg.device_name.trim();
    for sep in ["'s ", "’s "] {
        if let Some((owner, _)) = dev.split_once(sep)
            && !owner.trim().is_empty()
        {
            return owner.trim().to_string();
        }
    }
    if core.cfg.platform != "android" {
        for var in ["USERNAME", "USER"] {
            if let Ok(u) = std::env::var(var)
                && !u.trim().is_empty()
            {
                return u.trim().to_string();
            }
        }
    }
    "Me".into()
}

/// The `is_self` person, created on first use. Call under the module's write lock.
pub(crate) fn self_person(core: &Core) -> Result<Person> {
    let people = all(core)?;
    // Prefer the canonical record if several claim to be "me" (created on two devices).
    if let Some(p) = people
        .iter()
        .find(|p| p.is_self && p.id.as_str() == SELF_ID)
        .or_else(|| people.iter().find(|p| p.is_self))
    {
        return Ok(p.clone());
    }
    let mut p = new_person(&default_self_name(core), None);
    p.id = Id(SELF_ID.into());
    p.is_self = true;
    put(core, &p)?;
    Ok(p)
}

// ---------------------------------------------------------------- aggregation

/// What one meeting says about people.
struct MeetingLinks {
    meeting: Meeting,
    labels: Option<SpeakerLabels>,
    /// Seconds per transcript speaker (of the labelled transcript).
    talk: BTreeMap<SpeakerId, f32>,
    /// Lower-case attendee emails of the linked calendar event.
    attendees: BTreeSet<String>,
}

fn talk_time(t: &Transcript) -> BTreeMap<SpeakerId, f32> {
    let mut m = BTreeMap::new();
    for s in &t.segments {
        if let Some(id) = &s.speaker {
            *m.entry(id.clone()).or_insert(0.0) += s.t1_ms.saturating_sub(s.t0_ms) as f32 / 1000.0;
        }
    }
    m
}

/// The calendar event a meeting is linked to, if it is in the store.
pub(crate) fn linked_event(core: &Core, m: &Meeting) -> Result<Option<CalendarEvent>> {
    let Some(r) = &m.calendar_event else {
        return Ok(None);
    };
    Ok(core
        .store()
        .calendar_events_of(&r.source)?
        .into_iter()
        .find(|e| e.event_id == r.event_id))
}

fn links(core: &Core) -> Result<Vec<MeetingLinks>> {
    let store = core.store();
    let mut events: BTreeMap<String, Vec<CalendarEvent>> = BTreeMap::new();
    let mut out = vec![];
    for meeting in store.list_meetings(&MeetingQuery::default())? {
        let arts = store.list_artifacts(&meeting.id, Some(labels::KIND))?;
        let labels = labels::latest_live(&arts).and_then(|a| a.decode::<SpeakerLabels>().ok());
        let talk = match &labels {
            Some(l) => match store.get_artifact(&l.transcript)? {
                Some(t) => t
                    .decode::<Transcript>()
                    .map(|t| talk_time(&t))
                    .unwrap_or_default(),
                None => BTreeMap::new(),
            },
            None => BTreeMap::new(),
        };
        let mut attendees = BTreeSet::new();
        if let Some(r) = &meeting.calendar_event {
            if !events.contains_key(&r.source) {
                events.insert(r.source.clone(), store.calendar_events_of(&r.source)?);
            }
            if let Some(ev) = events[&r.source].iter().find(|e| e.event_id == r.event_id) {
                attendees.extend(
                    ev.attendees
                        .iter()
                        .filter_map(|a| a.email.as_deref())
                        .map(norm_email),
                );
            }
        }
        out.push(MeetingLinks {
            meeting,
            labels,
            talk,
            attendees,
        });
    }
    Ok(out)
}

fn rank(s: LabelSource) -> u8 {
    match s {
        LabelSource::User => 3,
        LabelSource::Voice => 2,
        LabelSource::Calendar => 1,
    }
}

fn meetings_of(p: &Person, links: &[MeetingLinks]) -> Vec<PersonMeeting> {
    let emails: BTreeSet<String> = p.emails.iter().map(|e| norm_email(e)).collect();
    let mut out = vec![];
    for l in links {
        let mine: Vec<_> = l
            .labels
            .iter()
            .flat_map(|x| &x.labels)
            .filter(|x| x.person == p.id)
            .collect();
        let attendee = !emails.is_disjoint(&l.attendees);
        if mine.is_empty() && !attendee {
            continue;
        }
        out.push(PersonMeeting {
            meeting_id: l.meeting.id.clone(),
            title: l.meeting.title.clone(),
            started_at: l.meeting.started_at,
            speakers: mine.iter().map(|x| x.speaker.clone()).collect(),
            talk_time_s: mine
                .iter()
                .map(|x| l.talk.get(&x.speaker).copied().unwrap_or(0.0))
                .sum(),
            source: mine.iter().map(|x| x.source).max_by_key(|s| rank(*s)),
            attendee,
        });
    }
    out.sort_by_key(|m| std::cmp::Reverse(m.started_at));
    out
}

pub(crate) fn list(core: &Core) -> Result<Vec<PersonSummary>> {
    let links = links(core)?;
    let mut out: Vec<PersonSummary> = all(core)?
        .into_iter()
        .map(|p| {
            let ms = meetings_of(&p, &links);
            PersonSummary {
                meeting_count: ms.len() as u32,
                talk_time_s: ms.iter().map(|m| m.talk_time_s).sum(),
                last_met: ms.iter().map(|m| m.started_at).max(),
                person: stripped(p),
            }
        })
        .collect();
    // Me first, then most recently met, then by name.
    out.sort_by(|a, b| {
        b.person
            .is_self
            .cmp(&a.person.is_self)
            .then(b.last_met.cmp(&a.last_met))
            .then(
                a.person
                    .name
                    .to_lowercase()
                    .cmp(&b.person.name.to_lowercase()),
            )
    });
    Ok(out)
}

pub(crate) fn detail(core: &Core, id: &Id) -> Result<PersonDetail> {
    let p = get(core, id)?;
    let meetings = meetings_of(&p, &links(core)?);
    Ok(PersonDetail {
        talk_time_s: meetings.iter().map(|m| m.talk_time_s).sum(),
        last_met: meetings.iter().map(|m| m.started_at).max(),
        meetings,
        person: stripped(p),
    })
}

// ---------------------------------------------------------------- edits

/// Make `id` the only `is_self` person.
fn clear_other_self(core: &Core, id: &Id) -> Result<()> {
    for mut o in all(core)? {
        if o.is_self && &o.id != id {
            o.is_self = false;
            put(core, &o)?;
        }
    }
    Ok(())
}

pub(crate) async fn save(core: &Core, s: SavePerson) -> Result<Person> {
    let name = s.name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("a person needs a name".into()));
    }
    let _g = core.people.write.lock().await;
    let mut p = match &s.id {
        Some(id) => get(core, id)?,
        None => new_person(name, None),
    };
    p.name = name.to_string();
    if let Some(e) = s.emails {
        p.emails = dedup(e.iter().map(|x| norm_email(x)));
    }
    if let Some(a) = s.aliases {
        p.aliases = dedup(a);
    }
    if let Some(n) = s.notes {
        p.notes = n;
    }
    if let Some(keep) = s.voiceprints {
        p.voiceprints.retain(|v| {
            keep.iter().any(|k| {
                k.meeting_id == v.meeting_id
                    && k.speaker == v.speaker
                    && k.created_at == v.created_at
            })
        });
    }
    if let Some(me) = s.is_self {
        p.is_self = me;
        if me {
            clear_other_self(core, &p.id)?;
        }
    }
    put(core, &p)?;
    Ok(stripped(p))
}

pub(crate) async fn delete(core: &Core, id: &Id) -> Result<()> {
    let _g = core.people.write.lock().await;
    get(core, id)?;
    labels::replace_person(core, id, None)?;
    core.delete_record(COLLECTION, id.as_str())
}

/// `remove` becomes part of `keep`: names, emails, notes, voice prints and speaker labels.
pub(crate) async fn merge(core: &Core, keep: &Id, remove: &Id) -> Result<Person> {
    if keep == remove {
        return Err(Error::Invalid("cannot merge a person with themself".into()));
    }
    let _g = core.people.write.lock().await;
    let mut k = get(core, keep)?;
    let r = get(core, remove)?;
    let mut aliases = k.aliases.clone();
    if r.name.trim().to_lowercase() != k.name.trim().to_lowercase() {
        aliases.push(r.name.clone());
    }
    aliases.extend(r.aliases.iter().cloned());
    k.aliases = dedup(aliases)
        .into_iter()
        .filter(|a| a.to_lowercase() != k.name.to_lowercase())
        .collect();
    k.emails = dedup(k.emails.iter().chain(&r.emails).map(|e| norm_email(e)));
    k.notes = match (k.notes.trim().is_empty(), r.notes.trim().is_empty()) {
        (_, true) => k.notes,
        (true, false) => r.notes.clone(),
        (false, false) => format!("{}\n\n{}", k.notes.trim_end(), r.notes.trim()),
    };
    for v in r.voiceprints {
        if !k
            .voiceprints
            .iter()
            .any(|x| x.meeting_id == v.meeting_id && x.speaker == v.speaker && x.model == v.model)
        {
            k.voiceprints.push(v);
        }
    }
    k.is_self |= r.is_self;
    k.created_at = k.created_at.min(r.created_at);
    put(core, &k)?;
    labels::replace_person(core, remove, Some(keep))?;
    core.delete_record(COLLECTION, remove.as_str())?;
    Ok(stripped(k))
}
