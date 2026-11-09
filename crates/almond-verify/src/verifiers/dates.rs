//! `verify.dates`: dates / deadlines in a claim are normalized against the meeting date ("thứ Sáu"
//! -> 2026-11-13, "trước tháng 12" -> deadline 2026-12) and must agree with the cited evidence.
//! A deadline that falls before the meeting itself is contradicted.

use super::numbers::capitalize;
use super::{AtomStatus, VerifyConfig, label_for, verdict};
use crate::dates::{DateExpr, Match, Qualifier, extract};
use crate::evidence::{EvidenceIndex, ResolvedSpan};
use almond_types::artifact::body::{Claim, Verdict};
use almond_types::verify::{Verifier, VerifierDescriptor, VerifyContext};
use async_trait::async_trait;
use serde_json::json;

pub const NAME: &str = "verify.dates";

#[derive(Debug, Clone, Default)]
pub struct DatesVerifier {
    pub config: VerifyConfig,
}

#[async_trait]
impl Verifier for DatesVerifier {
    fn descriptor(&self) -> VerifierDescriptor {
        VerifierDescriptor {
            name: NAME.into(),
            version: "1".into(),
            family: "symbolic".into(),
            logical_forms: vec![],
            description: "Relative and absolute dates (VI/EN) are resolved against the meeting \
                          date and must match the cited evidence."
                .into(),
        }
    }

    async fn verify(
        &self,
        claims: &[Claim],
        ctx: &VerifyContext,
    ) -> almond_types::Result<Vec<Verdict>> {
        let idx = EvidenceIndex::new(ctx, self.config.tz_offset_min);
        let r = idx.reference_day;
        let all = idx.all_segments();
        let mut out = Vec::new();
        for c in claims {
            let ds = extract(&c.text, r);
            if ds.is_empty() {
                continue;
            }
            let ev = idx.resolve_all(&c.evidence);
            let ev_d: Vec<(&ResolvedSpan, DateExpr)> = ev
                .iter()
                .flat_map(|s| extract(&s.text, r).into_iter().map(move |d| (s, d)))
                .collect();
            let mut statuses = Vec::new();
            let mut checks = Vec::new();
            let mut msgs = Vec::new();
            let mut used = Vec::new();
            for d in &ds {
                let before_meeting = d.qualifier != Qualifier::After && d.value.last_day() < r;
                let best = ev_d.iter().map(|(s, e)| (s, e, d.compare(e))).min_by_key(
                    |(_, _, m)| match m {
                        Match::Exact => 0,
                        Match::Partial => 1,
                        Match::Conflict => 2,
                    },
                );
                let (status, ev_json) = match best {
                    Some((s, e, Match::Exact)) if !before_meeting => {
                        used.push(s.span.clone());
                        (AtomStatus::Match, json!({"span": s.span, "date": e}))
                    }
                    Some((s, e, Match::Partial)) if !before_meeting => {
                        used.push(s.span.clone());
                        msgs.push(format!(
                            "\"{}\" only partially matches \"{}\" (different granularity)",
                            d.surface, e.surface
                        ));
                        (AtomStatus::Partial, json!({"span": s.span, "date": e}))
                    }
                    Some((s, e, _)) => {
                        used.push(s.span.clone());
                        msgs.push(format!(
                            "claim says \"{}\" ({}) but the cited evidence says \"{}\" ({})",
                            d.surface,
                            describe(d),
                            e.surface,
                            describe(e)
                        ));
                        (AtomStatus::Conflict, json!({"span": s.span, "date": e}))
                    }
                    None if before_meeting => {
                        msgs.push(format!(
                            "\"{}\" ({}) is before the meeting date {r}",
                            d.surface,
                            describe(d)
                        ));
                        (AtomStatus::Conflict, json!({"meeting_day": r}))
                    }
                    None => {
                        let elsewhere = all.iter().find(|s| {
                            extract(&s.text, r)
                                .iter()
                                .any(|e| d.compare(e) == Match::Exact)
                        });
                        match elsewhere {
                            Some(s) => {
                                used.push(s.span.clone());
                                msgs.push(format!(
                                    "\"{}\" appears in segment {} which is not cited",
                                    d.surface, s.span.segment
                                ));
                                (AtomStatus::Uncited, json!({"span": s.span}))
                            }
                            None => {
                                msgs.push(format!(
                                    "\"{}\" ({}) is not mentioned in the meeting",
                                    d.surface,
                                    describe(d)
                                ));
                                (AtomStatus::Absent, serde_json::Value::Null)
                            }
                        }
                    }
                };
                statuses.push(status);
                checks.push(json!({"claim": d, "status": status, "evidence": ev_json,
                    "before_meeting": before_meeting}));
            }
            let (label, conf) = label_for(&statuses);
            let explanation = if msgs.is_empty() {
                format!(
                    "Date(s) match the cited evidence: {}.",
                    ds.iter()
                        .map(|d| format!("\"{}\" = {}", d.surface, describe(d)))
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
                Some(json!({"rule": "dates.v1", "meeting_day": r, "checks": checks})),
            ));
        }
        Ok(out)
    }
}

pub(crate) fn describe(d: &DateExpr) -> String {
    use crate::dates::DateVal::*;
    let v = match d.value {
        Day { date } => date.format("%Y-%m-%d (%a)").to_string(),
        Week { monday } => format!("week of {monday}"),
        Month { year, month } => format!("{year}-{month:02}"),
    };
    match d.qualifier {
        Qualifier::On => v,
        Qualifier::Deadline => format!("by {v}"),
        Qualifier::After => format!("after {v}"),
    }
}
