//! Aggregation: many verifiers' verdicts -> one label per claim (`verifier = "verify.aggregate"`).
//!
//! Precedence (`precedence.v1`), first matching rule wins:
//!
//! 1. **R1 strong contradiction** — any `contradicted` with confidence ≥ `contradict_threshold`
//!    → `contradicted`. One solid counter-example beats any amount of support; symbolic checks
//!    (numbers, dates, citations) are precise, so their contradictions are decisive.
//! 2. **R2 weak contradiction** — any other `contradicted` → `needs_review`.
//! 3. **R3 review requested** — any `needs_review` → `needs_review`.
//! 4. **R4 whole-claim judge** — if a *holistic* verifier (default `verify.nli`) ran:
//!    `supported` → `supported`; otherwise, if some atomic check supported a part of the claim →
//!    `needs_review` (parts check out but the claim as a whole is not entailed); else
//!    `unverifiable`.
//! 5. **R5 atomic support** — any `supported` from a non-weak verifier → `supported`
//!    (confidence capped at 0.8: atoms checked, the rest of the claim was not).
//! 6. **R6 weak only** — only weak support (default `verify.citation_exists`: the citation exists,
//!    nothing about its content) → `unverifiable`.
//! 7. **R7 nothing** → `unverifiable`.

use almond_types::Id;
use almond_types::artifact::body::{Verdict, VerdictLabel};
use serde_json::json;
use std::collections::BTreeMap;

pub const NAME: &str = "verify.aggregate";
pub const POLICY: &str = "precedence.v1";

#[derive(Debug, Clone)]
pub struct AggregatePolicy {
    pub contradict_threshold: f32,
    /// Verifiers that judge the whole claim (NLI-style).
    pub holistic: Vec<String>,
    /// Verifiers whose `supported` says nothing about content.
    pub weak: Vec<String>,
}

impl Default for AggregatePolicy {
    fn default() -> Self {
        AggregatePolicy {
            contradict_threshold: 0.7,
            holistic: vec![crate::verifiers::nli::NAME.into()],
            weak: vec![crate::verifiers::citation::NAME.into()],
        }
    }
}

/// One aggregate verdict per claim id that appears in `verdicts` or `claim_ids`.
pub fn aggregate(
    claims_artifact: &Id,
    claim_ids: &[String],
    verdicts: &[Verdict],
    policy: &AggregatePolicy,
) -> Vec<Verdict> {
    let mut by_claim: BTreeMap<&str, Vec<&Verdict>> = BTreeMap::new();
    for id in claim_ids {
        by_claim.entry(id.as_str()).or_default();
    }
    for v in verdicts.iter().filter(|v| v.verifier != NAME) {
        by_claim.entry(v.claim_id.as_str()).or_default().push(v);
    }
    let mut order: Vec<&str> = claim_ids.iter().map(String::as_str).collect();
    for k in by_claim.keys() {
        if !order.contains(k) {
            order.push(k);
        }
    }
    order
        .into_iter()
        .map(|id| aggregate_one(claims_artifact, id, &by_claim[id], policy))
        .collect()
}

