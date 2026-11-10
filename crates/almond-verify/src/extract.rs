//! Claim extraction: summary bullets / AI note blocks / action items -> atomic `Claim`s with
//! source + evidence `SpanRef`s and (optionally) a `triples.v1` logical form.
//!
//! Two extractors:
//! - `Rules`: one claim per unit, verbatim, heuristic triples. Deterministic; the baseline.
//! - `Llm`: JSON-schema constrained LLM splits units into atomic claims and proposes triples.
//!   Guard rails: a unit the LLM dropped falls back to a verbatim claim; an LLM claim that
//!   introduces a number not present in its unit is discarded (extraction must not hallucinate).

use crate::evidence::{Unit, units};
use crate::llm::{JsonCall, model_ref};
use crate::triples::{self, Triple, TriplesV1, canonical_predicate, literal_from};
use almond_types::artifact::body::{Claim, Claims};
use almond_types::llm::LlmProvider;
use almond_types::pipeline::{JobCtx, Processor, ProcessorDescriptor};
use almond_types::{Artifact, ArtifactDraft, ArtifactKind, Provenance};
use async_trait::async_trait;
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub const NAME: &str = "verify.extract_claims";
pub const PROMPT_ID: &str = "extract_claims.v1";

#[derive(Clone)]
pub enum Extractor {
    Rules,
    Llm {
        llm: Arc<dyn LlmProvider>,
        call: JsonCall,
        /// Units per LLM call.
        batch: usize,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtractStats {
    pub units: usize,
    pub claims: usize,
    /// Units that fell back to a verbatim claim (LLM dropped them / failed).
    pub fallbacks: usize,
    /// LLM claims discarded because they introduced numbers not in the unit.
    pub rejected: usize,
    pub llm_calls: usize,
    pub with_logical_form: usize,
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LlmOut {
    claims: Vec<LlmClaim>,
}

#[derive(Debug, Deserialize)]
struct LlmClaim {
    unit: i64,
    text: String,
    #[serde(default)]
    triples: Vec<LlmTriple>,
}

#[derive(Debug, Deserialize)]
struct LlmTriple {
    s: String,
    p: String,
    o: String,
    #[serde(default)]
    o_type: String,
}

const SYSTEM: &str = "You split AI-written meeting notes into atomic claims for fact-checking.\n\
Input: numbered ITEMS (summary bullets, note lines, action items), Vietnamese and/or English.\n\
For EVERY item output one or more claims. Each claim:\n\
- states exactly ONE checkable fact (a decision, an amount, an owner, a deadline, a count ...);\n\
- is self-contained: replace pronouns (it, they, nó, họ) with the explicit subject;\n\
- stays in the SAME language as the item and copies numbers, names and dates VERBATIM \
(do not convert 'thứ Sáu' or '200tr' into something else);\n\
- adds nothing that is not in the item. An item with a single fact is copied unchanged.\n\
For each claim also give triples (s, p, o, o_type): s = short noun phrase for what the fact is \
about (e.g. 'ngân sách marketing Q4', 'gửi kế hoạch', 'backend hiring'); p is one of owner, \
due, amount, count, percent, decision, status, attendee, date, other; o is copied verbatim from \
the item; o_type is one of person, entity, money, number, percent, date, text. \
Owners: p=owner, o_type=person. Deadlines: p=due, o_type=date, o includes 'trước'/'by'.\n\
Example item 0: \"Lan gửi kế hoạch và báo giá 50 triệu trước thứ Sáu.\" ->\n\
{\"claims\":[{\"unit\":0,\"text\":\"Lan gửi kế hoạch trước thứ Sáu.\",\"triples\":[\
{\"s\":\"gửi kế hoạch\",\"p\":\"owner\",\"o\":\"Lan\",\"o_type\":\"person\"},\
{\"s\":\"gửi kế hoạch\",\"p\":\"due\",\"o\":\"trước thứ Sáu\",\"o_type\":\"date\"}]},\
{\"unit\":0,\"text\":\"Lan gửi báo giá 50 triệu trước thứ Sáu.\",\"triples\":[\
{\"s\":\"báo giá\",\"p\":\"amount\",\"o\":\"50 triệu\",\"o_type\":\"money\"},\
{\"s\":\"gửi báo giá\",\"p\":\"owner\",\"o\":\"Lan\",\"o_type\":\"person\"}]}]}\n\
Output JSON only.";

fn schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "claims": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "unit": {"type": "integer"},
                        "text": {"type": "string"},
                        "triples": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "s": {"type": "string"},
                                    "p": {"type": "string", "enum": ["owner", "due", "amount", "count", "percent", "decision", "status", "attendee", "date", "other"]},
                                    "o": {"type": "string"},
                                    "o_type": {"type": "string", "enum": ["person", "entity", "money", "number", "percent", "date", "text"]}
                                },
                                "required": ["s", "p", "o", "o_type"]
                            }
                        }
                    },
                    "required": ["unit", "text", "triples"]
                }
            }
        },
        "required": ["claims"]
    })
}

