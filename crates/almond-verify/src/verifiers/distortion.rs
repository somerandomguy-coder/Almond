//! `verify.distortion`: meaning shifts that keep every number, date and name intact, so the atom
//! checks can't see them and LLM judges tend to wave them through (eval/verify-bench REPORT §4):
//!
//! - **hedge dropped** — the source was uncertain ("khoảng 50 triệu", "maybe Friday", "I think"),
//!   the claim states it as fact → `needs_review`;
//! - **quantifier changed** — "một số khách hàng" → "tất cả khách hàng", "some" → "all"
//!   → `contradicted`; a claim that generalises ("all", "always", "hầu hết") from a source with no
//!   quantifier → `needs_review`;
//! - **planned → done** — the source says it *will* happen ("sẽ", "will", "plan to"), the claim
//!   says it *did* ("đã", "already", "completed") → `contradicted` (weak);
//! - **negation flipped** — the aligned source passage and the claim disagree on "không/chưa/not"
//!   → `needs_review`.
//!
//! Each claim is first aligned to the best-matching window of its cited segments (content-word
//! overlap); with no good alignment the verifier abstains, and it never emits `supported` — it can
//! only raise concerns. Markers are compared on lowercase words *with* diacritics, so English
//! "may" and Vietnamese "máy" don't collide. Ported idea from SymAI's distortion rules, with VI+EN
//! lexicons ([`LEXICON`]).

use super::{VerifyConfig, verdict};
use crate::evidence::{EvidenceIndex, ResolvedSpan};
use crate::text::{TokKind, tokenize};
use almond_types::artifact::body::{Claim, Verdict, VerdictLabel};
use almond_types::verify::{Verifier, VerifierDescriptor, VerifyContext};
use async_trait::async_trait;
use serde_json::json;

pub const NAME: &str = "verify.distortion";

/// The marker lexicons, lowercase with diacritics, multi-word phrases space-separated.
pub struct Lexicon {
    /// Uncertainty about the whole statement.
    pub hedges: &'static [&'static str],
    /// Approximators: hedges only when followed by a number ("khoảng 50", "about 200").
    pub approx: &'static [&'static str],
    pub universal: &'static [&'static str],
    pub majority: &'static [&'static str],
    pub partial: &'static [&'static str],
    pub none: &'static [&'static str],
    pub future: &'static [&'static str],
    pub past: &'static [&'static str],
    pub negation: &'static [&'static str],
}

pub const LEXICON: Lexicon = Lexicon {
    // Opinion framing ("I think", "nghĩ là", "I believe") is left out on purpose: "I think we should
    // X" summarised as "Lan suggested X" is faithful, and it was the main false-alarm source.
    hedges: &[
        "có lẽ",
        "hình như",
        "chắc là",
        "chắc có lẽ",
        "có khả năng",
        "dự kiến",
        "tạm thời",
        "ước tính",
        "chưa chắc",
        "không chắc",
        "maybe",
        "perhaps",
        "probably",
        "possibly",
        "might",
        "likely",
        "tentatively",
        "tentative",
        "expected to",
        "estimated",
        "not sure",
    ],
    approx: &[
        "khoảng",
        "chừng",
        "tầm",
        "gần",
        "gần như",
        "hơn",
        "trên dưới",
        "about",
        "around",
        "approximately",
        "roughly",
        "nearly",
        "almost",
        "over",
        "under",
        "up to",
    ],
    universal: &[
        "tất cả",
        "toàn bộ",
        "mọi",
        "ai cũng",
        "lúc nào cũng",
        "luôn luôn",
        "hằng ngày",
        "all",
        "every",
        "everyone",
        "everybody",
        "always",
        "entire",
        "whole",
    ],
    majority: &[
        "hầu hết",
        "đa số",
        "phần lớn",
        "nhiều",
        "most",
        "many",
        "majority",
        "usually",
        "mostly",
    ],
    partial: &[
        "một số",
        "một vài",
        "vài",
        "một ít",
        "một phần",
        "some",
        "several",
        "a few",
        "few",
        "part of",
        "sometimes",
        "thỉnh thoảng",
    ],
    none: &[
        "không ai",
        "chẳng ai",
        "không bao giờ",
        "chưa bao giờ",
        "no one",
        "nobody",
        "none",
        "never",
    ],
    future: &[
        "sẽ",
        "dự định",
        "will",
        "going to",
        "plan to",
        "plans to",
        "planned to",
        "intend to",
        "next week",
        "tuần sau",
        "tuần tới",
    ],
    past: &[
        "đã",
        "xong",
        "hoàn thành",
        "already",
        "completed",
        "finished",
        "done",
        "was done",
        "did",
    ],
    negation: &[
        "không", "chưa", "chẳng", "chả", "đừng", "not", "no", "never", "cannot", "didn", "doesn",
        "isn", "aren", "wasn", "weren", "hasn", "haven",
    ],
};

