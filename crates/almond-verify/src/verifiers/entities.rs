//! `verify.entities`: people (owners, speakers) and other named entities in a claim must appear in
//! the cited evidence (or as the evidence's speaker). If the evidence names a *different* person
//! the claim is contradicted (wrong owner); a name that is nowhere in the meeting nor the attendee
//! list is contradicted too.

use super::numbers::capitalize;
use super::{AtomStatus, VerifyConfig, label_for, verdict};
use crate::evidence::{EvidenceIndex, mentions};
use crate::names::{extract, key};
use almond_types::artifact::body::{Claim, Verdict};
use almond_types::verify::{Verifier, VerifierDescriptor, VerifyContext};
use async_trait::async_trait;
use serde_json::json;

pub const NAME: &str = "verify.entities";

#[derive(Debug, Clone, Default)]
pub struct EntitiesVerifier {
    pub config: VerifyConfig,
}

#[async_trait]
impl Verifier for EntitiesVerifier {
    fn descriptor(&self) -> VerifierDescriptor {
        VerifierDescriptor {
            name: NAME.into(),
            version: "1".into(),
            family: "symbolic".into(),
            logical_forms: vec![],
            description: "People and named entities in the claim must appear in the cited \
                          evidence, as its speaker, or (weaker) in the attendee list."
                .into(),
        }
    }

    async fn verify(
        &self,
        claims: &[Claim],
        ctx: &VerifyContext,
    ) -> almond_types::Result<Vec<Verdict>> {
        let idx = EvidenceIndex::new(ctx, self.config.tz_offset_min);
        let all = idx.all_segments();
        // Known people = speakers + attendees + capitalized names said in the transcript.
        let mut known = idx.people.clone();
        for s in &all {
            for n in extract(&s.text, &idx.people) {
                known.push(n.surface);
            }
        }
        known.sort();
        known.dedup();
        let mut out = Vec::new();
        for c in claims {
            let names = extract(&c.text, &known);
            if names.is_empty() {
                continue;
            }
            let ev = idx.resolve_all(&c.evidence);
            // People named in the cited evidence (text mentions + speakers).
            let mut ev_people: Vec<String> = Vec::new();
            for r in &ev {
                ev_people.extend(extract(&r.text, &known).into_iter().map(|n| n.surface));
                if let Some(s) = &r.speaker {
                    ev_people.push(s.clone());
                }
            }
            let mut statuses = Vec::new();
            let mut checks = Vec::new();
            let mut msgs = Vec::new();
            let mut used = Vec::new();
            for n in &names {
                let in_text = ev.iter().find(|r| mentions(&r.text, &n.surface));
                let as_speaker = ev.iter().find(|r| {
                    r.speaker
                        .as_deref()
                        .is_some_and(|s| key(s) == key(&n.surface) || mentions(s, &n.surface))
                });
                if let Some(r) = in_text {
                    used.push(r.span.clone());
                    statuses.push(AtomStatus::Match);
                    checks.push(json!({"name": n.surface, "status": AtomStatus::Match, "evidence": {"span": r.span}}));
                    continue;
                }
                if let Some(r) = as_speaker {
                    used.push(r.span.clone());
                    // The speaker said it, but if the segment names someone else ("Mai sẽ liên
                    // hệ ..." said by Tuấn) that person is the likely actor.
                    let named: Vec<String> = extract(&r.text, &known)
                        .into_iter()
                        .map(|m| m.surface)
                        .filter(|m| {
                            key(m) != key(&n.surface)
                                && idx.people.iter().any(|k| mentions(k, m) || key(k) == key(m))
                        })
                        .collect();
                    let status = if named.is_empty() {
                        AtomStatus::Match
                    } else {
                        msgs.push(format!(
                            "\"{}\" is only the speaker of the cited segment, which names {} as the actor",
                            n.surface,
                            named.join(", ")
                        ));
                        AtomStatus::Uncited
                    };
                    statuses.push(status);
                    checks.push(json!({"name": n.surface, "status": status, "as_speaker": true,
                        "named_in_segment": named, "evidence": {"span": r.span}}));
                    continue;
                }
                let others: Vec<&String> = ev_people
                    .iter()
                    .filter(|p| key(p) != key(&n.surface) && !mentions(p, &n.surface))
                    .filter(|p| {
                        idx.people
                            .iter()
                            .any(|k| mentions(k, p) || key(k) == key(p))
                    })
                    .collect();
                let is_attendee = idx
                    .attendees
                    .iter()
                    .chain(idx.people.iter())
                    .any(|a| mentions(a, &n.surface) || key(a) == key(&n.surface));
                let elsewhere = all.iter().find(|r| mentions(&r.text, &n.surface));
                // Only speakers / attendees are treated as people for the "different person" rule;
                // other capitalized names (products, places) just need to be in the evidence.
                let person = is_attendee;
                let (status, why) = if person && !others.is_empty() {
                    let o: Vec<&str> = others.iter().map(|s| s.as_str()).collect();
                    (
                        AtomStatus::Conflict,
                        format!(
                            "claim names \"{}\" but the cited evidence names {}",
                            n.surface,
                            o.join(", ")
                        ),
                    )
                } else if let Some(r) = elsewhere {
                    used.push(r.span.clone());
                    (
                        AtomStatus::Uncited,
                        format!(
                            "\"{}\" is mentioned in segment {} which is not cited",
                            n.surface, r.span.segment
                        ),
                    )
                } else if is_attendee {
                    (
                        AtomStatus::Absent,
                        format!(
                            "\"{}\" is an attendee but is not mentioned in the evidence",
                            n.surface
                        ),
                    )
                } else {
                    (
                        AtomStatus::Conflict,
                        format!(
                            "\"{}\" is not mentioned in the meeting and is not an attendee",
                            n.surface
                        ),
                    )
                };
                msgs.push(why);
                statuses.push(status);
                checks.push(
                    json!({"name": n.surface, "status": status, "known_person": n.known_person,
                    "is_attendee": is_attendee, "evidence_people": others}),
                );
            }
            let (label, mut conf) = label_for(&statuses);
            if statuses.contains(&AtomStatus::Conflict) {
                conf = 0.8; // name heuristics are noisier than number checks
            }
            let explanation = if msgs.is_empty() {
                format!(
                    "Named entities found in the cited evidence: {}.",
                    names
                        .iter()
                        .map(|n| n.surface.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                capitalize(&msgs.join("; ")) + "."
            };
            used.dedup();
            out.push(verdict(
                ctx,
                c,
                NAME,
                label,
                conf,
                used,
                explanation,
                Some(json!({"rule": "entities.v1", "checks": checks})),
            ));
        }
        Ok(out)
    }
}
