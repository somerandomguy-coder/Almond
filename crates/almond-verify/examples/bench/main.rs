//! Runner for the cross-system benchmark in `eval/verify-bench/`: drives `almond-verify` exactly as
//! the app's `verify.all` does (rule claim extraction + `VerifierRegistry::with_llm`, aggregate
//! `precedence.v1`) over a benchmark JSON and writes one JSON line per scored bullet.
//!
//! ```text
//! cargo run --release -p almond-verify --example bench -- --in bench.json --out almond.jsonl \
//!     [--config symbolic|full] [--model gemma4:12b] [--url http://127.0.0.1:11434] [--cache DIR]
//! ```
//! The input is the `eval/fixtures.json` format (one entry per summary *variant*); bullets with
//! an `"item"` id are scored, the rest are context. `--cache` stores judge responses on disk keyed
//! by request, so clean bullets repeated across variants are judged once.

#[path = "../scorecard/ollama.rs"]
mod ollama;

use almond_types::artifact::body::{Transcript, VerdictLabel};
use almond_types::llm::{ChatRequest, ChatResponse, LlmModelInfo, LlmProvider};
use almond_types::verify::VerifyContext;
use almond_verify::eval::{self, FixtureMeeting};
use almond_verify::evidence::units;
use almond_verify::extract::ClaimExtractor;
use almond_verify::{AggregatePolicy, VerifierRegistry, VerifyConfig, aggregate};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Deserialize)]
struct Bench {
    meetings: Vec<BenchMeeting>,
}

#[derive(Deserialize)]
struct BenchMeeting {
    #[serde(flatten)]
    fixture: FixtureMeeting,
    bullets_meta: Vec<Option<String>>,
}

/// On-disk response cache around a provider (deterministic: temperature 0).
struct Cached {
    inner: Arc<dyn LlmProvider>,
    dir: Option<PathBuf>,
    /// (uncached calls, milliseconds spent in them, cache hits)
    stats: Arc<std::sync::Mutex<(u64, u64, u64)>>,
}

impl Cached {
    fn key(req: &ChatRequest) -> String {
        let mut h = DefaultHasher::new();
        req.model.hash(&mut h);
        for m in &req.messages {
            m.content.hash(&mut h);
        }
        req.json_schema.as_ref().map(|s| s.to_string()).hash(&mut h);
        format!("{:016x}", h.finish())
    }
}

#[async_trait]
impl LlmProvider for Cached {
    fn name(&self) -> &str {
        self.inner.name()
    }
    async fn health(&self) -> almond_types::Result<()> {
        self.inner.health().await
    }
    async fn list_models(&self) -> almond_types::Result<Vec<LlmModelInfo>> {
        self.inner.list_models().await
    }
    async fn chat(&self, req: ChatRequest) -> almond_types::Result<ChatResponse> {
        let path = self
            .dir
            .as_ref()
            .map(|d| d.join(format!("{}.json", Self::key(&req))));
        if let Some(p) = &path
            && let Ok(s) = std::fs::read_to_string(p)
        {
            self.stats.lock().unwrap().2 += 1;
            return Ok(ChatResponse {
                content: s,
                model: LlmModelInfo {
                    name: req.model.clone(),
                    digest: None,
                    size_bytes: None,
                    context_length: req.num_ctx,
                },
                prompt_tokens: None,
                completion_tokens: None,
            });
        }
        let t = Instant::now();
        let r = self.inner.chat(req).await?;
        {
            let mut st = self.stats.lock().unwrap();
            st.0 += 1;
            st.1 += t.elapsed().as_millis() as u64;
        }
        if let Some(p) = &path {
            let _ = std::fs::write(p, &r.content);
        }
        Ok(r)
    }
    async fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> almond_types::Result<BoxStream<'static, almond_types::Result<String>>> {
        self.inner.chat_stream(req).await
    }
}

