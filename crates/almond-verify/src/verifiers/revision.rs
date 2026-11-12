//! `verify.revision`: was the claim changed or reversed *later* in the meeting?
//!
//! Every other verifier only looks at the segments a claim cites. A summary that says "launch in
//! December" and cites minute 10 passes all of them even when minute 90 says "no, push it to
//! January". This verifier looks forward:
//!
//! 1. take the transcript segments *after* the last segment the claim cites;
//! 2. keep the ones on the same topic: they share at least [`MIN_SHARED`] content words with the
//!    claim and cover at least [`MIN_COVER`] of them;
//! 3. in the [`TOP_K`] best of those, look for
//!    - a **value change**: a date or amount of the same kind as one in the claim, but different,
//!      and not also restating the claim's value;
//!    - a **reversal marker**: strong ("hoãn", "dời", "huỷ", "đổi", "thôi", "postpone", "cancel",
//!      "instead", "actually", "scratch that") or weak ("không", "chưa", "not", "no", "maybe").
//!
//! | value change | marker | label |
//! |---|---|---|
//! | yes | strong | `contradicted` (0.75) |
//! | yes | weak only | `contradicted` (0.6, so the aggregate shows `needs_review`) |
//! | yes | none | nothing: a different number later is usually another item |
//! | no | strong, close topic match | `needs_review` (0.55) |
//!
//! It is deterministic (no LLM), linear in transcript length (each segment's words are computed
//! once), and only emits verdicts when it finds something: it never says `supported`.

use super::distortion::content_words;
use super::{VerifyConfig, verdict};
use crate::dates::{self, Match};
use crate::evidence::{EvidenceIndex, ResolvedSpan};
use crate::numbers;
use crate::text::{TokKind, tokenize};
use almond_types::artifact::body::{Claim, Verdict, VerdictLabel};
use almond_types::verify::{Verifier, VerifierDescriptor, VerifyContext};
use async_trait::async_trait;
use serde_json::json;

pub const NAME: &str = "verify.revision";

/// Topic match: shared content words (absolute) and share of the claim's content words.
pub const MIN_SHARED: usize = 2;
pub const MIN_COVER: f32 = 0.3;
/// Closer topic match needed when there is only a marker, no changed value.
pub const MARKER_COVER: f32 = 0.5;
/// Later segments examined per claim.
pub const TOP_K: usize = 5;

/// Reversal markers that mean "this changed" on their own (lowercase, diacritics kept).
const STRONG: &[&str] = &[
    "hoãn",
    "dời",
    "lùi",
    "huỷ",
    "hủy",
    "đổi",
    "đổi lại",
    "khoan",
    "thực ra",
    "nhầm",
    "sửa lại",
    "thay vì",
    "để sau",
    "tính lại",
    "bỏ",
    "postpone",
    "postponed",
    "push",
    "pushed",
    "delay",
    "delayed",
    "cancel",
    "cancelled",
    "canceled",
    "instead",
    "actually",
    "scratch that",
    "on second thought",
    "never mind",
    "hold off",
    "change",
    "changed",
    "move",
    "moved",
    "rather",
    "drop",
    "dropped",
    "let s not",
    "decided against",
    "no longer",
    "không làm nữa",
    "không cần nữa",
];
/// Markers that only count together with a changed value ("thôi" is also a filler: "vậy thôi").
const WEAK: &[&str] = &[
    "thôi", "không", "chưa", "chẳng", "no", "not", "maybe", "có lẽ", "nope",
];

fn lower_words(s: &str) -> Vec<String> {
    tokenize(s)
        .into_iter()
        .filter(|t| t.kind == TokKind::Word)
        .map(|t| t.lower)
        .collect()
}

fn markers(w: &[String], list: &[&str]) -> Vec<String> {
    list.iter()
        .filter(|p| {
            let p: Vec<&str> = p.split(' ').collect();
            w.windows(p.len())
                .any(|win| win.iter().zip(&p).all(|(a, b)| a == b))
        })
        .map(|p| p.to_string())
        .collect()
}

/// The claim's date or amount is changed in `later`: the *last* value of the same kind mentioned
/// there differs from the claim's ("tháng 12 không kịp, dời sang tháng 1" → tháng 1), so a
/// passage that repeats the old value on its way to the new one still counts, and one that ends
/// on the claim's value (a reaffirmation) doesn't. Returns (claim value, later value).
fn changed_value(claim: &str, later: &str, idx: &EvidenceIndex) -> Option<(String, String)> {
    let r = idx.reference_day;
    let ld = dates::extract(later, r);
    if let Some(last) = ld.iter().max_by_key(|d| d.span.0) {
        for c in dates::extract(claim, r) {
            if c.compare(last) == Match::Conflict {
                return Some((c.surface, last.surface.clone()));
            }
        }
    }
    let lq = numbers::extract(later);
    for c in numbers::extract(claim) {
        let same_kind = |l: &&numbers::Quantity| {
            c.compatible(l) && std::mem::discriminant(&c.kind) == std::mem::discriminant(&l.kind)
        };
        if let Some(last) = lq.iter().filter(same_kind).max_by_key(|l| l.span.0)
            && !c.same_value(last)
        {
            return Some((c.surface, last.surface.clone()));
        }
    }
    None
}
struct Seg {
    r: ResolvedSpan,
    content: Vec<String>,
    lower: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RevisionVerifier {
    pub config: VerifyConfig,
}

#[async_trait]
impl Verifier for RevisionVerifier {
    fn descriptor(&self) -> VerifierDescriptor {
        VerifierDescriptor {
            name: NAME.into(),
            version: "1".into(),
            family: "symbolic".into(),
            logical_forms: vec![],
            description: "Looks after the cited segments for a later change or reversal on the \
                          same topic (a different date or amount, or 'hoãn / thôi / actually / \
                          instead')."
                .into(),
        }
    }