/// Function words ignored when aligning a claim to the evidence.
const STOP: &[&str] = &[
    "là", "của", "và", "thì", "mà", "có", "cho", "với", "được", "các", "những", "này", "đó", "nhé",
    "nha", "à", "ạ", "ừ", "ờ", "the", "a", "an", "of", "to", "and", "is", "are", "was", "were",
    "be", "in", "on", "at", "for", "that", "this", "it", "we", "i", "you", "he", "she", "they",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Quant {
    Universal,
    Majority,
    Partial,
    None,
}

#[derive(Debug, Default)]
struct Features {
    hedges: Vec<String>,
    quant: Vec<(Quant, String)>,
    future: Vec<String>,
    past: Vec<String>,
    negated: bool,
}

/// Lowercase words (diacritics kept) and whether each is a number.
fn words(s: &str) -> Vec<(String, bool)> {
    tokenize(s)
        .into_iter()
        .filter(|t| t.kind != TokKind::Sym || t.raw == "'")
        .map(|t| (t.lower, t.kind == TokKind::Num))
        .collect()
}

const NUMBER_WORDS: &[&str] = &[
    "một", "hai", "ba", "bốn", "tư", "năm", "sáu", "bảy", "tám", "chín", "mười", "mươi", "trăm",
    "nghìn", "ngàn", "triệu", "tỷ", "tỉ", "one", "two", "three", "four", "five", "six", "seven",
    "eight", "nine", "ten", "twenty", "thirty", "fifty", "hundred", "thousand", "million",
    "billion", "half",
];

/// Positions where `phrase` starts in `w`.
fn find(w: &[(String, bool)], phrase: &str) -> Vec<usize> {
    let p: Vec<&str> = phrase.split(' ').collect();
    if p.is_empty() || w.len() < p.len() {
        return vec![];
    }
    (0..=w.len() - p.len())
        .filter(|&i| p.iter().enumerate().all(|(k, x)| w[i + k].0 == *x))
        .collect()
}

fn hits(w: &[(String, bool)], list: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = list
        .iter()
        .filter(|p| !find(w, p).is_empty())
        .map(|p| p.to_string())
        .collect();
    // keep the longest phrases ("không ai" over "không")
    let all = out.clone();
    out.retain(|p| !all.iter().any(|q| q != p && q.contains(p.as_str())));
    out
}

fn features(w: &[(String, bool)]) -> Features {
    let lx = &LEXICON;
    let mut f = Features {
        hedges: hits(w, lx.hedges),
        ..Default::default()
    };
    for a in lx.approx {
        let n = a.split(' ').count();
        if find(w, a).into_iter().any(|i| {
            w.get(i + n)
                .is_some_and(|(x, num)| *num || NUMBER_WORDS.contains(&x.as_str()))
        }) {
            f.hedges.push(a.to_string());
        }
    }
    for (q, list) in [
        (Quant::None, lx.none),
        (Quant::Universal, lx.universal),
        (Quant::Majority, lx.majority),
        (Quant::Partial, lx.partial),
    ] {
        for h in hits(w, list) {
            // "không ai" is also a negation and contains it: one quantifier is enough
            if !f.quant.iter().any(|(_, s)| s.contains(h.as_str())) {
                f.quant.push((q, h));
            }
        }
    }
    f.future = hits(w, lx.future);
    f.past = hits(w, lx.past);
    // Vietnamese yes/no questions end in "không"/"chưa" ("anh đi không"): not a negation.
    let n = w.len();
    let neg_hits = lx
        .negation
        .iter()
        .flat_map(|p| find(w, p))
        .filter(|&i| !(i + 1 == n && matches!(w[i].0.as_str(), "không" | "chưa")));
    f.negated = neg_hits.count() % 2 == 1;
    f
}

fn content(w: &[(String, bool)]) -> Vec<String> {
    let lx = &LEXICON;
    let markers: Vec<&str> = [
        lx.hedges,
        lx.approx,
        lx.universal,
        lx.majority,
        lx.partial,
        lx.none,
        lx.future,
        lx.past,
        lx.negation,
    ]
    .iter()
    .flat_map(|l| l.iter().copied())
    .filter(|p| !p.contains(' '))
    .collect();
    w.iter()
        .map(|(x, _)| crate::text::fold(x))
        .zip(w.iter())
        .filter(|(_, (x, _))| !STOP.contains(&x.as_str()) && !markers.contains(&x.as_str()))
        .map(|(f, _)| f)
        .collect()
}

/// Best window of the cited evidence for this claim: (segment, window words, overlap 0..1).
/// Lowercase words with an is-number flag.
type Words = Vec<(String, bool)>;
/// One concern: label, confidence, message, trace.
type Finding = (VerdictLabel, f32, String, serde_json::Value);

fn align<'a>(
    claim: &[(String, bool)],
    cited: &'a [ResolvedSpan],
) -> Option<(&'a ResolvedSpan, Words, f32)> {
    let cw = content(claim);
    let mut want: Vec<String> = cw.clone();
    want.sort();
    want.dedup();
    if want.len() < 2 {
        return None;
    }
    let size = claim.len() + 8;
    let mut best: Option<(&ResolvedSpan, Words, f32)> = None;
    for r in cited {
        let ew = words(&r.text);
        let step = 2;
        let mut start = 0;
        loop {
            let end = (start + size).min(ew.len());
            let win = &ew[start..end];
            let have = content(win);
            let score = want.iter().filter(|w| have.contains(w)).count() as f32 / want.len() as f32;
            if best.as_ref().is_none_or(|b| score > b.2) {
                best = Some((r, win.to_vec(), score));
            }
            if end == ew.len() {
                break;
            }
            start += step;
        }
    }
    best
}

