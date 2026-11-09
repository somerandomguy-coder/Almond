//! Resolving `SpanRef`s to text, and the "unit" convention for AI artifacts.
//!
//! **SpanRef.segment convention** (used by claims' `source` and by verdict evidence):
//! - `transcript`: `Segment::idx`
//! - `summary`: flat bullet index across sections, in order (section 0 bullets first)
//! - `enhanced_notes`: block index in `EnhancedNotes::blocks`
//! - `action_items`: item index in `ActionItems::items`

use crate::dates;
use crate::text::{fold, tokenize};
use almond_types::artifact::body::{
    ActionItems, EnhancedNotes, NoteOrigin, Speaker, Summary, Transcript,
};
use almond_types::verify::VerifyContext;
use almond_types::{Artifact, ArtifactKind, Id, SpanRef};
use chrono::NaiveDate;
use std::collections::HashMap;

/// One checkable unit of an AI artifact (summary bullet, AI note block, action item).
#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    pub source: SpanRef,
    /// "summary_bullet" | "note_block" | "action_item"
    pub kind: &'static str,
    pub text: String,
    pub cites: Vec<SpanRef>,
    pub heading: Option<String>,
    pub owner: Option<String>,
    pub due: Option<String>,
}

fn src(a: &Artifact, i: usize) -> SpanRef {
    SpanRef {
        artifact: a.id.clone(),
        segment: i as u32,
        chars: None,
        time_ms: None,
    }
}

