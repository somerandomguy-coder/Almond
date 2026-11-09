//! End-to-end demo with a real Ollama: fixture meeting + transcript -> DAG -> jobs -> summary,
//! enhanced notes, action items and title artifacts (with validated citations).
//!
//! ```text
//! cargo run -p almond-pipeline --example demo_meeting -- [model] [lang]
//! cargo run -p almond-pipeline --example demo_meeting -- qwen3.5:4b en
//! ```
//! Uses `OLLAMA_HOST` if set (default http://127.0.0.1:11434).

use almond_llm::{LlmProcessor, LlmTasks, Ollama, PromptLibrary, TaskConfig};
use almond_pipeline::{JobRunner, PlanPolicy, ProcessorRegistry, RunResult, RunnerConfig};
use almond_types::event::EventBody;
use almond_types::job::Capability;
use almond_types::mock::{MemBlobStore, MemStore, fixtures};
use almond_types::store::Store;
use almond_types::{DeviceId, time::now_ms};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> almond_types::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let model = args
        .first()
        .cloned()
        .unwrap_or_else(almond_llm::tasks::default_model);
    let lang = args.get(1).cloned().unwrap_or_else(|| "auto".into());

    let ollama = Arc::new(Ollama::from_env()?);
    let tasks = LlmTasks::new(
        ollama,
        PromptLibrary::builtin(),
        TaskConfig {
            model: model.clone(),
            ..Default::default()
        },
    );
    let registry = Arc::new(ProcessorRegistry::new());
    registry.register_all(LlmProcessor::all(tasks))?;

    let store = Arc::new(MemStore::new(DeviceId("demo-pc".into())));
    let mut policy = PlanPolicy::default();
    for p in ["summarize.llm", "action_items.llm", "title.llm"] {
        policy.params.insert(p.into(), json!({ "lang": lang }));
    }
    policy.params.insert(
        "enhance_notes.llm".into(),
        json!({ "lang": lang, "user_notes": "# Ngân sách\n- marketing bao nhiêu?\n# Tuyển dụng" }),
    );
    let runner = JobRunner::new(
        store.clone(),
        registry,
        Arc::new(MemBlobStore::default()),
        vec![
            Capability::new("gpu"),
            Capability::new(format!("llm:{model}")),
        ],
        RunnerConfig {
            policy,
            ..Default::default()
        },
    );
    let mut rx = runner.subscribe();
    tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            eprintln!(
                "[{:>9}] {:<18} {:>4.0}% {}",
                format!("{:?}", p.status),
                p.job.processor,
                p.progress * 100.0,
                p.message
            );
        }
    });

    let m = fixtures::meeting(now_ms());
    store.append_local(EventBody::MeetingUpsert(m.clone()))?;
    store.append_local(EventBody::ArtifactAdd(fixtures::transcript(
        &m.id,
        &store.device_id(),
    )))?;
    runner.plan_meeting(&m.id)?;
    for r in runner.run_pending().await? {
        if let RunResult::Finished(o) = r
            && o.error.is_some()
        {
            eprintln!("job {} failed: {:?}", o.job_id, o.error);
        }
    }
    for a in store.list_artifacts(&m.id, None)? {
        println!(
            "=== {} ({}) model={:?}\n{}",
            a.kind.0,
            a.provenance.processor,
            a.provenance.model,
            serde_json::to_string_pretty(&a.body)?
        );
    }
    Ok(())
}
