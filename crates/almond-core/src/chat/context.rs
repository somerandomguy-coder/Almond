//! Context builder: turns a [`ChatScope`] and a question into numbered sources for the prompt.
//!
//! - `All`: upcoming events (today + 7 days), full-text search hits for the question (transcript
//!   lines, summaries, notes, recipe outputs), digests of the most recent meetings, and the people
//!   catalog (who you know, when you last met them).
//! - `Meeting`: that meeting's summary, action items, the user's notes, recipe outputs and its
//!   transcript (excerpts chosen by the question when it doesn't fit).
//! - `Person`: the person, upcoming events with them, and their meetings (digest + their lines),
//!   found through `speaker_labels` artifacts and calendar attendees by email.
//!
//! Everything is kept under a token budget (`almond_llm::tokens::estimate_tokens`).

use super::text::{clip, fold, keywords, strip_markers};
use crate::Core;
use almond_llm::tokens::estimate_tokens;
use almond_llm::transcript::fmt_ms;
use almond_types::artifact::body::{
    ActionItems, EnhancedNotes, RecipeOutput, Segment, SpeakerLabels, Summary, Transcript,
};
use almond_types::calendar::CalendarEvent;
use almond_types::chat::{ChatScope, ChatSource, ChatSourceKind};
use almond_types::meeting::{AudioChannel, EventRef};
use almond_types::people::{self, Person};
use almond_types::store::{MeetingQuery, Store};
use almond_types::time::UnixMs;
use almond_types::{Artifact, ArtifactKind, Error, Id, Meeting, Result, SpanRef};
use chrono::{Local, TimeZone};
use std::collections::{HashMap, HashSet};

const MIN_MS: i64 = 60_000;
const DAY_MS: i64 = 24 * 60 * MIN_MS;

/// A numbered source: what the chip shows and links to, and the text the model reads.
#[derive(Debug, Clone)]
pub(crate) struct Source {
    pub src: ChatSource,
    pub text: String,
}

#[derive(Debug, Default)]
pub(crate) struct Context {
    pub sources: Vec<Source>,
    /// Unnumbered background lines (what the chat is about, who the user knows).
    pub background: Vec<String>,
}

// ------------------------------------------------------------------------------ formatting

fn local(ms: UnixMs) -> chrono::DateTime<Local> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(Local::now)
}

/// "Wed 12 Nov 2026 14:05 (+07:00)" — the model's notion of "now".
pub(crate) fn fmt_now(ms: UnixMs) -> String {
    local(ms).format("%A %-d %B %Y, %H:%M (UTC%:z)").to_string()
}

fn fmt_when(ms: UnixMs) -> String {
    local(ms).format("%a %-d %b %Y %H:%M").to_string()
}

fn fmt_day(ms: UnixMs) -> String {
    local(ms).format("%-d %b").to_string()
}

fn fmt_hm(ms: UnixMs) -> String {
    local(ms).format("%H:%M").to_string()
}

fn title_of(m: &Meeting) -> &str {
    if m.title.trim().is_empty() {
        "Untitled meeting"
    } else {
        m.title.trim()
    }
}

/// Chip label: "Budget sync · 12 Nov".
fn meeting_label(m: &Meeting) -> String {
    format!("{} · {}", clip(title_of(m), 48), fmt_day(m.started_at))
}

fn meeting_header(m: &Meeting) -> String {
    let dur = m
        .ended_at
        .map(|e| format!(", {} min", ((e - m.started_at) / MIN_MS).max(1)))
        .unwrap_or_default();
    format!(
        "Meeting \"{}\" on {}{dur}",
        title_of(m),
        fmt_when(m.started_at)
    )
}

