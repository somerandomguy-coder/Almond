//! `verify.citation_exists`: every cited `SpanRef` must resolve to a real transcript segment
//! (artifact known, segment index present, char range inside the text, time range overlapping).

use super::{VerifyConfig, verdict};
use crate::evidence::EvidenceIndex;
use almond_types::artifact::body::{Claim, Verdict, VerdictLabel};
use almond_types::verify::{Verifier, VerifierDescriptor, VerifyContext};
use async_trait::async_trait;
use serde_json::json;

pub const NAME: &str = "verify.citation_exists";

#[derive(Debug, Clone, Default)]
pub struct CitationExistsVerifier {
    pub config: VerifyConfig,
}

#[async_trait]
impl Verifier for CitationExistsVerifier {
    fn descriptor(&self) -> VerifierDescriptor {
        VerifierDescriptor {
            name: NAME.into(),
            version: "1".into(),
            family: "symbolic".into(),
            logical_forms: vec![],
            description: "Citations resolve to existing transcript segments; uncited claims need \
                          review. Supported here only means 'the citation exists'."
                .into(),
        }
    }

    async fn verify(
        &self,
        claims: &[Claim],
        ctx: &VerifyContext,
    ) -> almond_types::Result<Vec<Verdict>> {
        let idx = EvidenceIndex::new(ctx, self.config.tz_offset_min);
        let mut out = Vec::new();
        for c in claims {
            if c.evidence.is_empty() {
                out.push(verdict(
                    ctx,
                    c,
                    NAME,
                    VerdictLabel::NeedsReview,
                    0.7,
                    vec![],
                    "No citation: the claim is not grounded in the transcript.",
                    Some(json!({"rule": "citation_exists.v1", "checks": [], "reason": "no_citation"})),
                ));
                continue;
            }
            let mut checks = Vec::new();
            let mut broken = Vec::new();
            for s in &c.evidence {
                let status = if !idx.has_transcript(&s.artifact) {
                    "unknown_artifact"
                } else {
                    match idx.resolve(s) {
                        None => {
                            if s.chars.is_some() {
                                let mut whole = s.clone();
                                whole.chars = None;
                                if idx.resolve(&whole).is_some() {
                                    "bad_char_range"
                                } else {
                                    "missing_segment"
                                }
                            } else {
                                "missing_segment"
                            }
                        }
                        Some(_) => match (s.time_ms, segment_time(&idx, s)) {
                            (Some((a, b)), Some((t0, t1))) if b + 1000 < t0 || a > t1 + 1000 => {
                                "time_mismatch"
                            }
                            _ => "ok",
                        },
                    }
                };
                if status != "ok" {
                    broken.push(format!("segment {} ({status})", s.segment));
                }
                checks.push(json!({"span": s, "status": status}));
            }
            let trace = Some(json!({"rule": "citation_exists.v1", "checks": checks}));
            let v = if broken.is_empty() {
                verdict(
                    ctx,
                    c,
                    NAME,
                    VerdictLabel::Supported,
                    0.3,
                    c.evidence.clone(),
                    format!(
                        "All {} citation(s) resolve to transcript segments.",
                        checks.len()
                    ),
                    trace,
                )
            } else {
                verdict(
                    ctx,
                    c,
                    NAME,
                    VerdictLabel::Contradicted,
                    0.9,
                    vec![],
                    format!("Citation does not exist: {}.", broken.join(", ")),
                    trace,
                )
            };
            out.push(v);
        }
        Ok(out)
    }
}

fn segment_time(idx: &EvidenceIndex, s: &almond_types::SpanRef) -> Option<(u32, u32)> {
    idx.all_segments()
        .into_iter()
        .find(|r| r.span.artifact == s.artifact && r.span.segment == s.segment)
        .and_then(|r| r.span.time_ms)
}
