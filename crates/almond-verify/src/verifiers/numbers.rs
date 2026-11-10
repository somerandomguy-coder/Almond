//! `verify.numbers`: every number / money amount / percentage in a claim must appear with the same
//! value in the cited evidence ("300 triệu" vs "200 triệu đồng" -> contradicted).

use super::{AtomStatus, VerifyConfig, label_for, verdict};
use crate::evidence::{EvidenceIndex, ResolvedSpan};
use crate::numbers::{Quantity, extract};
use almond_types::artifact::body::{Claim, Verdict};
use almond_types::verify::{Verifier, VerifierDescriptor, VerifyContext};
use async_trait::async_trait;
use serde_json::json;

pub const NAME: &str = "verify.numbers";

#[derive(Debug, Clone, Default)]
pub struct NumbersVerifier {
    pub config: VerifyConfig,
}

fn q_json(q: &Quantity) -> serde_json::Value {
    serde_json::to_value(q).unwrap_or_default()
}

#[async_trait]
impl Verifier for NumbersVerifier {
    fn descriptor(&self) -> VerifierDescriptor {
        VerifierDescriptor {
            name: NAME.into(),
            version: "1".into(),
            family: "symbolic".into(),
            logical_forms: vec![],
            description: "Numbers, money and percentages in the claim (VI/EN, digits or words) \
                          must appear with the same value in the cited evidence."
                .into(),
        }
    }

    async fn verify(
        &self,
        claims: &[Claim],
        ctx: &VerifyContext,
    ) -> almond_types::Result<Vec<Verdict>> {
        let idx = EvidenceIndex::new(ctx, self.config.tz(ctx));
        let all = idx.all_segments();
        let mut out = Vec::new();
        for c in claims {
            let qs = extract(&c.text);
            if qs.is_empty() {
                continue;
            }
            let ev = idx.resolve_all(&c.evidence);
            let ev_q: Vec<(&ResolvedSpan, Quantity)> = ev
                .iter()
                .flat_map(|r| extract(&r.text).into_iter().map(move |q| (r, q)))
                .collect();
            let mut statuses = Vec::new();
            let mut checks = Vec::new();
            let mut used = Vec::new();
            let mut msgs = Vec::new();
            // Multiset matching: each evidence quantity backs at most one claim quantity, so
            // "two frontend and two designer roles" vs "two frontend and one designer" fails.
            let mut consumed = vec![false; ev_q.len()];
            for q in &qs {
                let free = ev_q
                    .iter()
                    .enumerate()
                    .find(|(k, (_, e))| !consumed[*k] && q.matches(e));
                if let Some((k, (r, e))) = free {
                    consumed[k] = true;
                    statuses.push(AtomStatus::Match);
                    used.push(r.span.clone());
                    checks.push(json!({"claim": q_json(q), "status": AtomStatus::Match,
                        "evidence": {"span": r.span, "quantity": q_json(e)}}));
                    continue;
                }
                if let Some((r, e)) = ev_q.iter().find(|(_, e)| q.matches(e)) {
                    statuses.push(AtomStatus::Conflict);
                    used.push(r.span.clone());
                    msgs.push(format!(
                        "claim uses \"{}\" more often than the cited evidence does",
                        q.surface
                    ));
                    checks.push(json!({"claim": q_json(q), "status": AtomStatus::Conflict,
                        "reason": "already_consumed", "evidence": {"span": r.span, "quantity": q_json(e)}}));
                    continue;
                }
                let conflicting: Vec<_> = ev_q.iter().filter(|(_, e)| q.compatible(e)).collect();
                if !conflicting.is_empty() {
                    statuses.push(AtomStatus::Conflict);
                    let shown: Vec<String> =
                        conflicting.iter().map(|(_, e)| e.surface.clone()).collect();
                    msgs.push(format!(
                        "claim says \"{}\" but the cited evidence says \"{}\"",
                        q.surface,
                        shown.join("\", \"")
                    ));
                    used.extend(conflicting.iter().map(|(r, _)| r.span.clone()));
                    checks.push(json!({"claim": q_json(q), "status": AtomStatus::Conflict,
                        "evidence": conflicting.iter().map(|(r, e)| json!({"span": r.span, "quantity": q_json(e)})).collect::<Vec<_>>()}));
                    continue;
                }
                let elsewhere = all
                    .iter()
                    .find(|r| extract(&r.text).iter().any(|e| q.matches(e)));
                if let Some(r) = elsewhere {
                    statuses.push(AtomStatus::Uncited);
                    msgs.push(format!(
                        "\"{}\" is said in segment {} but that segment is not cited",
                        q.surface, r.span.segment
                    ));
                    used.push(r.span.clone());
                    checks.push(json!({"claim": q_json(q), "status": AtomStatus::Uncited,
                        "evidence": {"span": r.span}}));
                } else {
                    statuses.push(AtomStatus::Absent);
                    msgs.push(format!("\"{}\" is not mentioned in the meeting", q.surface));
                    checks.push(json!({"claim": q_json(q), "status": AtomStatus::Absent}));
                }
            }
            let (label, conf) = label_for(&statuses);
            let explanation = if msgs.is_empty() {
                format!(
                    "All {} number(s) match the cited evidence ({}).",
                    qs.len(),
                    qs.iter()
                        .map(|q| q.surface.as_str())
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
                Some(json!({"rule": "numbers.v1", "checks": checks})),
            ));
        }
        Ok(out)
    }
}

pub(crate) fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
