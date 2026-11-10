//! `verify.nli`: LLM-as-judge entailment. The judge must answer with strict JSON and *quote* the
//! evidence it relied on; a quote that is not actually in the evidence downgrades the verdict to
//! `needs_review` (judges hallucinate too).

use super::VerifyConfig;
use super::verdict;
use crate::evidence::{EvidenceIndex, ResolvedSpan};
use crate::llm::{JsonCall, model_ref};
use crate::text::{TokKind, tokenize};
use almond_types::artifact::body::{Claim, Verdict, VerdictLabel};
use almond_types::llm::LlmProvider;
use almond_types::verify::{Verifier, VerifierDescriptor, VerifyContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub const NAME: &str = "verify.nli";
pub const PROMPT_ID: &str = "nli.v1";

const SYSTEM: &str = "You are a strict fact-checker for meeting notes. You get a CLAIM taken from \
an AI-written meeting summary and EVIDENCE: numbered transcript segments the summary cited \
(plus nearby context segments). Claim and evidence may be in Vietnamese, English or both; \
compare meaning across languages.\n\
Decide:\n\
- \"entailed\": every detail of the claim (numbers, amounts, dates, deadlines, people, owners, \
decisions) is stated or directly implied by the evidence.\n\
- \"contradicted\": some detail conflicts with the evidence (different number, person, date, \
or the evidence says the opposite / something was only proposed, not decided).\n\
- \"neutral\": the evidence does not mention the claim or is not enough to decide.\n\
\"quote\": copy the shortest exact substring of ONE evidence segment that justifies your \
label, verbatim in its original language (empty string if neutral). \"segment\": that segment's \
number (-1 if neutral). Be skeptical: a plausible claim that the evidence does not state is \
neutral. Output JSON only.";

#[derive(Debug, Clone, Deserialize)]
struct JudgeOut {
    #[serde(default)]
    reason: String,
    #[serde(default)]
    quote: String,
    #[serde(default)]
    segment: Option<i64>,
    label: String,
    #[serde(default)]
    confidence: Option<f32>,
}

fn schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "reason": {"type": "string"},
            "quote": {"type": "string"},
            "segment": {"type": "integer"},
            "label": {"type": "string", "enum": ["entailed", "contradicted", "neutral"]},
            "confidence": {"type": "number"}
        },
        "required": ["reason", "quote", "segment", "label", "confidence"]
    })
}

pub struct NliVerifier {
    pub llm: Arc<dyn LlmProvider>,
    pub call: JsonCall,
    /// Include this many neighbouring segments on each side as context.
    pub context_radius: u32,
    pub config: VerifyConfig,
}

impl NliVerifier {
    pub fn new(llm: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        NliVerifier {
            llm,
            call: JsonCall::new(model),
            context_radius: 1,
            config: VerifyConfig::default(),
        }
    }
}

/// Folded word sequence used to check quotes (punctuation/case/diacritics-insensitive).
fn words(s: &str) -> Vec<String> {
    tokenize(s)
        .into_iter()
        .filter(|t| t.kind != TokKind::Sym)
        .map(|t| t.folded)
        .collect()
}

fn quote_in(quote: &str, text: &str) -> bool {
    let q = words(quote);
    !q.is_empty() && crate::text::contains_seq(&words(text), &q)
}

fn render(r: &ResolvedSpan) -> String {
    match &r.speaker {
        Some(s) => format!("[{}] {}: {}", r.span.segment, s, r.text),
        None => format!("[{}] {}", r.span.segment, r.text),
    }
}