/// Split a summary / enhanced_notes / action_items artifact into units. Other kinds -> empty.
pub fn units(a: &Artifact) -> almond_types::Result<Vec<Unit>> {
    let mut out = Vec::new();
    if a.kind.is(ArtifactKind::SUMMARY) {
        let s: Summary = a.decode()?;
        let mut i = 0;
        for sec in &s.sections {
            for b in &sec.bullets {
                out.push(Unit {
                    source: src(a, i),
                    kind: "summary_bullet",
                    text: b.text.clone(),
                    cites: b.cites.clone(),
                    heading: Some(sec.heading.clone()),
                    owner: None,
                    due: None,
                });
                i += 1;
            }
        }
    } else if a.kind.is(ArtifactKind::ENHANCED_NOTES) {
        let n: EnhancedNotes = a.decode()?;
        let mut heading = None;
        for (i, b) in n.blocks.iter().enumerate() {
            if b.style == "heading" {
                heading = Some(b.text.clone());
                continue;
            }
            if b.origin != NoteOrigin::Ai || b.text.trim().is_empty() {
                continue;
            }
            out.push(Unit {
                source: src(a, i),
                kind: "note_block",
                text: b.text.clone(),
                cites: b.cites.clone(),
                heading: heading.clone(),
                owner: None,
                due: None,
            });
        }
    } else if a.kind.is(ArtifactKind::ACTION_ITEMS) {
        let n: ActionItems = a.decode()?;
        for (i, it) in n.items.iter().enumerate() {
            let mut text = it.text.clone();
            if let Some(o) = &it.owner
                && !fold(&text).contains(&fold(o))
            {
                text = format!("{o}: {text}");
            }
            if let Some(d) = &it.due
                && !fold(&text).contains(&fold(d))
            {
                text = format!("{text} ({d})");
            }
            out.push(Unit {
                source: src(a, i),
                kind: "action_item",
                text,
                cites: it.cites.clone(),
                heading: None,
                owner: it.owner.clone(),
                due: it.due.clone(),
            });
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSpan {
    pub span: SpanRef,
    pub text: String,
    pub speaker: Option<String>,
}

/// Index over everything a verifier may cite: transcripts (+ AI artifacts' units), people.
pub struct EvidenceIndex {
    transcripts: HashMap<Id, Transcript>,
    units: HashMap<Id, Vec<Unit>>,
    /// Primary transcript id (ctx.transcript).
    pub primary: Option<Id>,
    /// Known people: speaker names, calendar attendees (names + email local parts).
    pub people: Vec<String>,
    pub attendees: Vec<String>,
    pub reference_day: NaiveDate,
}

impl EvidenceIndex {
    pub fn new(ctx: &VerifyContext, tz_offset_min: Option<i32>) -> Self {
        let mut transcripts = HashMap::new();
        let mut units_map = HashMap::new();
        let mut primary = None;
        if let Some((a, t)) = &ctx.transcript {
            primary = Some(a.id.clone());
            transcripts.insert(a.id.clone(), t.clone());
        }
        for a in &ctx.extra {
            if a.kind.is(ArtifactKind::TRANSCRIPT) {
                if let Ok(t) = a.decode::<Transcript>() {
                    transcripts.entry(a.id.clone()).or_insert(t);
                }
            } else if let Ok(u) = units(a)
                && !u.is_empty()
            {
                units_map.insert(a.id.clone(), u);
            }
        }
        let mut people = Vec::new();
        let mut attendees = Vec::new();
        for t in transcripts.values() {
            for s in &t.speakers {
                if let Some(n) = &s.name {
                    people.push(n.clone());
                }
            }
        }
        if let Some(ev) = &ctx.calendar_event {
            for a in &ev.attendees {
                if let Some(n) = &a.name {
                    attendees.push(n.clone());
                }
                if let Some(e) = &a.email
                    && let Some(local) = e.split('@').next()
                {
                    attendees.push(local.replace(['.', '_'], " "));
                }
            }
            if let Some(o) = &ev.organizer {
                attendees.push(o.split('@').next().unwrap_or(o).replace(['.', '_'], " "));
            }
        }
        people.extend(attendees.iter().cloned());
        people.sort();
        people.dedup();
        EvidenceIndex {
            transcripts,
            units: units_map,
            primary,
            people,
            attendees,
            reference_day: dates::reference_day(ctx.meeting.started_at, tz_offset_min),
        }
    }

    fn speaker_name(t: &Transcript, id: &almond_types::ids::SpeakerId) -> Option<String> {
        t.speakers
            .iter()
            .find(|s: &&Speaker| &s.id == id)
            .and_then(|s| s.name.clone())
    }

    /// Does the span point at something that exists (artifact, segment, char range)?
    pub fn resolve(&self, span: &SpanRef) -> Option<ResolvedSpan> {
        if let Some(t) = self.transcripts.get(&span.artifact) {
            let seg = t.segments.iter().find(|s| s.idx == span.segment)?;
            let text = match span.chars {
                Some((a, b)) => {
                    let n = seg.text.chars().count() as u32;
                    if a >= b || b > n {
                        return None;
                    }
                    crate::text::char_slice(&seg.text, a as usize, b as usize)
                }
                None => seg.text.clone(),
            };
            let speaker = seg
                .speaker
                .as_ref()
                .and_then(|id| Self::speaker_name(t, id));
            return Some(ResolvedSpan {
                span: span.clone(),
                text,
                speaker,
            });
        }
        let u = self
            .units
            .get(&span.artifact)?
            .iter()
            .find(|u| u.source.segment == span.segment)?;
        Some(ResolvedSpan {
            span: span.clone(),
            text: u.text.clone(),
            speaker: None,
        })
    }

    /// Resolve all spans, dropping the ones that don't exist.
    pub fn resolve_all(&self, spans: &[SpanRef]) -> Vec<ResolvedSpan> {
        spans.iter().filter_map(|s| self.resolve(s)).collect()
    }

    /// Every transcript segment (all transcripts), as spans.
    pub fn all_segments(&self) -> Vec<ResolvedSpan> {
        let mut out = Vec::new();
        let mut ids: Vec<&Id> = self.transcripts.keys().collect();
        ids.sort();
        for id in ids {
            let t = &self.transcripts[id];
            for s in &t.segments {
                out.push(ResolvedSpan {
                    span: SpanRef {
                        artifact: id.clone(),
                        segment: s.idx,
                        chars: None,
                        time_ms: Some((s.t0_ms, s.t1_ms)),
                    },
                    text: s.text.clone(),
                    speaker: s.speaker.as_ref().and_then(|sp| Self::speaker_name(t, sp)),
                });
            }
        }
        out
    }

    /// Neighbouring segments of a transcript span (for NLI context), excluding the span itself.
    pub fn neighbours(&self, span: &SpanRef, radius: u32) -> Vec<ResolvedSpan> {
        let Some(t) = self.transcripts.get(&span.artifact) else {
            return vec![];
        };
        t.segments
            .iter()
            .filter(|s| s.idx != span.segment && s.idx.abs_diff(span.segment) <= radius)
            .map(|s| ResolvedSpan {
                span: SpanRef {
                    artifact: span.artifact.clone(),
                    segment: s.idx,
                    chars: None,
                    time_ms: None,
                },
                text: s.text.clone(),
                speaker: s.speaker.as_ref().and_then(|sp| Self::speaker_name(t, sp)),
            })
            .collect()
    }

    pub fn has_transcript(&self, id: &Id) -> bool {
        self.transcripts.contains_key(id) || self.units.contains_key(id)
    }
}

/// Is `name` mentioned in `text`? Folded token-sequence match; a folded-only match (diacritics
/// differ) must be capitalized in `text` so the pronoun "mình" never matches the name "Minh".
pub fn mentions(text: &str, name: &str) -> bool {
    let needle: Vec<(String, String)> = tokenize(name)
        .into_iter()
        .filter(|t| t.kind == crate::text::TokKind::Word)
        .map(|t| (t.lower, t.folded))
        .collect();
    if needle.is_empty() {
        return false;
    }
    let hay = tokenize(text);
    let hay: Vec<_> = hay
        .iter()
        .filter(|t| t.kind == crate::text::TokKind::Word)
        .collect();
    hay.windows(needle.len()).any(|w| {
        w.iter().zip(&needle).all(|(h, (lower, folded))| {
            h.lower == *lower || (h.folded == *folded && h.is_capitalized())
        })
    })
}