    async fn verify(
        &self,
        claims: &[Claim],
        ctx: &VerifyContext,
    ) -> almond_types::Result<Vec<Verdict>> {
        let idx = EvidenceIndex::new(ctx, self.config.tz(ctx));
        // Index the transcript once.
        let segs: Vec<Seg> = idx
            .all_segments()
            .into_iter()
            .map(|r| {
                let mut content = content_words(&r.text);
                content.sort();
                content.dedup();
                Seg {
                    lower: lower_words(&r.text),
                    content,
                    r,
                }
            })
            .collect();
        let mut out = Vec::new();
        for c in claims {
            let cited = idx.resolve_all(&c.evidence);
            // The last cited transcript segment, per transcript artifact.
            let Some(last) = cited
                .iter()
                .filter(|r| segs.iter().any(|s| s.r.span.artifact == r.span.artifact))
                .max_by_key(|r| r.span.segment)
            else {
                continue;
            };
            let mut want = content_words(&c.text);
            want.sort();
            want.dedup();
            if want.len() < MIN_SHARED {
                continue;
            }
            let mut cands: Vec<(&Seg, usize, f32)> = segs
                .iter()
                .filter(|s| {
                    s.r.span.artifact == last.span.artifact && s.r.span.segment > last.span.segment
                })
                .map(|s| {
                    let shared = want.iter().filter(|w| s.content.contains(w)).count();
                    (s, shared, shared as f32 / want.len() as f32)
                })
                .filter(|(_, shared, cover)| *shared >= MIN_SHARED && *cover >= MIN_COVER)
                .collect();
            cands.sort_by(|a, b| {
                b.2.total_cmp(&a.2)
                    .then(a.0.r.span.segment.cmp(&b.0.r.span.segment))
            });
            cands.truncate(TOP_K);

            let mut best: Option<(VerdictLabel, f32, String, &Seg, serde_json::Value)> = None;
            for (s, shared, cover) in cands {
                let strong = markers(&s.lower, STRONG);
                let weak = markers(&s.lower, WEAK);
                let changed = changed_value(&c.text, &s.r.text, &idx);
                let at =
                    s.r.span
                        .time_ms
                        .map(|(t0, _)| format!("{}:{:02}", t0 / 60_000, (t0 / 1000) % 60))
                        .unwrap_or_else(|| format!("segment {}", s.r.span.segment));
                let found = match &changed {
                    // A different value alone is common (another motion, another budget line):
                    // it needs a change word in the same passage too.
                    Some((cv, lv)) if !strong.is_empty() || !weak.is_empty() => Some((
                        VerdictLabel::Contradicted,
                        if strong.is_empty() { 0.6 } else { 0.75 },
                        format!(
                            "Later in the meeting ({at}) this changes: \"{cv}\" becomes \"{lv}\""
                        ),
                    )),
                    _ if !strong.is_empty() && cover >= MARKER_COVER => Some((
                        VerdictLabel::NeedsReview,
                        0.55,
                        format!(
                            "Later in the meeting ({at}) the same topic comes up again with \"{}\"; \
                             check it wasn't changed",
                            strong.join("\", \"")
                        ),
                    )),
                    _ => None,
                };
                if let Some((label, conf, why)) = found {
                    let trace = json!({
                        "rule": "revision.v1",
                        "later_segment": s.r.span.segment,
                        "shared_words": shared,
                        "cover": cover,
                        "strong_markers": strong,
                        "weak_markers": weak,
                        "changed": changed,
                    });
                    let better = best.as_ref().is_none_or(|b| {
                        let rank = |l: &VerdictLabel| matches!(l, VerdictLabel::Contradicted) as u8;
                        (rank(&label), conf) > (rank(&b.0), b.1)
                    });
                    if better {
                        best = Some((label, conf, why, s, trace));
                    }
                }
            }
            if let Some((label, conf, why, s, trace)) = best {
                out.push(verdict(
                    ctx,
                    c,
                    NAME,
                    label,
                    conf,
                    vec![s.r.span.clone()],
                    why,
                    Some(trace),
                ));
            }
        }
        Ok(out)
    }
}
