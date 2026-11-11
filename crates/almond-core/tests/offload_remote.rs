//! Remote Ollama (setting `llm.remote_ollama_url`) against a real machine, e.g. the home PC over
//! Tailscale. Ignored; run with:
//!
//! ```text
//! ALMOND_REMOTE_OLLAMA=100.80.179.59 cargo test -p almond-core --test offload_remote -- --ignored --nocapture
//! ```

mod common;

use almond_types::event::EventBody;
use almond_types::mock::fixtures;
use almond_types::store::Store;
use almond_types::{Artifact, ArtifactKind};
use serde_json::json;
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a reachable remote Ollama in ALMOND_REMOTE_OLLAMA"]
async fn remote_ollama_is_used_while_it_answers() {
    let remote = std::env::var("ALMOND_REMOTE_OLLAMA").expect("set ALMOND_REMOTE_OLLAMA");
    let (api, dir) = common::open_core("offload-remote").await;

    api.call(
        "settings.set",
        json!({ "key": "llm.remote_ollama_url", "value": remote }),
    )
    .await
    .unwrap();
    let st = api
        .call("llm.status", json!({ "refresh": true }))
        .await
        .unwrap();
    eprintln!("remote: {st}");
    assert_eq!(st["remote"], true, "{st}");
    assert!(!st["models"].as_array().unwrap().is_empty(), "{st}");

    // A meeting with a transcript gets its summary from the remote model.
    let core = api.core.clone();
    let m = fixtures::meeting(almond_types::time::now_ms());
    core.store()
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    core.store()
        .append_local(EventBody::ArtifactAdd(fixtures::transcript(
            &m.id,
            core.device_id(),
        )))
        .unwrap();
    core.processing.kick();
    let t0 = Instant::now();
    let summary: Artifact = loop {
        let arts = core
            .store()
            .list_artifacts(&m.id, Some(ArtifactKind::SUMMARY))
            .unwrap();
        if let Some(a) = arts.into_iter().next() {
            break a;
        }
        let failed = api
            .call("jobs.list", json!({ "status": "failed" }))
            .await
            .unwrap();
        assert!(failed.as_array().unwrap().is_empty(), "{failed}");
        assert!(
            t0.elapsed() < Duration::from_secs(300),
            "no summary in 5 min"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let model = summary.provenance.model.clone().unwrap_or_default();
    eprintln!("summary in {:?} by {model}", t0.elapsed());
    assert!(model.contains("gemma4:12b"), "{model}");

    // An unreachable remote falls back to this device's Ollama.
    api.call(
        "settings.set",
        json!({ "key": "llm.remote_ollama_url", "value": "http://127.0.0.1:9" }),
    )
    .await
    .unwrap();
    let st = api
        .call("llm.status", json!({ "refresh": true }))
        .await
        .unwrap();
    eprintln!("fallback: {st}");
    assert_eq!(st["remote"], false, "{st}");

    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}
