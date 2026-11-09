//! Evaluation harness: hallucination fixtures (`eval/fixtures.json`) -> claims -> verdicts ->
//! precision / recall per verifier and for the aggregate. Used by `examples/eval` (writes the
//! numbers in `EVAL.md`) and by the regression test in `tests/eval.rs`.
//!
//! A *bullet* is flagged by a verifier when any claim extracted from it gets a non-`supported`
//! verdict from that verifier. Positive class = hallucinated bullet.

use crate::aggregate::{self, AggregatePolicy};
use crate::evidence::units;
use crate::extract::{ClaimExtractor, ExtractStats};
use crate::registry::VerifierRegistry;
use almond_types::artifact::body::{
    CitedText, Segment, Speaker, Summary, SummarySection, Transcript, VerdictLabel,
};
use almond_types::calendar::{Attendee, CalendarEvent};
use almond_types::ids::{DeviceId, SpeakerId};
use almond_types::meeting::{AudioChannel, MeetingKind};
use almond_types::verify::VerifyContext;
use almond_types::{Artifact, ArtifactDraft, Hlc, Id, LangTag, Meeting, Provenance, SpanRef};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;

pub const FIXTURES: &str = include_str!("../eval/fixtures.json");

#[derive(Debug, Clone, Deserialize)]
pub struct Fixtures {
    pub meetings: Vec<FixtureMeeting>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureMeeting {
    pub id: String,
    pub title: String,
    /// RFC 3339
    pub start: String,
    pub tz_offset_min: i32,
    pub duration_min: i64,
    pub attendees: Vec<FixtureAttendee>,
    pub speakers: BTreeMap<String, String>,
    /// (speaker, lang, text)
    pub transcript: Vec<(String, String, String)>,
    pub bullets: Vec<FixtureBullet>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureAttendee {
    pub name: String,
    pub email: String,
    pub response: String,
    #[serde(default)]
    pub is_self: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureBullet {
    pub text: String,
    pub cites: Vec<u32>,
    pub gold: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

impl FixtureBullet {
    pub fn hallucinated(&self) -> bool {
        self.gold == "hallucinated"
    }
}

/// Written after tuning on [`FIXTURES`]; never used to tune anything.
pub const HELDOUT: &str = include_str!("../eval/heldout.json");

pub fn load() -> Fixtures {
    serde_json::from_str(FIXTURES).expect("eval/fixtures.json is valid")
}

pub fn load_heldout() -> Fixtures {
    serde_json::from_str(HELDOUT).expect("eval/heldout.json is valid")
}

/// Materialized fixture: everything a verifier needs.
pub struct Built {
    pub meeting: Meeting,
    pub transcript: Artifact,
    pub summary: Artifact,
    pub event: CalendarEvent,
}

fn art<T: almond_types::artifact::ArtifactBody>(meeting: &Id, body: &T, proc_: &str) -> Artifact {
    ArtifactDraft::new(
        meeting.clone(),
        body,
        Provenance {
            processor: proc_.into(),
            processor_version: "eval".into(),
            ..Default::default()
        },
    )
    .expect("encode")
    .into_artifact(Id::new(), Hlc::default(), DeviceId("eval".into()))
}

pub fn build(m: &FixtureMeeting) -> Built {
    let start = chrono::DateTime::parse_from_rfc3339(&m.start)
        .expect("fixture start")
        .timestamp_millis();
    let meeting = Meeting {
        id: Id::new(),
        title: m.title.clone(),
        started_at: start,
        ended_at: Some(start + m.duration_min * 60_000),
        kind: MeetingKind::Online { app: None },
        calendar_event: None,
        languages: vec![LangTag::vi(), LangTag::en()],
        audio: vec![],
        notes_doc: Id::new(),
        tags: vec![],
    };
    let segments = m
        .transcript
        .iter()
        .enumerate()
        .map(|(i, (spk, lang, text))| Segment {
            idx: i as u32,
            t0_ms: i as u32 * 8000,
            t1_ms: i as u32 * 8000 + 7500,
            channel: if spk == "me" {
                AudioChannel::Mic
            } else {
                AudioChannel::System
            },
            speaker: Some(SpeakerId(spk.clone())),
            lang: Some(LangTag(lang.clone())),
            text: text.clone(),
            confidence: Some(0.9),
            words: None,
        })
        .collect();
    let speakers = m
        .speakers
        .iter()
        .map(|(id, name)| Speaker {
            id: SpeakerId(id.clone()),
            name: Some(name.clone()),
            email: None,
            is_me: id == "me",
        })
        .collect();
    let transcript = art(
        &meeting.id,
        &Transcript {
            segments,
            speakers,
        },
        "transcribe.fixture",
    );
    let bullets = m
        .bullets
        .iter()
        .map(|b| CitedText {
            text: b.text.clone(),
            cites: b
                .cites
                .iter()
                .map(|&s| SpanRef {
                    artifact: transcript.id.clone(),
                    segment: s,
                    chars: None,
                    time_ms: None,
                })
                .collect(),
        })
        .collect();
    let summary = art(
        &meeting.id,
        &Summary {
            lang: None,
            sections: vec![SummarySection {
                heading: "Summary".into(),
                bullets,
            }],
        },
        "summarize.fixture",
    );
    let event = CalendarEvent {
        source: "fixture".into(),
        event_id: m.id.clone(),
        calendar_id: "primary".into(),
        calendar_name: None,
        title: m.title.clone(),
        description: None,
        location: None,
        start,
        end: start + m.duration_min * 60_000,
        all_day: false,
        attendees: m
            .attendees
            .iter()
            .map(|a| Attendee {
                email: Some(a.email.clone()),
                name: Some(a.name.clone()),
                response: Some(a.response.clone()),
                is_self: a.is_self,
            })
            .collect(),
        conference_url: None,
        conference_app: None,
        organizer: None,
        status: "confirmed".into(),
        updated: start,
    };
    Built {
        meeting,
        transcript,
        summary,
        event,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Prf {
    pub tp: usize,
    pub fp: usize,
    pub fn_: usize,
    pub tn: usize,
    /// Bullets this system produced any verdict for.
    pub covered: usize,
}

impl Prf {
    pub fn add(&mut self, flagged: bool, positive: bool, covered: bool) {
        match (flagged, positive) {
            (true, true) => self.tp += 1,
            (true, false) => self.fp += 1,
            (false, true) => self.fn_ += 1,
            (false, false) => self.tn += 1,
        }
        self.covered += usize::from(covered);
    }
    pub fn precision(&self) -> f64 {
        ratio(self.tp, self.tp + self.fp)
    }
    pub fn recall(&self) -> f64 {
        ratio(self.tp, self.tp + self.fn_)
    }
    pub fn f1(&self) -> f64 {
        let (p, r) = (self.precision(), self.recall());
        if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        }
    }
    pub fn total(&self) -> usize {
        self.tp + self.fp + self.fn_ + self.tn
    }
}

fn ratio(a: usize, b: usize) -> f64 {
    if b == 0 { 0.0 } else { a as f64 / b as f64 }
}

#[derive(Debug, Clone)]
pub struct BulletResult {
    pub meeting: String,
    pub index: usize,
    pub text: String,
    pub gold: bool,
    pub kind: Option<String>,
    pub aggregate: VerdictLabel,
    /// verifier -> worst label for this bullet
    pub labels: BTreeMap<String, VerdictLabel>,
    pub claims: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub name: String,
    pub per_verifier: BTreeMap<String, Prf>,
    /// Aggregate, flagged = label != supported.
    pub strict: Prf,
    /// Aggregate, flagged = contradicted | needs_review.
    pub alarm: Prf,
    /// Hallucination type -> (caught by strict aggregate, total).
    pub by_type: BTreeMap<String, (usize, usize)>,
    pub rows: Vec<BulletResult>,
    pub extract: ExtractStats,
    pub errors: Vec<(String, String)>,
    pub seconds: f64,
}

fn severity(l: VerdictLabel) -> u8 {
    match l {
        VerdictLabel::Supported => 0,
        VerdictLabel::Unverifiable => 1,
        VerdictLabel::NeedsReview => 2,
        VerdictLabel::Contradicted => 3,
    }
}

/// Run one configuration over all fixtures.
pub async fn run(
    name: &str,
    fixtures: &Fixtures,
    extractor: &ClaimExtractor,
    registry: &VerifierRegistry,
    policy: &AggregatePolicy,
) -> Report {
    let t0 = std::time::Instant::now();
    let mut rep = Report {
        name: name.into(),
        ..Default::default()
    };
    for v in registry.names() {
        rep.per_verifier.entry(v).or_default();
    }
    for fm in &fixtures.meetings {
        let b = build(fm);
        let reference = crate::dates::reference_day(b.meeting.started_at, Some(fm.tz_offset_min));
        let us = units(&b.summary).expect("units");
        let people: Vec<String> = fm.speakers.values().cloned().collect();
        let (claims, st) = extractor
            .extract(&us, reference, &people)
            .await
            .expect("extract");
        rep.extract.units += st.units;
        rep.extract.claims += st.claims;
        rep.extract.fallbacks += st.fallbacks;
        rep.extract.rejected += st.rejected;
        rep.extract.llm_calls += st.llm_calls;
        rep.extract.with_logical_form += st.with_logical_form;
        if st.model.is_some() {
            rep.extract.model = st.model.clone();
        }
        let claims_art = art(
            &b.meeting.id,
            &almond_types::artifact::body::Claims {
                claims: claims.clone(),
            },
            crate::extract::NAME,
        );
        let t: Transcript = b.transcript.decode().expect("transcript");
        let ctx = VerifyContext {
            meeting: b.meeting.clone(),
            transcript: Some((b.transcript.clone(), t)),
            calendar_event: Some(b.event.clone()),
            claims_artifact: claims_art.clone(),
            extra: vec![b.summary.clone()],
        };
        let out = registry.run(&claims, &ctx, None).await;
        rep.errors.extend(out.errors.iter().cloned());
        let ids: Vec<String> = claims.iter().map(|c| c.id.clone()).collect();
        let agg = aggregate::aggregate(&claims_art.id, &ids, &out.verdicts, policy);

        for (bi, bullet) in fm.bullets.iter().enumerate() {
            let cids: Vec<&str> = claims
                .iter()
                .filter(|c| c.source.segment as usize == bi)
                .map(|c| c.id.as_str())
                .collect();
            let mut labels: BTreeMap<String, VerdictLabel> = BTreeMap::new();
            for v in out.verdicts.iter().filter(|v| cids.contains(&v.claim_id.as_str())) {
                let e = labels.entry(v.verifier.clone()).or_insert(v.label);
                if severity(v.label) > severity(*e) {
                    *e = v.label;
                }
            }
            let a = agg
                .iter()
                .filter(|v| cids.contains(&v.claim_id.as_str()))
                .map(|v| v.label)
                .max_by_key(|l| severity(*l))
                .unwrap_or(VerdictLabel::Unverifiable);
            let gold = bullet.hallucinated();
            for (name, prf) in rep.per_verifier.iter_mut() {
                let l = labels.get(name);
                prf.add(
                    l.is_some_and(|l| *l != VerdictLabel::Supported),
                    gold,
                    l.is_some(),
                );
            }
            rep.strict.add(a != VerdictLabel::Supported, gold, true);
            rep.alarm.add(
                matches!(a, VerdictLabel::Contradicted | VerdictLabel::NeedsReview),
                gold,
                true,
            );
            if gold {
                let e = rep
                    .by_type
                    .entry(bullet.kind.clone().unwrap_or_else(|| "other".into()))
                    .or_default();
                e.1 += 1;
                e.0 += usize::from(a != VerdictLabel::Supported);
            }
            rep.rows.push(BulletResult {
                meeting: fm.id.clone(),
                index: bi,
                text: bullet.text.clone(),
                gold,
                kind: bullet.kind.clone(),
                aggregate: a,
                labels,
                claims: claims
                    .iter()
                    .filter(|c| c.source.segment as usize == bi)
                    .map(|c| c.text.clone())
                    .collect(),
            });
        }
    }
    rep.seconds = t0.elapsed().as_secs_f64();
    rep
}

fn label_str(l: VerdictLabel) -> &'static str {
    match l {
        VerdictLabel::Supported => "supported",
        VerdictLabel::Contradicted => "contradicted",
        VerdictLabel::Unverifiable => "unverifiable",
        VerdictLabel::NeedsReview => "needs_review",
    }
}

impl Report {
    /// Markdown section with the metric tables.
    pub fn to_markdown(&self, with_rows: bool) -> String {
        let mut s = String::new();
        let n = self.strict.total();
        let pos = self.strict.tp + self.strict.fn_;
        let _ = writeln!(
            s,
            "### {}\n\n{} bullets ({} hallucinated, {} correct) · {} claims · {:.1}s\n",
            self.name,
            n,
            pos,
            n - pos,
            self.extract.claims,
            self.seconds
        );
        let _ = writeln!(
            s,
            "| system | flagged = | P | R | F1 | TP | FP | FN | coverage |\n|---|---|---|---|---|---|---|---|---|"
        );
        let row = |s: &mut String, name: &str, rule: &str, p: &Prf| {
            let _ = writeln!(
                s,
                "| {name} | {rule} | {:.2} | {:.2} | {:.2} | {} | {} | {} | {}/{} |",
                p.precision(),
                p.recall(),
                p.f1(),
                p.tp,
                p.fp,
                p.fn_,
                p.covered,
                p.total()
            );
        };
        for (name, p) in &self.per_verifier {
            row(&mut s, name, "≠ supported", p);
        }
        row(&mut s, "**aggregate**", "≠ supported", &self.strict);
        row(&mut s, "**aggregate**", "contradicted / needs_review", &self.alarm);
        let _ = writeln!(s, "\nRecall of the aggregate (≠ supported) by injected error:\n");
        let _ = writeln!(s, "| type | caught |\n|---|---|");
        for (k, (c, t)) in &self.by_type {
            let _ = writeln!(s, "| {k} | {c}/{t} |");
        }
        if self.extract.llm_calls > 0 {
            let _ = writeln!(
                s,
                "\nExtraction ({}): {} units -> {} claims, {} LLM calls, {} verbatim fallbacks, {} claims rejected by the number guard, {} with triples.",
                self.extract.model.as_deref().unwrap_or("?"),
                self.extract.units,
                self.extract.claims,
                self.extract.llm_calls,
                self.extract.fallbacks,
                self.extract.rejected,
                self.extract.with_logical_form
            );
        }
        if !self.errors.is_empty() {
            let _ = writeln!(s, "\nVerifier errors: {}", self.errors.len());
        }
        if with_rows {
            let _ = writeln!(
                s,
                "\n<details><summary>Per-bullet results</summary>\n\n| meeting | # | gold | aggregate | flagged by | bullet |\n|---|---|---|---|---|---|"
            );
            for r in &self.rows {
                let by: Vec<String> = r
                    .labels
                    .iter()
                    .filter(|(_, l)| **l != VerdictLabel::Supported)
                    .map(|(k, l)| {
                        format!(
                            "{}:{}",
                            k.trim_start_matches("verify."),
                            &label_str(*l)[..4]
                        )
                    })
                    .collect();
                let gold = if r.gold {
                    r.kind.clone().unwrap_or_else(|| "hall".into())
                } else {
                    "ok".into()
                };
                let mark = if (r.aggregate != VerdictLabel::Supported) == r.gold {
                    ""
                } else {
                    " ✗"
                };
                let _ = writeln!(
                    s,
                    "| {} | {} | {} | {}{} | {} | {} |",
                    r.meeting,
                    r.index,
                    gold,
                    label_str(r.aggregate),
                    mark,
                    by.join(" "),
                    r.text.replace('|', "/")
                );
            }
            let _ = writeln!(s, "\n</details>");
        }
        s
    }
}