#[async_trait]
impl Verifier for NliVerifier {
    fn descriptor(&self) -> VerifierDescriptor {
        VerifierDescriptor {
            name: NAME.into(),
            version: "1".into(),
            family: "neural".into(),
            logical_forms: vec![],
            description: format!(
                "LLM-as-judge entailment ({}) with mandatory verbatim evidence quotes.",
                self.call.model
            ),
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
            let cited = idx.resolve_all(&c.evidence);
            if cited.is_empty() {
                out.push(verdict(
                    ctx,
                    c,
                    NAME,
                    VerdictLabel::Unverifiable,
                    0.5,
                    vec![],
                    "No resolvable evidence to judge against.",
                    None,
                ));
                continue;
            }
            let mut context: Vec<ResolvedSpan> = Vec::new();
            for r in &cited {
                for n in idx.neighbours(&r.span, self.context_radius) {
                    if !cited.iter().any(|x| x.span.segment == n.span.segment)
                        && !context.iter().any(|x| x.span.segment == n.span.segment)
                    {
                        context.push(n);
                    }
                }
            }
            context.sort_by_key(|r| r.span.segment);
            let mut user = format!("CLAIM: {}\n\nEVIDENCE (cited):\n", c.text);
            for r in &cited {
                user.push_str(&render(r));
                user.push('\n');
            }
            if !context.is_empty() {
                user.push_str("\nCONTEXT (nearby, not cited):\n");
                for r in &context {
                    user.push_str(&render(r));
                    user.push('\n');
                }
            }
            let (j, resp) = match self
                .call
                .call::<JudgeOut>(self.llm.as_ref(), SYSTEM, &user, schema())
                .await
            {
                Ok(x) => x,
                Err(e) => {
                    out.push(verdict(
                        ctx,
                        c,
                        NAME,
                        VerdictLabel::NeedsReview,
                        0.2,
                        vec![],
                        format!("Judge failed: {e}"),
                        Some(json!({"prompt": PROMPT_ID, "error": e.to_string()})),
                    ));
                    continue;
                }
            };
            let conf = j.confidence.unwrap_or(0.7).clamp(0.0, 1.0);
            let in_cited = cited.iter().find(|r| quote_in(&j.quote, &r.text));
            let in_context = context.iter().find(|r| quote_in(&j.quote, &r.text));
            let quote_span = in_cited.or(in_context).map(|r| r.span.clone());
            let (label, conf, note) = match j.label.as_str() {
                "entailed" | "contradicted" if quote_span.is_none() => (
                    VerdictLabel::NeedsReview,
                    0.4,
                    " (judge's quote was not found in the evidence)",
                ),
                "entailed" if in_cited.is_none() => (
                    VerdictLabel::NeedsReview,
                    0.5,
                    " (support is only in an uncited neighbouring segment)",
                ),
                "entailed" => (VerdictLabel::Supported, conf, ""),
                "contradicted" => (VerdictLabel::Contradicted, conf, ""),
                _ => (VerdictLabel::Unverifiable, conf, ""),
            };
            let explanation = match label {
                VerdictLabel::Supported | VerdictLabel::Contradicted => {
                    format!("{} — \"{}\"{note}", j.reason.trim(), j.quote.trim())
                }
                _ => format!("{}{note}", j.reason.trim()),
            };
            out.push(verdict(
                ctx,
                c,
                NAME,
                label,
                conf,
                quote_span.clone().into_iter().collect(),
                explanation,
                Some(json!({
                    "prompt": PROMPT_ID,
                    "model": model_ref(self.llm.name(), &resp),
                    "raw_label": j.label,
                    "quote": j.quote,
                    "quote_segment": j.segment,
                    "quote_found": quote_span.is_some(),
                })),
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_matching_is_lenient_on_case_and_diacritics() {
        assert!(quote_in(
            "200 triệu đồng",
            "Ngân sách marketing là 200 triệu đồng."
        ));
        assert!(quote_in(
            "ngan sach marketing",
            "Ngân sách marketing là 200 triệu đồng."
        ));
        assert!(!quote_in(
            "300 triệu",
            "Ngân sách marketing là 200 triệu đồng."
        ));
        assert!(!quote_in("", "anything"));
    }
}