pub struct ClaimExtractor {
    pub extractor: Extractor,
    /// Fill `logical_form` with `triples.v1`.
    pub fill_triples: bool,
}

impl ClaimExtractor {
    pub fn rules() -> Self {
        ClaimExtractor {
            extractor: Extractor::Rules,
            fill_triples: true,
        }
    }

    pub fn llm(llm: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        ClaimExtractor {
            extractor: Extractor::Llm {
                llm,
                call: JsonCall::new(model),
                batch: 8,
            },
            fill_triples: true,
        }
    }

    fn verbatim(
        &self,
        u: &Unit,
        reference: NaiveDate,
        people: &[String],
    ) -> (String, Option<TriplesV1>) {
        let t = self.fill_triples.then(|| {
            triples::heuristic(
                &u.text,
                u.owner.as_deref(),
                u.due.as_deref(),
                people,
                reference,
            )
        });
        (u.text.clone(), t)
    }

    /// Extract claims from units. `reference` = meeting day (for date normalization).
    pub async fn extract(
        &self,
        units: &[Unit],
        reference: NaiveDate,
        people: &[String],
    ) -> almond_types::Result<(Vec<Claim>, ExtractStats)> {
        let mut stats = ExtractStats {
            units: units.len(),
            ..Default::default()
        };
        // (unit index, text, triples)
        let mut raw: Vec<(usize, String, Option<TriplesV1>)> = Vec::new();
        match &self.extractor {
            Extractor::Rules => {
                for (i, u) in units.iter().enumerate() {
                    let (t, tr) = self.verbatim(u, reference, people);
                    raw.push((i, t, tr));
                }
            }
            Extractor::Llm { llm, call, batch } => {
                for (chunk_no, chunk) in units.chunks((*batch).max(1)).enumerate() {
                    let base = chunk_no * (*batch).max(1);
                    let mut user = String::from("ITEMS:\n");
                    for (k, u) in chunk.iter().enumerate() {
                        let head = u
                            .heading
                            .as_deref()
                            .map(|h| format!(" (section: {h})"))
                            .unwrap_or_default();
                        user.push_str(&format!("{k}{head}: {}\n", u.text));
                    }
                    stats.llm_calls += 1;
                    let parsed = call
                        .call::<LlmOut>(llm.as_ref(), SYSTEM, &user, schema())
                        .await;
                    let mut got = vec![false; chunk.len()];
                    if let Ok((out, resp)) = parsed {
                        stats.model = Some(model_ref(llm.name(), &resp));
                        for c in out.claims {
                            let Some(k) = usize::try_from(c.unit).ok().filter(|k| *k < chunk.len())
                            else {
                                continue;
                            };
                            let u = &chunk[k];
                            if c.text.trim().is_empty() {
                                continue;
                            }
                            // Guard: no new numbers.
                            let unit_q = crate::numbers::extract(&u.text);
                            let invented = crate::numbers::extract(&c.text)
                                .iter()
                                .any(|q| !unit_q.iter().any(|x| x.matches(q)));
                            if invented {
                                stats.rejected += 1;
                                continue;
                            }
                            got[k] = true;
                            let tr = if self.fill_triples {
                                let ts: Vec<Triple> = c
                                    .triples
                                    .iter()
                                    .filter(|t| !t.s.trim().is_empty() && !t.o.trim().is_empty())
                                    .map(|t| Triple {
                                        s: t.s.clone(),
                                        p: canonical_predicate(&t.p),
                                        o: literal_from(&t.o, &t.o_type, reference),
                                    })
                                    .collect();
                                if ts.is_empty() {
                                    Some(triples::heuristic(
                                        &c.text,
                                        u.owner.as_deref(),
                                        u.due.as_deref(),
                                        people,
                                        reference,
                                    ))
                                } else {
                                    Some(TriplesV1 { triples: ts })
                                }
                            } else {
                                None
                            };
                            raw.push((base + k, c.text.trim().to_string(), tr));
                        }
                    }
                    for (k, ok) in got.iter().enumerate() {
                        if !ok {
                            stats.fallbacks += 1;
                            let (t, tr) = self.verbatim(&chunk[k], reference, people);
                            raw.push((base + k, t, tr));
                        }
                    }
                }
                raw.sort_by_key(|r| r.0);
            }
        }
        let claims: Vec<Claim> = raw
            .into_iter()
            .enumerate()
            .map(|(n, (ui, text, tr))| {
                let u = &units[ui];
                let has = tr.as_ref().is_some_and(|t| !t.triples.is_empty());
                Claim {
                    id: format!("c{}", n + 1),
                    text,
                    source: u.source.clone(),
                    evidence: u.cites.clone(),
                    logical_form_kind: has.then(|| triples::KIND.to_string()),
                    logical_form: tr
                        .filter(|t| !t.triples.is_empty())
                        .map(|t| serde_json::to_value(t).unwrap_or_default()),
                }
            })
            .collect();
        stats.claims = claims.len();
        stats.with_logical_form = claims.iter().filter(|c| c.logical_form.is_some()).count();
        Ok((claims, stats))
    }
}