fn aggregate_one(
    claims_artifact: &Id,
    claim_id: &str,
    vs: &[&Verdict],
    p: &AggregatePolicy,
) -> Verdict {
    use VerdictLabel::*;
    let is = |v: &&&Verdict, l: VerdictLabel| v.label == l;
    let holistic = |v: &&&Verdict| p.holistic.contains(&v.verifier);
    let weak = |v: &&&Verdict| p.weak.contains(&v.verifier);
    let max_conf = |xs: &[&&Verdict]| xs.iter().map(|v| v.confidence).fold(0.0f32, f32::max);

    let strong_contra: Vec<_> = vs
        .iter()
        .filter(|v| is(v, Contradicted) && v.confidence >= p.contradict_threshold)
        .collect();
    let weak_contra: Vec<_> = vs.iter().filter(|v| is(v, Contradicted)).collect();
    let review: Vec<_> = vs.iter().filter(|v| is(v, NeedsReview)).collect();
    let hol: Vec<_> = vs.iter().filter(holistic).collect();
    let atomic_support: Vec<_> = vs
        .iter()
        .filter(|v| is(v, Supported) && !holistic(v) && !weak(v))
        .collect();
    let weak_support: Vec<_> = vs.iter().filter(|v| is(v, Supported) && weak(v)).collect();

    let (rule, label, conf, decisive): (&str, VerdictLabel, f32, Vec<&&Verdict>) =
        if !strong_contra.is_empty() {
            ("R1", Contradicted, max_conf(&strong_contra), strong_contra)
        } else if !weak_contra.is_empty() {
            ("R2", NeedsReview, max_conf(&weak_contra), weak_contra)
        } else if !review.is_empty() {
            ("R3", NeedsReview, max_conf(&review), review)
        } else if !hol.is_empty() {
            let sup: Vec<_> = hol.iter().copied().filter(|v| is(v, Supported)).collect();
            if !sup.is_empty() {
                ("R4", Supported, max_conf(&sup), sup)
            } else if !atomic_support.is_empty() {
                ("R4", NeedsReview, 0.5, hol)
            } else {
                ("R4", Unverifiable, max_conf(&hol), hol)
            }
        } else if !atomic_support.is_empty() {
            let c = atomic_support
                .iter()
                .map(|v| v.confidence)
                .fold(1.0f32, f32::min)
                .min(0.8);
            ("R5", Supported, c, atomic_support)
        } else if !weak_support.is_empty() {
            ("R6", Unverifiable, 0.3, weak_support)
        } else {
            ("R7", Unverifiable, 0.0, vec![])
        };

    let explanation = match rule {
        "R6" => "Citations exist, but no verifier could check the content.".to_string(),
        "R7" => "No verifier produced a verdict for this claim.".to_string(),
        "R4" if label == NeedsReview => format!(
            "Parts of the claim check out, but the judge did not find it entailed: {}",
            decisive
                .first()
                .map(|v| v.explanation.as_str())
                .unwrap_or("")
        ),
        _ => decisive
            .iter()
            .map(|v| format!("[{}] {}", short(&v.verifier), v.explanation))
            .collect::<Vec<_>>()
            .join(" "),
    };
    let mut evidence = Vec::new();
    for v in &decisive {
        for e in &v.evidence {
            if !evidence.contains(e) {
                evidence.push(e.clone());
            }
        }
    }
    Verdict {
        claims_artifact: claims_artifact.clone(),
        claim_id: claim_id.to_string(),
        verifier: NAME.into(),
        label,
        confidence: conf.clamp(0.0, 1.0),
        evidence,
        explanation,
        trace: Some(json!({
            "rule": "aggregate.v1",
            "policy": POLICY,
            "fired": rule,
            "decisive": decisive.iter().map(|v| &v.verifier).collect::<Vec<_>>(),
            "inputs": vs.iter().map(|v| json!({
                "verifier": v.verifier, "label": v.label, "confidence": v.confidence
            })).collect::<Vec<_>>(),
        })),
    }
}

fn short(name: &str) -> &str {
    name.strip_prefix("verify.").unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(verifier: &str, label: VerdictLabel, conf: f32) -> Verdict {
        Verdict {
            claims_artifact: Id("A".into()),
            claim_id: "c1".into(),
            verifier: verifier.into(),
            label,
            confidence: conf,
            evidence: vec![],
            explanation: verifier.into(),
            trace: None,
        }
    }

    fn agg(vs: &[Verdict]) -> (VerdictLabel, String) {
        let a = aggregate(
            &Id("A".into()),
            &["c1".into()],
            vs,
            &AggregatePolicy::default(),
        );
        let t = a[0].trace.as_ref().unwrap()["fired"]
            .as_str()
            .unwrap()
            .to_string();
        (a[0].label, t)
    }

    #[test]
    fn precedence() {
        use VerdictLabel::*;
        let cite = || v("verify.citation_exists", Supported, 0.3);
        assert_eq!(
            agg(&[
                cite(),
                v("verify.numbers", Contradicted, 0.9),
                v("verify.nli", Supported, 0.9)
            ]),
            (Contradicted, "R1".into())
        );
        assert_eq!(
            agg(&[cite(), v("verify.nli", Contradicted, 0.5)]),
            (NeedsReview, "R2".into())
        );
        assert_eq!(
            agg(&[cite(), v("verify.dates", NeedsReview, 0.6)]),
            (NeedsReview, "R3".into())
        );
        assert_eq!(
            agg(&[cite(), v("verify.nli", Supported, 0.8)]),
            (Supported, "R4".into())
        );
        assert_eq!(
            agg(&[
                cite(),
                v("verify.numbers", Supported, 0.9),
                v("verify.nli", Unverifiable, 0.6)
            ]),
            (NeedsReview, "R4".into())
        );
        assert_eq!(
            agg(&[cite(), v("verify.numbers", Supported, 0.9)]),
            (Supported, "R5".into())
        );
        assert_eq!(agg(&[cite()]), (Unverifiable, "R6".into()));
        assert_eq!(agg(&[]), (Unverifiable, "R7".into()));
    }
}