#[derive(Debug, Clone, Default)]
pub struct DistortionVerifier {
    pub config: VerifyConfig,
}

/// Minimum content-word overlap for the checks to apply at all, and for the negation check.
const MIN_ALIGN: f32 = 0.5;
const NEG_ALIGN: f32 = 0.7;

#[async_trait]
impl Verifier for DistortionVerifier {
    fn descriptor(&self) -> VerifierDescriptor {
        VerifierDescriptor {
            name: NAME.into(),
            version: "1".into(),
            family: "symbolic".into(),
            logical_forms: vec![],
            description: "Meaning shifts against the cited evidence: dropped hedges (khoảng, \
                          maybe), changed quantifiers (một số → tất cả), planned → done, \
                          flipped negation. VI + EN."
                .into(),
        }
    }

    async fn verify(
        &self,
        claims: &[Claim],
        ctx: &VerifyContext,
    ) -> almond_types::Result<Vec<Verdict>> {
        let idx = EvidenceIndex::new(ctx, self.config.tz(ctx));
        let mut out = Vec::new();
        for c in claims {
            let cited = idx.resolve_all(&c.evidence);
            let cw = words(&c.text);
            let Some((seg, win, score)) = align(&cw, &cited) else {
                continue;
            };
            if score < MIN_ALIGN {
                continue;
            }
            let fc = features(&cw);
            let fe = features(&win);
            let mut found: Vec<Finding> = vec![];

            // hedge dropped
            if !fe.hedges.is_empty() && fc.hedges.is_empty() {
                found.push((
                    VerdictLabel::NeedsReview,
                    0.6,
                    format!(
                        "the source is hedged (\"{}\") but the claim states it as fact",
                        fe.hedges.join("\", \"")
                    ),
                    json!({"rule": "hedge_dropped", "source": fe.hedges}),
                ));
            }
            // quantifiers
            let cq: Vec<Quant> = fc.quant.iter().map(|q| q.0).collect();
            let eq: Vec<Quant> = fe.quant.iter().map(|q| q.0).collect();
            if let Some((q, s)) = fc.quant.first() {
                if !eq.is_empty() && !eq.contains(q) {
                    found.push((
                        VerdictLabel::Contradicted,
                        0.75,
                        format!(
                            "the claim says \"{s}\" but the source says \"{}\"",
                            fe.quant[0].1
                        ),
                        json!({"rule": "quantifier_changed", "claim": cq, "source": eq}),
                    ));
                } else if eq.is_empty() && *q == Quant::Universal {
                    found.push((
                        VerdictLabel::NeedsReview,
                        0.55,
                        format!("the claim generalises (\"{s}\"); the source doesn't say so"),
                        json!({"rule": "overgeneralised", "claim": cq}),
                    ));
                }
            }
            // planned → done
            if !fc.past.is_empty()
                && fc.future.is_empty()
                && !fe.future.is_empty()
                && fe.past.is_empty()
            {
                found.push((
                    VerdictLabel::Contradicted,
                    0.65,
                    format!(
                        "the source says it will happen (\"{}\"), the claim says it already did (\"{}\")",
                        fe.future[0], fc.past[0]
                    ),
                    json!({"rule": "planned_as_done", "claim": fc.past, "source": fe.future}),
                ));
            }
            // negation flipped (only on a strong alignment)
            if score >= NEG_ALIGN && fc.negated != fe.negated {
                found.push((
                    VerdictLabel::NeedsReview,
                    0.55,
                    if fc.negated {
                        "the claim is negated but the matching source passage is not".to_string()
                    } else {
                        "the matching source passage is negated but the claim is not".to_string()
                    },
                    json!({"rule": "negation_flipped", "claim_negated": fc.negated}),
                ));
            }
            if found.is_empty() {
                continue;
            }
            let worst = found
                .iter()
                .max_by(|a, b| {
                    let rank = |l: &VerdictLabel| matches!(l, VerdictLabel::Contradicted) as u8;
                    (rank(&a.0), a.1).partial_cmp(&(rank(&b.0), b.1)).unwrap()
                })
                .map(|f| (f.0, f.1))
                .unwrap();
            let explanation = super::numbers::capitalize(
                &found
                    .iter()
                    .map(|f| f.2.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            ) + ".";
            out.push(verdict(
                ctx,
                c,
                NAME,
                worst.0,
                worst.1,
                vec![seg.span.clone()],
                explanation,
                Some(json!({
                    "rule": "distortion.v1",
                    "alignment": score,
                    "findings": found.iter().map(|f| f.3.clone()).collect::<Vec<_>>(),
                })),
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> Features {
        features(&words(s))
    }

    #[test]
    fn markers() {
        assert_eq!(f("Chi phí khoảng 50 triệu mỗi tháng").hedges, ["khoảng"]);
        // "about" is a hedge only before a number
        assert!(f("we talked about the budget").hedges.is_empty());
        assert_eq!(f("about 200 people").hedges, ["about"]);
        assert_eq!(f("Có lẽ thứ Sáu").hedges, ["có lẽ"]);
        // "máy" is not English "may"/anything
        assert!(f("MÁY CHỦ MỚI").hedges.is_empty());
        assert_eq!(f("Một số khách hàng phàn nàn").quant[0].0, Quant::Partial);
        assert_eq!(f("TẤT CẢ KHÁCH HÀNG").quant[0].0, Quant::Universal);
        assert_eq!(f("không ai phản đối").quant[0].0, Quant::None);
        assert!(f("Lan không đi").negated);
        assert!(!f("anh đi họp không").negated, "yes/no question");
        assert!(f("we did not approve it").negated);
        assert!(!f("we approved it").negated);
    }
}