/// "Lan Nguyễn <lan@x.com>, Minh".
fn attendees_text(e: &CalendarEvent) -> String {
    e.attendees
        .iter()
        .filter(|a| !a.is_self)
        .map(|a| match (&a.name, &a.email) {
            (Some(n), Some(m)) => format!("{n} <{m}>"),
            (Some(n), None) => n.clone(),
            (None, Some(m)) => m.clone(),
            (None, None) => "?".into(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn relative(start: UnixMs, end: UnixMs, now: UnixMs) -> String {
    if start <= now && now < end {
        return "happening now".into();
    }
    if end <= now {
        return "already over".into();
    }
    let mins = (start - now) / MIN_MS;
    if mins < 60 {
        format!("in {mins} min")
    } else if mins < 24 * 60 {
        format!("in {} h {} min", mins / 60, mins % 60)
    } else {
        format!("in {} days", mins / (24 * 60))
    }
}

fn event_text(e: &CalendarEvent, now: UnixMs, linked: Option<&Meeting>) -> String {
    let when = if e.all_day {
        format!("{} (all day)", local(e.start).format("%a %-d %b %Y"))
    } else {
        format!("{}–{}", fmt_when(e.start), fmt_hm(e.end))
    };
    let mut t = format!(
        "Calendar event \"{}\"\nWhen: {when} ({})",
        e.title.trim(),
        relative(e.start, e.end, now)
    );
    if let Some(l) = e.location.as_deref().filter(|l| !l.trim().is_empty()) {
        t.push_str(&format!("\nWhere: {}", l.trim()));
    }
    if let Some(app) = &e.conference_app {
        t.push_str(&format!("\nVideo call: {}", app.replace('_', " ")));
    }
    let att = attendees_text(e);
    if !att.is_empty() {
        t.push_str(&format!("\nAttendees: {att}"));
    }
    if let Some(o) = e.organizer.as_deref().filter(|o| !o.is_empty()) {
        t.push_str(&format!("\nOrganizer: {o}"));
    }
    if e.status == "tentative" {
        t.push_str("\nStatus: tentative");
    }
    if let Some(d) = e.description.as_deref().filter(|d| !d.trim().is_empty()) {
        t.push_str(&format!("\nDescription: {}", clip(d, 400)));
    }
    if let Some(m) = linked {
        t.push_str(&format!(
            "\nThe user already has notes for it: {}",
            meeting_header(m)
        ));
    }
    t
}

fn summary_text(s: &Summary) -> String {
    let mut t = String::new();
    for sec in &s.sections {
        if !sec.heading.trim().is_empty() {
            t.push_str(&format!("{}:\n", sec.heading.trim()));
        }
        for b in &sec.bullets {
            t.push_str(&format!("- {}\n", b.text.trim()));
        }
    }
    t
}

fn actions_text(a: &ActionItems) -> String {
    let mut t = String::new();
    for i in &a.items {
        let mut extra = vec![];
        if let Some(o) = &i.owner {
            extra.push(format!("owner: {o}"));
        }
        if let Some(d) = &i.due {
            extra.push(format!("due: {d}"));
        }
        let extra = if extra.is_empty() {
            String::new()
        } else {
            format!(" ({})", extra.join(", "))
        };
        t.push_str(&format!("- {}{extra}\n", i.text.trim()));
    }
    t
}

fn ai_notes_text(n: &EnhancedNotes) -> String {
    n.blocks
        .iter()
        .map(|b| {
            let indent = "  ".repeat(b.level as usize);
            match b.style.as_str() {
                "heading" => format!("{}:", b.text.trim()),
                "bullet" | "todo" => format!("{indent}- {}", b.text.trim()),
                _ => b.text.trim().to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shrink `text` to about `max_tokens`.
fn fit(text: &str, max_tokens: usize) -> String {
    let est = estimate_tokens(text);
    if est <= max_tokens {
        return text.trim().to_string();
    }
    let chars = text.chars().count();
    let mut keep = (chars as f64 * max_tokens as f64 / est as f64 * 0.95) as usize;
    loop {
        let t = clip(text, keep.max(1));
        if estimate_tokens(&t) <= max_tokens || keep < 16 {
            return t;
        }
        keep = keep * 9 / 10;
    }
}

// ------------------------------------------------------------------------------ builder

struct Builder {
    ctx: Context,
    used: usize,
    budget: usize,
    /// End of the current section's budget (`section`).
    section_end: usize,
}

impl Builder {
    fn new(budget: usize) -> Self {
        Builder {
            ctx: Context::default(),
            used: 0,
            budget,
            section_end: budget,
        }
    }

    /// Limit the following pushes to `max` more tokens (and the overall budget).
    fn section(&mut self, max: usize) {
        self.section_end = self.used.saturating_add(max).min(self.budget);
    }

    fn remaining(&self) -> usize {
        self.section_end.saturating_sub(self.used)
    }

    fn background(&mut self, line: String) {
        self.used += estimate_tokens(&line) + 2;
        self.ctx.background.push(line);
    }

    /// Add a source of at most `max_tokens` (clipped to fit). Returns its number, or `None` when
    /// the section is full.
    fn push(&mut self, mut src: ChatSource, text: &str, max_tokens: usize) -> Option<u32> {
        let cap = max_tokens.min(self.remaining().saturating_sub(12));
        if cap < 40 || text.trim().is_empty() {
            return None;
        }
        let text = fit(text, cap);
        self.used += estimate_tokens(&text) + 12;
        let n = self.ctx.sources.len() as u32 + 1;
        src.n = n;
        self.ctx.sources.push(Source { src, text });
        Some(n)
    }
}

fn source(kind: ChatSourceKind, label: String, meeting: Option<&Meeting>) -> ChatSource {
    ChatSource {
        n: 0,
        kind,
        label,
        meeting_id: meeting.map(|m| m.id.clone()),
        span: None,
        event: None,
    }
}

// ------------------------------------------------------------------------------ store access

/// The latest non-superseded artifacts of a meeting, by kind.
#[derive(Default)]
struct MeetingArts {
    transcript: Option<Artifact>,
    summary: Option<Artifact>,
    action_items: Option<Artifact>,
    enhanced_notes: Option<Artifact>,
    speaker_labels: Option<Artifact>,
    /// Latest output per recipe.
    recipe_outputs: Vec<Artifact>,
}

fn meeting_arts(core: &Core, meeting: &Id) -> Result<MeetingArts> {
    // Latest first (store order).
    let all = core.store().list_artifacts(meeting, None)?;
    let superseded: HashSet<Id> = all.iter().filter_map(|a| a.supersedes.clone()).collect();
    let mut out = MeetingArts::default();
    let mut recipes = HashSet::new();
    for a in all {
        if superseded.contains(&a.id) {
            continue;
        }
        let slot = match a.kind.0.as_str() {
            ArtifactKind::TRANSCRIPT => &mut out.transcript,
            ArtifactKind::SUMMARY => &mut out.summary,
            ArtifactKind::ACTION_ITEMS => &mut out.action_items,
            ArtifactKind::ENHANCED_NOTES => &mut out.enhanced_notes,
            ArtifactKind::SPEAKER_LABELS => &mut out.speaker_labels,
            ArtifactKind::RECIPE_OUTPUT => {
                let rid = a
                    .body
                    .get("recipe_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if recipes.insert(rid) {
                    out.recipe_outputs.push(a);
                }
                continue;
            }
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(a);
        }
    }
    Ok(out)
}

fn decode<T: almond_types::artifact::ArtifactBody>(a: &Option<Artifact>) -> Option<T> {
    a.as_ref().and_then(|a| a.decode().ok())
}

/// The calendar event a meeting is linked to.
fn linked_event(core: &Core, m: &Meeting) -> Option<CalendarEvent> {
    let r = m.calendar_event.as_ref()?;
    core.store()
        .calendar_events(m.started_at - 2 * DAY_MS, m.started_at + 2 * DAY_MS)
        .ok()?
        .into_iter()
        .find(|e| e.source == r.source && e.event_id == r.event_id)
}

fn load_people(core: &Core) -> HashMap<Id, Person> {
    core.records::<Person>(people::COLLECTION)
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect()
}

/// Display name per transcript speaker id: speaker labels (people) first, then names stored in
/// the transcript, "Me" for the mic channel.
fn speaker_names(
    t: &Transcript,
    labels: Option<&SpeakerLabels>,
    people: &HashMap<Id, Person>,
) -> HashMap<String, String> {
    let mut names: HashMap<String, String> = t
        .speakers
        .iter()
        .filter_map(|s| s.name.clone().map(|n| (s.id.0.clone(), n)))
        .collect();
    if let Some(l) = labels {
        for lab in &l.labels {
            if let Some(p) = people.get(&lab.person) {
                let name = if p.is_self {
                    format!("{} (me)", p.name)
                } else {
                    p.name.clone()
                };
                names.insert(lab.speaker.0.clone(), name);
            }
        }
    }
    names.entry("me".into()).or_insert_with(|| "Me".into());
    names
}

fn speaker_of(s: &Segment, names: &HashMap<String, String>) -> String {
    match &s.speaker {
        Some(id) => names.get(&id.0).cloned().unwrap_or_else(|| id.0.clone()),
        None if s.channel == AudioChannel::Mic => "Me".into(),
        None => "Someone".into(),
    }
}

fn line(s: &Segment, names: &HashMap<String, String>) -> String {
    format!(
        "{} {}: {}",
        fmt_ms(s.t0_ms),
        speaker_of(s, names),
        s.text.trim()
    )
}

fn span_of(artifact: &Id, segs: &[&Segment]) -> Option<SpanRef> {
    let first = segs.first()?;
    let last = segs.last()?;
    Some(SpanRef {
        artifact: artifact.clone(),
        segment: first.idx,
        chars: None,
        time_ms: Some((first.t0_ms, last.t1_ms)),
    })
}

/// Everything known about one meeting, read once.
struct MeetingInfo {
    meeting: Meeting,
    arts: MeetingArts,
    transcript: Option<Transcript>,
    names: HashMap<String, String>,
}

impl MeetingInfo {
    fn load(core: &Core, meeting: Meeting, people: &HashMap<Id, Person>) -> Result<Self> {
        let arts = meeting_arts(core, &meeting.id)?;
        let transcript: Option<Transcript> = decode(&arts.transcript);
        let labels: Option<SpeakerLabels> = decode(&arts.speaker_labels);
        let names = transcript
            .as_ref()
            .map(|t| speaker_names(t, labels.as_ref(), people))
            .unwrap_or_default();
        Ok(MeetingInfo {
            meeting,
            arts,
            transcript,
            names,
        })
    }

    /// Title, date, summary (or notes / the start of the transcript) and action items.
    fn digest(&self, core: &Core) -> (ChatSourceKind, String) {
        let mut t = meeting_header(&self.meeting);
        t.push('\n');
        let mut kind = ChatSourceKind::Summary;
        if let Some(s) = decode::<Summary>(&self.arts.summary) {
            t.push_str(&summary_text(&s));
        } else if let Some(n) = decode::<EnhancedNotes>(&self.arts.enhanced_notes) {
            kind = ChatSourceKind::Notes;
            t.push_str(&clip(&ai_notes_text(&n), 1200));
            t.push('\n');
        } else {
            let notes =
                crate::library::notes_text(core, &self.meeting.notes_doc).unwrap_or_default();
            if !notes.trim().is_empty() {
                kind = ChatSourceKind::Notes;
                t.push_str(&format!("The user's notes:\n{}\n", clip(&notes, 1200)));
            } else if let Some(tr) = &self.transcript {
                kind = ChatSourceKind::Transcript;
                t.push_str("Start of the transcript:\n");
                for s in tr.segments.iter().take(12) {
                    t.push_str(&line(s, &self.names));
                    t.push('\n');
                }
            } else {
                t.push_str("(no transcript, summary or notes yet)\n");
            }
        }
        if let Some(a) = decode::<ActionItems>(&self.arts.action_items)
            && !a.items.is_empty()
        {
            t.push_str("Action items:\n");
            t.push_str(&actions_text(&a));
        }
        (kind, t)
    }
}

/// Meetings each person was in: speakers labelled as them, or calendar attendees by email.
fn people_index(
    core: &Core,
    infos: &[MeetingInfo],
    people: &HashMap<Id, Person>,
) -> HashMap<Id, Vec<usize>> {
    let by_email: HashMap<String, Id> = people
        .values()
        .flat_map(|p| {
            p.emails
                .iter()
                .map(move |e| (e.trim().to_lowercase(), p.id.clone()))
        })
        .collect();
    let mut idx: HashMap<Id, Vec<usize>> = HashMap::new();
    for (i, info) in infos.iter().enumerate() {
        let mut here: HashSet<Id> = HashSet::new();
        if let Some(l) = decode::<SpeakerLabels>(&info.arts.speaker_labels) {
            here.extend(l.labels.into_iter().map(|l| l.person));
        }
        if !by_email.is_empty()
            && let Some(ev) = linked_event(core, &info.meeting)
        {
            for a in &ev.attendees {
                if let Some(id) = a
                    .email
                    .as_ref()
                    .and_then(|e| by_email.get(&e.trim().to_lowercase()))
                {
                    here.insert(id.clone());
                }
            }
        }
        for id in here {
            idx.entry(id).or_default().push(i);
        }
    }
    idx
}

fn person_matches_event(p: &Person, e: &CalendarEvent) -> bool {
    e.attendees.iter().any(|a| {
        a.email.as_ref().is_some_and(|m| {
            let m = m.trim().to_lowercase();
            p.emails.iter().any(|pe| pe.trim().to_lowercase() == m)
        })
    })
}

/// People the question names (full name, alias, or a 3+ letter part of the name as a word).
fn mentioned_people<'a>(question: &str, people: &'a HashMap<Id, Person>) -> Vec<&'a Person> {
    let q = fold(question);
    let words: HashSet<&str> = q.split(|c: char| !c.is_alphanumeric()).collect();
    let mut out: Vec<&Person> = people
        .values()
        .filter(|p| !p.is_self)
        .filter(|p| {
            std::iter::once(&p.name).chain(&p.aliases).any(|n| {
                let n = fold(n);
                (n.chars().count() >= 3 && q.contains(&n))
                    || n.split_whitespace()
                        .any(|part| part.chars().count() >= 3 && words.contains(part))
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.truncate(3);
    out
}

/// Original-char index of the first folded match of `kw` in `text`.
fn find_folded(text: &str, kw: &str) -> Option<usize> {
    let mut folded = String::new();
    let mut map = vec![]; // folded byte offset -> original char index
    for (i, c) in text.chars().enumerate() {
        let f = fold(&c.to_string());
        for _ in 0..f.len() {
            map.push(i);
        }
        folded.push_str(&f);
    }
    folded.find(kw).map(|b| map[b])
}

/// About `radius` chars around original char index `at`.
fn excerpt(text: &str, at: usize, radius: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let from = at.saturating_sub(radius);
    let to = (at + radius).min(chars.len());
    let mut s: String = chars[from..to].iter().collect();
    if from > 0 {
        s = format!("…{}", s.trim_start());
    }
    if to < chars.len() {
        s = format!("{}…", s.trim_end());
    }
    s
}

/// Distinct keywords that occur in `text` (folded), and where the first one is.
fn keyword_hits(text: &str, kws: &[String]) -> (usize, Option<usize>) {
    let mut n = 0;
    let mut first = None;
    for k in kws {
        if let Some(at) = find_folded(text, k) {
            n += 1;
            first.get_or_insert(at);
        }
    }
    (n, first)
}

// ------------------------------------------------------------------------------ scopes

/// Token budget for all sources (the prompt adds instructions, history and the question; the
/// model gets `num_ctx` for the total, at most 32k).
pub(crate) const SOURCES_BUDGET: usize = 10_000;

pub(crate) fn build(
    core: &Core,
    scope: &ChatScope,
    question: &str,
    now: UnixMs,
    budget: usize,
) -> Result<Context> {
    let mut b = Builder::new(budget);
    match scope {
        ChatScope::All => build_all(core, question, now, &mut b)?,
        ChatScope::Meeting { meeting_id } => build_meeting(core, meeting_id, question, &mut b)?,
        ChatScope::Person { person_id } => build_person(core, person_id, question, now, &mut b)?,
    }
    Ok(b.ctx)
}

fn local_midnight(now: UnixMs) -> UnixMs {
    let d = local(now).date_naive();
    d.and_hms_opt(0, 0, 0)
        .and_then(|t| Local.from_local_datetime(&t).earliest())
        .map(|t| t.timestamp_millis())
        .unwrap_or(now - DAY_MS / 2)
}

fn build_all(core: &Core, question: &str, now: UnixMs, b: &mut Builder) -> Result<()> {
    let people = load_people(core);
    let recent: Vec<Meeting> = core.store().list_meetings(&MeetingQuery {
        limit: Some(60),
        ..Default::default()
    })?;
    let by_id: HashMap<Id, &Meeting> = recent.iter().map(|m| (m.id.clone(), m)).collect();
    let linked: HashMap<EventRef, &Meeting> = recent
        .iter()
        .filter_map(|m| m.calendar_event.clone().map(|r| (r, m)))
        .collect();

    // 1. Calendar: the rest of today and the next 7 days.
    b.section(2_500);
    let mut events: Vec<CalendarEvent> = core
        .store()
        .calendar_events(local_midnight(now), now + 7 * DAY_MS)?
        .into_iter()
        .filter(|e| e.status != "cancelled")
        .collect();
    events.sort_by_key(|e| e.start);
    if events.is_empty() {
        b.background("The user's calendar has no events from today through the next 7 days (or no calendar is connected).".into());
    }
    for e in events.iter().take(25) {
        let r = EventRef {
            source: e.source.clone(),
            event_id: e.event_id.clone(),
        };
        let m = linked.get(&r).copied();
        let mut src = source(
            ChatSourceKind::CalendarEvent,
            format!(
                "{} · {} {}",
                clip(e.title.trim(), 40),
                fmt_day(e.start),
                fmt_hm(e.start)
            ),
            m,
        );
        src.event = Some(r);
        if b.push(src, &event_text(e, now, m), 220).is_none() {
            break;
        }
    }

    // 2. What the question is about: search hits, notes and recipe outputs that mention it.
    let mut digested: HashSet<Id> = HashSet::new();
    let mut infos: HashMap<Id, MeetingInfo> = HashMap::new();
    let kws = keywords(question, 6);
    b.section(4_500);
    if !kws.is_empty() {
        search_sources(core, &kws, &by_id, &people, &mut infos, &mut digested, b)?;
    }
    for p in mentioned_people(question, &people) {
        person_source(core, p, &recent, &people, b, 500)?;
    }

    // 3. Recent meetings (digests).
    b.section(usize::MAX);
    let mut shown = 0;
    for m in recent.iter().take(12) {
        if shown >= 8 {
            break;
        }
        if digested.contains(&m.id) {
            continue;
        }
        let info = match infos.remove(&m.id) {
            Some(i) => i,
            None => MeetingInfo::load(core, m.clone(), &people)?,
        };
        let (kind, text) = info.digest(core);
        if b.push(source(kind, meeting_label(m), Some(m)), &text, 380)
            .is_none()
        {
            break;
        }
        digested.insert(m.id.clone());
        shown += 1;
    }
    if recent.is_empty() {
        b.background("The user has no recorded meetings or notes yet.".into());
    } else {
        b.background(format!(
            "The user has {} meeting(s) on record; the most recent was {}.",
            if recent.len() >= 60 {
                "60+".to_string()
            } else {
                recent.len().to_string()
            },
            meeting_header(&recent[0])
        ));
    }

    // 4. People catalog (names + last met), when the people feature has records.
    if !people.is_empty() {
        let infos: Vec<MeetingInfo> = recent
            .iter()
            .map(|m| MeetingInfo {
                meeting: m.clone(),
                arts: meeting_arts(core, &m.id).unwrap_or_default(),
                transcript: None,
                names: HashMap::new(),
            })
            .collect();
        let idx = people_index(core, &infos, &people);
        let mut list: Vec<(&Person, Option<UnixMs>)> = people
            .values()
            .filter(|p| !p.is_self)
            .map(|p| {
                let last = idx.get(&p.id).and_then(|v| {
                    v.iter()
                        .map(|&i| infos[i].meeting.started_at)
                        .filter(|&t| t <= now)
                        .max()
                });
                (p, last)
            })
            .collect();
        list.sort_by_key(|(p, last)| (std::cmp::Reverse(*last), p.name.clone()));
        let names: Vec<String> = list
            .iter()
            .take(40)
            .map(|(p, last)| match last {
                Some(t) => format!("{} (last met {})", p.name, fmt_day(*t)),
                None => p.name.clone(),
            })
            .collect();
        if !names.is_empty() {
            b.background(format!("People the user knows: {}.", names.join(", ")));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn search_sources(
    core: &Core,
    kws: &[String],
    by_id: &HashMap<Id, &Meeting>,
    people: &HashMap<Id, Person>,
    infos: &mut HashMap<Id, MeetingInfo>,
    digested: &mut HashSet<Id>,
    b: &mut Builder,
) -> Result<()> {
    // Score (meeting, artifact, kind, segment) by how many keywords hit it; the AND query of all
    // keywords counts extra.
    #[derive(Default)]
    struct Agg {
        score: f32,
        meeting: Id,
        artifact: Option<Id>,
        kind: String,
        segment: Option<u32>,
    }
    let mut agg: HashMap<(Id, Option<Id>, String, Option<u32>), Agg> = HashMap::new();
    let mut queries: Vec<(String, f32)> = kws.iter().map(|k| (k.clone(), 1.0)).collect();
    if kws.len() > 1 {
        queries.push((kws.join(" "), 2.0));
    }
    for (q, w) in &queries {
        for h in core.store().search(q, 40)? {
            let key = (
                h.meeting_id.clone(),
                h.artifact_id.clone(),
                h.kind.clone(),
                h.segment,
            );
            let a = agg.entry(key).or_insert_with(|| Agg {
                meeting: h.meeting_id.clone(),
                artifact: h.artifact_id.clone(),
                kind: h.kind.clone(),
                segment: h.segment,
                ..Default::default()
            });
            a.score += w + h.score.clamp(0.0, 20.0) / 40.0;
        }
    }
    let mut hits: Vec<Agg> = agg.into_values().collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));

    let mut seen_segs: HashSet<(Id, u32)> = HashSet::new();
    let mut added = 0;
    for h in hits {
        if added >= 10 {
            break;
        }
        let meeting = match by_id.get(&h.meeting) {
            Some(m) => (*m).clone(),
            None => match core.store().get_meeting(&h.meeting)? {
                Some(m) => m,
                None => continue,
            },
        };
        if !infos.contains_key(&meeting.id) {
            let info = MeetingInfo::load(core, meeting.clone(), people)?;
            infos.insert(meeting.id.clone(), info);
        }
        let info = &infos[&meeting.id];
        if h.kind == ArtifactKind::TRANSCRIPT {
            let (Some(aid), Some(seg)) = (&h.artifact, h.segment) else {
                continue;
            };
            if seen_segs.contains(&(aid.clone(), seg)) {
                continue;
            }
            // The hit may be in an older (superseded) transcript: read that one.
            let body: Option<Transcript> =
                if info.arts.transcript.as_ref().map(|a| &a.id) == Some(aid) {
                    info.transcript.clone()
                } else {
                    core.store()
                        .get_artifact(aid)?
                        .and_then(|a| a.decode::<Transcript>().ok())
                };
            let Some(body) = body else { continue };
            let Some(pos) = body.segments.iter().position(|s| s.idx == seg) else {
                continue;
            };
            let from = pos.saturating_sub(1);
            let to = (pos + 2).min(body.segments.len());
            let segs: Vec<&Segment> = body.segments[from..to].iter().collect();
            for s in &segs {
                seen_segs.insert((aid.clone(), s.idx));
            }
            let lines: Vec<String> = segs.iter().map(|s| line(s, &info.names)).collect();
            let hit = &body.segments[pos];
            let mut src = source(
                ChatSourceKind::Transcript,
                format!("{} · {}", meeting_label(&meeting), fmt_ms(hit.t0_ms)),
                Some(&meeting),
            );
            src.span = Some(SpanRef {
                artifact: aid.clone(),
                segment: hit.idx,
                chars: None,
                time_ms: Some((hit.t0_ms, hit.t1_ms)),
            });
            let text = format!(
                "From the transcript of {}:\n{}",
                meeting_header(&meeting),
                lines.join("\n")
            );
            if b.push(src, &text, 260).is_some() {
                added += 1;
            }
        } else if digested.insert(meeting.id.clone()) {
            let (kind, text) = info.digest(core);
            if b.push(
                source(kind, meeting_label(&meeting), Some(&meeting)),
                &text,
                450,
            )
            .is_some()
            {
                added += 1;
            }
        }
    }

    // User notes and recipe outputs (not in the search index) of recent meetings.
    let strong: Vec<String> = kws
        .iter()
        .filter(|k| k.chars().count() >= 3 || k.chars().any(|c| c.is_ascii_digit()))
        .cloned()
        .collect();
    if strong.is_empty() {
        return Ok(());
    }
    let need = strong.len().min(2);
    let mut extra = 0;
    let mut recent: Vec<&Meeting> = by_id.values().copied().collect();
    recent.sort_by_key(|m| std::cmp::Reverse(m.started_at));
    for m in recent.into_iter().take(30) {
        if extra >= 4 {
            break;
        }
        let notes = crate::library::notes_text(core, &m.notes_doc).unwrap_or_default();
        if let (n, Some(at)) = keyword_hits(&notes, &strong)
            && n >= need
        {
            let text = format!(
                "The user's own notes for {}:\n{}",
                meeting_header(m),
                excerpt(&notes, at, 450)
            );
            let label = format!("{} · notes", meeting_label(m));
            if b.push(source(ChatSourceKind::Notes, label, Some(m)), &text, 300)
                .is_some()
            {
                extra += 1;
            }
        }
        for a in core
            .store()
            .list_artifacts(&m.id, Some(ArtifactKind::RECIPE_OUTPUT))?
            .into_iter()
            .take(6)
        {
            let Ok(r) = a.decode::<RecipeOutput>() else {
                continue;
            };
            let md = strip_markers(&r.markdown);
            if let (n, Some(at)) = keyword_hits(&md, &strong)
                && n >= need
            {
                let text = format!(
                    "Output of the recipe \"{}\" on {}:\n{}",
                    r.recipe_name,
                    meeting_header(m),
                    excerpt(&md, at, 450)
                );
                let label = format!("{} · {}", meeting_label(m), clip(&r.recipe_name, 24));
                if b.push(
                    source(ChatSourceKind::RecipeOutput, label, Some(m)),
                    &text,
                    300,
                )
                .is_some()
                {
                    extra += 1;
                }
                break;
            }
        }
    }
    Ok(())
}

/// A person card: who they are and the meetings they were in.
fn person_source(
    core: &Core,
    p: &Person,
    meetings: &[Meeting],
    people: &HashMap<Id, Person>,
    b: &mut Builder,
    max_tokens: usize,
) -> Result<Vec<MeetingInfo>> {
    let infos: Vec<MeetingInfo> = meetings
        .iter()
        .map(|m| MeetingInfo::load(core, m.clone(), people))
        .collect::<Result<_>>()?;
    let idx = people_index(core, &infos, people);
    let mine: Vec<usize> = idx.get(&p.id).cloned().unwrap_or_default();
    let mut t = format!("Person: {}", p.name);
    if !p.emails.is_empty() {
        t.push_str(&format!("\nEmail: {}", p.emails.join(", ")));
    }
    if !p.aliases.is_empty() {
        t.push_str(&format!("\nAlso called: {}", p.aliases.join(", ")));
    }
    if !p.notes.trim().is_empty() {
        t.push_str(&format!(
            "\nThe user's notes about them: {}",
            clip(&p.notes, 600)
        ));
    }
    if mine.is_empty() {
        t.push_str("\nNo recorded meetings with them yet.");
    } else {
        t.push_str(&format!("\nMeetings with them ({}):", mine.len()));
        for &i in mine.iter().take(15) {
            t.push_str(&format!("\n- {}", meeting_header(&infos[i].meeting)));
        }
    }
    let src = ChatSource {
        n: 0,
        kind: ChatSourceKind::Person,
        label: p.name.clone(),
        meeting_id: None,
        span: None,
        event: None,
    };
    b.push(src, &t, max_tokens);
    let mut infos = infos;
    let mut out = vec![];
    // Keep the matching meetings, most recent first (the input order).
    for i in mine.into_iter().rev() {
        out.push(infos.swap_remove(i));
    }
    out.sort_by_key(|i| std::cmp::Reverse(i.meeting.started_at));
    Ok(out)
}

fn build_meeting(core: &Core, meeting_id: &Id, question: &str, b: &mut Builder) -> Result<()> {
    let m = core
        .store()
        .get_meeting(meeting_id)?
        .ok_or_else(|| Error::NotFound(format!("meeting {meeting_id}")))?;
    let people = load_people(core);
    let info = MeetingInfo::load(core, m.clone(), &people)?;
    let mut about = format!("This chat is about one meeting: {}.", meeting_header(&m));
    if let Some(ev) = linked_event(core, &m) {
        let att = attendees_text(&ev);
        if !att.is_empty() {
            about.push_str(&format!(" Calendar attendees: {att}."));
        }
    }
    b.background(about);

    if let Some(s) = decode::<Summary>(&info.arts.summary) {
        let text = format!(
            "AI summary of the meeting (generated; may contain mistakes):\n{}",
            summary_text(&s)
        );
        b.push(
            source(
                ChatSourceKind::Summary,
                format!("{} · summary", meeting_label(&m)),
                Some(&m),
            ),
            &text,
            1_500,
        );
    }
    if let Some(a) = decode::<ActionItems>(&info.arts.action_items)
        && !a.items.is_empty()
    {
        let text = format!("Action items:\n{}", actions_text(&a));
        b.push(
            source(
                ChatSourceKind::Summary,
                format!("{} · action items", meeting_label(&m)),
                Some(&m),
            ),
            &text,
            600,
        );
    }
    let notes = crate::library::notes_text(core, &m.notes_doc).unwrap_or_default();
    if !notes.trim().is_empty() {
        b.push(
            source(
                ChatSourceKind::Notes,
                format!("{} · my notes", meeting_label(&m)),
                Some(&m),
            ),
            &format!("The user's own notes:\n{notes}"),
            1_500,
        );
    }
    if info.arts.summary.is_none()
        && let Some(n) = decode::<EnhancedNotes>(&info.arts.enhanced_notes)
    {
        b.push(
            source(
                ChatSourceKind::Notes,
                format!("{} · AI notes", meeting_label(&m)),
                Some(&m),
            ),
            &format!("AI-enhanced notes:\n{}", ai_notes_text(&n)),
            1_200,
        );
    }
    for a in &info.arts.recipe_outputs {
        if let Ok(r) = a.decode::<RecipeOutput>() {
            b.push(
                source(
                    ChatSourceKind::RecipeOutput,
                    clip(&r.recipe_name, 32),
                    Some(&m),
                ),
                &format!(
                    "Output of the recipe \"{}\":\n{}",
                    r.recipe_name,
                    strip_markers(&r.markdown)
                ),
                700,
            );
        }
    }
    match (&info.arts.transcript, &info.transcript) {
        (Some(a), Some(t)) => {
            let kws = keywords(question, 6);
            transcript_chunks(&m, &a.id, t, &info.names, &kws, b);
        }
        _ => b.background("This meeting has no transcript yet.".into()),
    }
    // What was said comes first (the model leans on the first sources); the summary and notes
    // were budgeted before the transcript so they are always there.
    b.ctx
        .sources
        .sort_by_key(|s| s.src.kind != ChatSourceKind::Transcript);
    for (i, s) in b.ctx.sources.iter_mut().enumerate() {
        s.src.n = i as u32 + 1;
    }
    Ok(())
}

/// The transcript as numbered chunks of consecutive lines. When it doesn't fit, half the room
/// goes to chunks mentioning the question's keywords and the rest to evenly spread chunks.
fn transcript_chunks(
    m: &Meeting,
    artifact: &Id,
    t: &Transcript,
    names: &HashMap<String, String>,
    kws: &[String],
    b: &mut Builder,
) {
    const CHUNK_TOKENS: usize = 280;
    let segs: Vec<&Segment> = t
        .segments
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .collect();
    let mut chunks: Vec<(Vec<&Segment>, String, usize)> = vec![];
    let mut cur: Vec<&Segment> = vec![];
    let mut text = String::new();
    for s in segs {
        let l = line(s, names);
        if !cur.is_empty() && estimate_tokens(&text) + estimate_tokens(&l) > CHUNK_TOKENS {
            let tok = estimate_tokens(&text);
            chunks.push((std::mem::take(&mut cur), std::mem::take(&mut text), tok));
        }
        cur.push(s);
        text.push_str(&l);
        text.push('\n');
    }
    if !cur.is_empty() {
        let tok = estimate_tokens(&text);
        chunks.push((cur, text, tok));
    }
    let room = b.remaining();
    let total: usize = chunks.iter().map(|c| c.2 + 14).sum();
    let mut chosen: Vec<usize> = if total <= room {
        (0..chunks.len()).collect()
    } else {
        b.background(format!(
            "The transcript is long ({} lines); only excerpts are included below.",
            t.segments.len()
        ));
        let mut picked: Vec<usize> = vec![];
        let mut used = 0;
        if !kws.is_empty() {
            let mut scored: Vec<(usize, usize)> = chunks
                .iter()
                .enumerate()
                .map(|(i, c)| (keyword_hits(&c.1, kws).0, i))
                .filter(|(n, _)| *n > 0)
                .collect();
            scored.sort_by_key(|(n, i)| (std::cmp::Reverse(*n), *i));
            for (_, i) in scored {
                if used + chunks[i].2 + 14 > room / 2 {
                    break;
                }
                used += chunks[i].2 + 14;
                picked.push(i);
            }
        }
        // Spread the rest evenly over the meeting.
        let left: Vec<usize> = (0..chunks.len()).filter(|i| !picked.contains(i)).collect();
        let fit_n = (room.saturating_sub(used)) / (CHUNK_TOKENS + 14);
        if fit_n > 0 && !left.is_empty() {
            let step = (left.len() as f64 / fit_n as f64).max(1.0);
            let mut x = 0.0;
            while (x as usize) < left.len() {
                let i = left[x as usize];
                if used + chunks[i].2 + 14 > room {
                    break;
                }
                used += chunks[i].2 + 14;
                picked.push(i);
                x += step;
            }
        }
        picked
    };
    chosen.sort_unstable();
    for i in chosen {
        let (segs, text, _) = &chunks[i];
        let mut src = source(
            ChatSourceKind::Transcript,
            format!("{} · {}", clip(title_of(m), 32), fmt_ms(segs[0].t0_ms)),
            Some(m),
        );
        src.span = span_of(artifact, segs);
        if b.push(
            src,
            &format!("Transcript excerpt:\n{text}"),
            CHUNK_TOKENS + 40,
        )
        .is_none()
        {
            break;
        }
    }
}

fn build_person(
    core: &Core,
    person_id: &Id,
    question: &str,
    now: UnixMs,
    b: &mut Builder,
) -> Result<()> {
    let people = load_people(core);
    let p = people
        .get(person_id)
        .ok_or_else(|| Error::NotFound(format!("person {person_id}")))?
        .clone();
    b.background(format!(
        "This chat is about one person: {}{}.",
        p.name,
        if p.is_self {
            " (the user themself)"
        } else {
            ""
        }
    ));
    let meetings = core.store().list_meetings(&MeetingQuery {
        limit: Some(200),
        ..Default::default()
    })?;
    b.section(900);
    let theirs = person_source(core, &p, &meetings, &people, b, 900)?;

    // Upcoming events with them (next 14 days).
    b.section(1_500);
    let mut events: Vec<CalendarEvent> = core
        .store()
        .calendar_events(now, now + 14 * DAY_MS)?
        .into_iter()
        .filter(|e| e.status != "cancelled" && person_matches_event(&p, e))
        .collect();
    events.sort_by_key(|e| e.start);
    for e in events.iter().take(5) {
        let mut src = source(
            ChatSourceKind::CalendarEvent,
            format!(
                "{} · {} {}",
                clip(e.title.trim(), 40),
                fmt_day(e.start),
                fmt_hm(e.start)
            ),
            None,
        );
        src.event = Some(EventRef {
            source: e.source.clone(),
            event_id: e.event_id.clone(),
        });
        b.push(src, &event_text(e, now, None), 220);
    }

    // Their meetings: digest + what they said (question-relevant lines first).
    b.section(usize::MAX);
    let kws = keywords(question, 6);
    for info in theirs.iter().take(6) {
        let m = &info.meeting;
        let (kind, text) = info.digest(core);
        if b.push(source(kind, meeting_label(m), Some(m)), &text, 450)
            .is_none()
        {
            break;
        }
        let (Some(a), Some(t)) = (&info.arts.transcript, &info.transcript) else {
            continue;
        };
        let speakers: HashSet<String> = decode::<SpeakerLabels>(&info.arts.speaker_labels)
            .map(|l| {
                l.labels
                    .into_iter()
                    .filter(|l| l.person == p.id)
                    .map(|l| l.speaker.0)
                    .collect()
            })
            .unwrap_or_default();
        if speakers.is_empty() {
            continue;
        }
        let mut said: Vec<&Segment> = t
            .segments
            .iter()
            .filter(|s| {
                s.speaker
                    .as_ref()
                    .is_some_and(|sp| speakers.contains(&sp.0))
            })
            .collect();
        if !kws.is_empty() {
            // Stable: lines with keywords first, then the rest in order.
            said.sort_by_key(|s| std::cmp::Reverse(keyword_hits(&s.text, &kws).0.min(1)));
        }
        said.truncate(14);
        said.sort_by_key(|s| s.idx);
        if said.is_empty() {
            continue;
        }
        let lines: Vec<String> = said.iter().map(|s| line(s, &info.names)).collect();
        let mut src = source(
            ChatSourceKind::Transcript,
            format!("{} · {}", meeting_label(m), p.name),
            Some(m),
        );
        src.span = span_of(&a.id, &said[..1]);
        let text = format!(
            "What {} said in {}:\n{}",
            p.name,
            meeting_header(m),
            lines.join("\n")
        );
        if b.push(src, &text, 380).is_none() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folded_find_and_excerpt() {
        let t = "Chào mọi người. Ngân sách marketing là 200 triệu đồng.";
        let at = find_folded(t, "ngan sach").unwrap();
        assert_eq!(t.chars().nth(at), Some('N'));
        assert!(excerpt(t, at, 8).contains("Ngân"));
        assert_eq!(keyword_hits(t, &["dong".into(), "xyz".into()]).0, 1);
    }

    #[test]
    fn fit_shrinks() {
        let long = "word ".repeat(2000);
        let f = fit(&long, 100);
        assert!(estimate_tokens(&f) <= 100);
        assert!(f.ends_with('…'));
    }
}