fn label(l: VerdictLabel) -> &'static str {
    match l {
        VerdictLabel::Supported => "supported",
        VerdictLabel::Contradicted => "contradicted",
        VerdictLabel::Unverifiable => "unverifiable",
        VerdictLabel::NeedsReview => "needs_review",
    }
}

fn severity(l: VerdictLabel) -> u8 {
    match l {
        VerdictLabel::Supported => 0,
        VerdictLabel::Unverifiable => 1,
        VerdictLabel::NeedsReview => 2,
        VerdictLabel::Contradicted => 3,
    }
}

/// Error score in [0, 1] for AUROC: label severity, refined by the aggregate's confidence.
fn score(l: VerdictLabel, conf: f32) -> f64 {
    let c = f64::from(conf.clamp(0.0, 1.0));
    match l {
        VerdictLabel::Supported => 0.25 * (1.0 - c),
        VerdictLabel::Unverifiable => 0.5,
        VerdictLabel::NeedsReview => 0.5 + 0.25 * c,
        VerdictLabel::Contradicted => 0.75 + 0.25 * c,
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let opt = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let input = opt("--in").expect("--in bench.json");
    let output = opt("--out").expect("--out results.jsonl");
    let config = opt("--config").unwrap_or_else(|| "full".into());
    let model = opt("--model").unwrap_or_else(|| "gemma4:12b".into());
    let url = opt("--url").unwrap_or_else(|| "http://127.0.0.1:11434".into());
    let cache = opt("--cache").map(PathBuf::from);
    if let Some(c) = &cache {
        std::fs::create_dir_all(c).expect("cache dir");
    }

    let bench: Bench =
        serde_json::from_str(&std::fs::read_to_string(&input).expect("read input")).expect("parse");
    let stats = Arc::new(std::sync::Mutex::new((0u64, 0u64, 0u64)));
    let llm: Option<Arc<dyn LlmProvider>> = match config.as_str() {
        "symbolic" => None,
        _ => {
            let raw: Arc<dyn LlmProvider> = Arc::new(ollama::Ollama::new(&url));
            raw.health().await.expect("ollama reachable");
            Some(Arc::new(Cached {
                inner: raw,
                dir: cache.clone(),
                stats: stats.clone(),
            }))
        }
    };
    let policy = AggregatePolicy::default();
    let extractor = ClaimExtractor::rules();
    let mut out = std::fs::File::create(&output).expect("create output");
    let n = bench.meetings.len();
    let started = Instant::now();
    let mut symbolic_s = 0.0f64;
    let mut scored_total = 0usize;
    for (mi, bm) in bench.meetings.iter().enumerate() {
        let fm = &bm.fixture;
        let t0 = Instant::now();
        let b = eval::build(fm);
        let cfg = VerifyConfig {
            tz_offset_min: Some(fm.tz_offset_min),
        };
        // Same verifier set as the app's `verify.all`, in the meeting's time zone.
        let registry = match &llm {
            None => VerifierRegistry::symbolic(cfg.clone()),
            Some(l) => VerifierRegistry::with_llm(cfg.clone(), l.clone(), &model),
        };
        let reference =
            almond_verify::dates::reference_day(b.meeting.started_at, cfg.tz_offset_min);
        let us = units(&b.summary).expect("units");
        let people: Vec<String> = fm.speakers.values().cloned().collect();
        let (claims, _) = extractor
            .extract(&us, reference, &people)
            .await
            .expect("extract");
        // Only bullets that are scored in this variant need verdicts; the others are context for
        // cross-claim rules, which `verify.consistency` sees through the full claim list.
        let claims_art = almond_types::ArtifactDraft::new(
            b.meeting.id.clone(),
            &almond_types::artifact::body::Claims {
                claims: claims.clone(),
            },
            almond_types::Provenance {
                processor: almond_verify::extract::NAME.into(),
                processor_version: "bench".into(),
                ..Default::default()
            },
        )
        .expect("encode")
        .into_artifact(
            almond_types::Id::new(),
            almond_types::Hlc::default(),
            almond_types::ids::DeviceId("bench".into()),
        );
        let t: Transcript = b.transcript.decode().expect("transcript");
        let scored: Vec<usize> = bm
            .bullets_meta
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.as_ref().map(|_| i))
            .collect();
        // Judge only scored bullets (the LLM verifier is per-claim); symbolic verifiers get all
        // claims so cross-claim Datalog rules see the whole summary.
        let ctx = VerifyContext {
            meeting: b.meeting.clone(),
            transcript: Some((b.transcript.clone(), t)),
            calendar_event: if fm.attendees.is_empty() {
                None
            } else {
                Some(b.event.clone())
            },
            claims_artifact: claims_art.clone(),
            extra: vec![b.summary.clone()],
            params: serde_json::Value::Null,
        };
        let mut verdicts = Vec::new();
        let mut errors = Vec::new();
        for name in registry.names() {
            let tv = Instant::now();
            let v = registry.get(&name).unwrap();
            let subset: Vec<_> = if name == almond_verify::verifiers::nli::NAME {
                claims
                    .iter()
                    .filter(|c| scored.contains(&(c.source.segment as usize)))
                    .cloned()
                    .collect()
            } else {
                claims.clone()
            };
            match v.verify(&subset, &ctx).await {
                Ok(vs) => verdicts.extend(vs),
                Err(e) => errors.push(format!("{name}: {e}")),
            }
            if name != almond_verify::verifiers::nli::NAME {
                symbolic_s += tv.elapsed().as_secs_f64();
            }
        }
        let ids: Vec<String> = claims.iter().map(|c| c.id.clone()).collect();
        let agg = aggregate(&claims_art.id, &ids, &verdicts, &policy);
        let el = t0.elapsed().as_secs_f64();
        scored_total += scored.len();
        for &bi in &scored {
            let item = bm.bullets_meta[bi].clone().unwrap();
            let cids: Vec<&str> = claims
                .iter()
                .filter(|c| c.source.segment as usize == bi)
                .map(|c| c.id.as_str())
                .collect();
            let mut per: BTreeMap<String, (VerdictLabel, f32)> = BTreeMap::new();
            for v in verdicts
                .iter()
                .filter(|v| cids.contains(&v.claim_id.as_str()))
            {
                let e = per
                    .entry(v.verifier.clone())
                    .or_insert((v.label, v.confidence));
                if severity(v.label) > severity(e.0) {
                    *e = (v.label, v.confidence);
                }
            }
            let worst = agg
                .iter()
                .filter(|v| cids.contains(&v.claim_id.as_str()))
                .max_by_key(|v| severity(v.label));
            let (a, conf, fired) = match worst {
                Some(v) => (
                    v.label,
                    v.confidence,
                    v.trace
                        .as_ref()
                        .and_then(|t| t.get("fired"))
                        .cloned()
                        .unwrap_or_default(),
                ),
                None => (VerdictLabel::Unverifiable, 0.0, json!("none")),
            };
            let line = json!({
                "item": item,
                "label": label(a),
                "confidence": conf,
                "score": score(a, conf),
                "fired": fired,
                "claims": cids.len(),
                "verifiers": per.iter().map(|(k, (l, c))| (k.clone(), json!({"label": label(*l), "confidence": c}))).collect::<serde_json::Map<_, _>>(),
                "variant_seconds": el,
                "variant_scored": scored.len(),
                "errors": errors,
            });
            writeln!(out, "{line}").unwrap();
        }
        eprintln!(
            "[{}/{n}] {} scored={} {:.1}s",
            mi + 1,
            fm.id,
            scored.len(),
            el
        );
    }
    let st = *stats.lock().unwrap();
    let meta = json!({
        "config": config, "model": model, "variants": n, "scored": scored_total,
        "wall_s": started.elapsed().as_secs_f64(), "symbolic_s": symbolic_s,
        "judge_calls": st.0, "judge_ms": st.1, "cache_hits": st.2,
    });
    std::fs::write(format!("{output}.meta.json"), meta.to_string()).unwrap();
    eprintln!("{meta}");
}
