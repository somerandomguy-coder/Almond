//! Regression gate on the hallucination fixtures with the deterministic (no-LLM) configuration.
//! Numbers with a real LLM are in EVAL.md (`cargo run -p almond-verify --example scorecard -- --ollama`).

use almond_types::artifact::body::VerdictLabel;
use almond_verify::eval;
use almond_verify::extract::ClaimExtractor;
use almond_verify::{AggregatePolicy, VerifierRegistry, VerifyConfig};

fn cfg() -> VerifyConfig {
    VerifyConfig {
        tz_offset_min: Some(420),
    }
}

#[tokio::test]
async fn symbolic_pipeline_meets_the_plan_bar() {
    for (name, fx) in [("dev", eval::load()), ("heldout", eval::load_heldout())] {
        let r = eval::run(
            name,
            &fx,
            &ClaimExtractor::rules(),
            &VerifierRegistry::symbolic(cfg()),
            &AggregatePolicy::default(),
        )
        .await;
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        // PLAN.md "Done when": >= 80% recall at reasonable precision.
        assert!(
            r.strict.recall() >= 0.8,
            "{name}: recall {}\n{}",
            r.strict.recall(),
            r.to_markdown(true)
        );
        assert!(
            r.strict.precision() >= 0.9,
            "{name}: precision {}\n{}",
            r.strict.precision(),
            r.to_markdown(true)
        );
    }
}

#[tokio::test]
async fn fixture_sizes() {
    let fx = eval::load();
    let bullets: usize = fx.meetings.iter().map(|m| m.bullets.len()).sum();
    let hall = fx
        .meetings
        .iter()
        .flat_map(|m| &m.bullets)
        .filter(|b| b.hallucinated())
        .count();
    assert!(bullets >= 20 && hall >= 10 && bullets - hall >= 10);
}

#[tokio::test]
async fn every_symbolic_contradiction_has_a_trace() {
    let r = eval::run(
        "dev",
        &eval::load(),
        &ClaimExtractor::rules(),
        &VerifierRegistry::symbolic(cfg()),
        &AggregatePolicy::default(),
    )
    .await;
    // Rows only keep labels; re-run one meeting through the registry to inspect traces.
    let fx = eval::load();
    let b = eval::build(&fx.meetings[0]);
    let units = almond_verify::evidence::units(&b.summary).unwrap();
    let (claims, _) = ClaimExtractor::rules()
        .extract(
            &units,
            almond_verify::dates::reference_day(b.meeting.started_at, Some(420)),
            &[],
        )
        .await
        .unwrap();
    let t = b.transcript.decode().unwrap();
    let ctx = almond_types::verify::VerifyContext {
        meeting: b.meeting.clone(),
        transcript: Some((b.transcript.clone(), t)),
        calendar_event: Some(b.event.clone()),
        claims_artifact: b.summary.clone(),
        extra: vec![b.summary.clone()],
        params: serde_json::Value::Null,
    };
    let out = VerifierRegistry::symbolic(cfg())
        .run(&claims, &ctx, None)
        .await;
    let contradictions: Vec<_> = out
        .verdicts
        .iter()
        .filter(|v| v.label == VerdictLabel::Contradicted)
        .collect();
    assert!(!contradictions.is_empty());
    for v in contradictions {
        let tr = v.trace.as_ref().expect("symbolic verdicts carry a trace");
        assert!(tr.get("rule").is_some(), "{tr}");
    }
    assert!(r.rows.len() >= 20);
}
