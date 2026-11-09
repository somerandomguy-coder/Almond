//! Precision/recall of the verifiers on the hallucination fixtures (`eval/fixtures.json` +
//! held-out `eval/heldout.json`). Output is the Markdown used in `EVAL.md`.
//!
//! ```text
//! cargo run -p almond-verify --example scorecard                     # symbolic only (no LLM)
//! cargo run -p almond-verify --example scorecard -- --ollama         # + verify.nli + LLM extraction
//! cargo run -p almond-verify --example scorecard -- --ollama --model qwen3.5:9b --rows
//! ```
//! Ollama URL: `--url http://localhost:11434` (default).

mod ollama;

use almond_types::llm::LlmProvider;
use almond_verify::eval::{self, Fixtures, Report};
use almond_verify::extract::ClaimExtractor;
use almond_verify::{AggregatePolicy, VerifierRegistry, VerifyConfig};
use std::sync::Arc;

async fn suite(
    set: &str,
    fx: &Fixtures,
    llm: Option<(&Arc<dyn LlmProvider>, &str)>,
    cfg: &VerifyConfig,
) -> Vec<Report> {
    let policy = AggregatePolicy::default();
    let mut reports = vec![
        eval::run(
            &format!("{set} · A. Symbolic verifiers, rule extraction (no LLM)"),
            fx,
            &ClaimExtractor::rules(),
            &VerifierRegistry::symbolic(cfg.clone()),
            &policy,
        )
        .await,
    ];
    if let Some((llm, model)) = llm {
        let with_nli = VerifierRegistry::with_llm(cfg.clone(), llm.clone(), model);
        let mut nli_only = VerifierRegistry::new();
        nli_only.register(with_nli.get("verify.nli").unwrap());
        reports.push(
            eval::run(
                &format!("{set} · B. Symbolic + verify.nli ({model}), rule extraction"),
                fx,
                &ClaimExtractor::rules(),
                &with_nli,
                &policy,
            )
            .await,
        );
        reports.push(
            eval::run(
                &format!("{set} · C. Symbolic + verify.nli, LLM extraction ({model})"),
                fx,
                &ClaimExtractor::llm(llm.clone(), model),
                &with_nli,
                &policy,
            )
            .await,
        );
        reports.push(
            eval::run(
                &format!("{set} · D. verify.nli alone ({model}), rule extraction"),
                fx,
                &ClaimExtractor::rules(),
                &nli_only,
                &policy,
            )
            .await,
        );
    }
    reports
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let opt = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let model = opt("--model").unwrap_or_else(|| "qwen3.5:9b".into());
    let url = opt("--url").unwrap_or_else(|| "http://localhost:11434".into());
    let rows = flag("--rows");

    // All fixtures are recorded in Vietnam time.
    let cfg = VerifyConfig {
        tz_offset_min: Some(420),
    };
    let llm: Option<Arc<dyn LlmProvider>> = if flag("--ollama") {
        let l: Arc<dyn LlmProvider> = Arc::new(ollama::Ollama::new(&url));
        if let Err(e) = l.health().await {
            eprintln!("ollama not reachable at {url}: {e}");
            std::process::exit(1);
        }
        Some(l)
    } else {
        None
    };
    let llm_ref = llm.as_ref().map(|l| (l, model.as_str()));
    let mut reports = suite("Dev set", &eval::load(), llm_ref, &cfg).await;
    reports.extend(suite("Held-out", &eval::load_heldout(), llm_ref, &cfg).await);
    for r in &reports {
        println!("{}", r.to_markdown(rows));
    }
}