/// Processor `verify.extract_claims`: summary (+ enhanced_notes, action_items if given) -> claims.
pub struct ExtractClaimsProcessor {
    pub extractor: ClaimExtractor,
    pub consumes: Vec<String>,
    pub tz_offset_min: Option<i32>,
}

impl ExtractClaimsProcessor {
    pub fn new(extractor: ClaimExtractor) -> Self {
        ExtractClaimsProcessor {
            extractor,
            consumes: vec![ArtifactKind::SUMMARY.into()],
            tz_offset_min: None,
        }
    }
}

#[async_trait]
impl Processor for ExtractClaimsProcessor {
    fn descriptor(&self) -> ProcessorDescriptor {
        let (requires, how) = match &self.extractor.extractor {
            Extractor::Rules => (vec![], "one claim per bullet (rules)".to_string()),
            Extractor::Llm { call, .. } => (
                vec![almond_types::job::Capability::new(format!(
                    "llm:{}",
                    call.model
                ))],
                format!("LLM ({}) atomic claims", call.model),
            ),
        };
        ProcessorDescriptor {
            name: NAME.into(),
            version: "1".into(),
            consumes: self.consumes.clone(),
            produces: vec![ArtifactKind::CLAIMS.into()],
            needs_audio: false,
            requires,
            description: format!(
                "Split AI output into atomic, cited claims with triples.v1 logical forms: {how}."
            ),
        }
    }

    async fn run(
        &self,
        ctx: &JobCtx,
        inputs: Vec<Artifact>,
    ) -> almond_types::Result<Vec<ArtifactDraft>> {
        ctx.check_cancel()?;
        let reference = crate::dates::reference_day(
            ctx.meeting.started_at,
            tz_param(ctx).or(self.tz_offset_min),
        );
        let mut all_units = Vec::new();
        let mut used = Vec::new();
        for a in &inputs {
            if self.consumes.iter().any(|k| a.kind.is(k))
                || [ArtifactKind::ENHANCED_NOTES, ArtifactKind::ACTION_ITEMS]
                    .iter()
                    .any(|k| a.kind.is(k))
            {
                let u = units(a)?;
                if !u.is_empty() {
                    used.push(a.id.clone());
                    all_units.extend(u);
                }
            }
        }
        // Known people from transcript speakers, for owner triples.
        let mut people = Vec::new();
        for a in inputs
            .iter()
            .filter(|a| a.kind.is(ArtifactKind::TRANSCRIPT))
        {
            if let Ok(t) = a.decode::<almond_types::artifact::body::Transcript>() {
                people.extend(t.speakers.into_iter().filter_map(|s| s.name));
            }
        }
        (ctx.progress)(0.1, "extracting claims");
        let (claims, stats) = self
            .extractor
            .extract(&all_units, reference, &people)
            .await?;
        ctx.check_cancel()?;
        (ctx.progress)(1.0, "done");
        let draft = ArtifactDraft::new(
            ctx.meeting.id.clone(),
            &Claims { claims },
            Provenance {
                processor: NAME.into(),
                processor_version: "1".into(),
                model: stats.model,
                params_hash: Some(match &self.extractor.extractor {
                    Extractor::Rules => "rules.v1".into(),
                    Extractor::Llm { .. } => PROMPT_ID.into(),
                }),
                inputs: used,
            },
        )?;
        Ok(vec![draft])
    }
}

/// UTC offset (minutes) from the job params (`tz_offset_min` or IANA `tz`) or the meeting's `tz`.
pub(crate) fn tz_param(ctx: &JobCtx) -> Option<i32> {
    crate::tz::offset_for(&ctx.params, &ctx.meeting)
}
