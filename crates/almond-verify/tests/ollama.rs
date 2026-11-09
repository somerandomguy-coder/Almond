//! Real-LLM smoke tests against a local Ollama. Ignored by default; run with
//! `ALMOND_TEST_MODEL=qwen3.5:4b cargo test -p almond-verify --test ollama -- --ignored`.

#[path = "../examples/scorecard/ollama.rs"]
mod ollama;

use almond_types::artifact::body::VerdictLabel;
use almond_types::llm::LlmProvider;
use almond_types::mock::fixtures;
use almond_types::verify::{Verifier, VerifyContext};
use almond_types::DeviceId;
use almond_verify::extract::ClaimExtractor;
use almond_verify::verifiers::NliVerifier;
use std::sync::Arc;

fn model() -> String {
    std::env::var("ALMOND_TEST_MODEL").unwrap_or_else(|_| "qwen3.5:4b".into())
}

#[tokio::test]
#[ignore = "needs a local Ollama with ALMOND_TEST_MODEL pulled"]
async fn nli_catches_300_vs_200_and_llm_extraction_runs() {
    let llm: Arc<dyn LlmProvider> = Arc::new(ollama::Ollama::new("http://localhost:11434"));
    let dev = DeviceId("t".into());
    let m = fixtures::meeting(1_794_189_600_000);
    let t = fixtures::transcript(&m.id, &dev);
    let s = fixtures::summary(&t, &dev);
    let units = almond_verify::evidence::units(&s).unwrap();
    let r = almond_verify::dates::reference_day(m.started_at, Some(420));
    let (claims, stats) = ClaimExtractor::llm(llm.clone(), model())
        .extract(&units, r, &[])
        .await
        .unwrap();
    assert!(!claims.is_empty() && stats.llm_calls == 1, "{stats:?}");
    let ctx = VerifyContext {
        meeting: m,
        transcript: Some((t.clone(), t.decode().unwrap())),
        calendar_event: None,
        claims_artifact: s.clone(),
        extra: vec![],
    };
    let vs = NliVerifier::new(llm, model())
        .verify(&claims, &ctx)
        .await
        .unwrap();
    let first = vs.iter().find(|v| v.claim_id == claims[0].id).unwrap();
    assert_ne!(first.label, VerdictLabel::Supported, "{first:?}");
}
