//! Real Ollama (ignored by default): the 30-min synthetic VI/EN meeting transcript through the
//! whole processing chain (long-meeting split/merge, claims, verdicts) via the core's job loop.
//!
//! ```text
//! cargo test -p almond-core --test processing_ollama -- --ignored --nocapture
//! ```
//! Env: `ALMOND_EVAL_TRANSCRIPT` (default
//! `~/.almond/eval/stt/meeting/transcript_gpu_zip30_turbo.json`), `ALMOND_EVAL_OUT` (directory
//! for the artifacts as JSON + Markdown; default: temp dir). Uses the Windows Ollama at
//! `OLLAMA_HOST` / `http://127.0.0.1:11434` and the default model slots (gemma4:12b on a GPU).

mod common;

use almond_types::Artifact;
use almond_types::artifact::body::{Claims, Summary, Transcript, Verdicts};
use almond_types::event::EventBody;
use almond_types::mock::fixtures;
use almond_types::store::Store;
use almond_types::{ArtifactDraft, Id, Provenance};
use common::open_core;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn transcript_path() -> PathBuf {
    std::env::var_os("ALMOND_EVAL_TRANSCRIPT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .expect("home dir");
            PathBuf::from(home).join(".almond/eval/stt/meeting/transcript_gpu_zip30_turbo.json")
        })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the real Ollama with gemma4:12b and the eval transcript"]
async fn real_30_min_meeting_through_the_chain() {
    let path = transcript_path();
    let body: Transcript = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap();
    let (api, dir) = open_core("proc-ollama").await;
    let core = api.core.clone();
    let info = core.processing.refresh_capabilities(&core).await;
    assert!(
        info.llm_error.is_none() && !info.llm_models.is_empty(),
        "Ollama not available: {:?}",
        info.llm_error
    );
    let slots = api.call("models.slots", json!({})).await.unwrap();
    eprintln!("capabilities: {:?}", info.capabilities);
    eprintln!("slots: {slots}");

    let mut m = fixtures::meeting(almond_types::time::now_ms() - 31 * 60_000);
    m.title = String::new();
    m.ended_at = Some(m.started_at + 30 * 60_000);
    core.store()
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    let mut rx = api.subscribe();
    let t0 = Instant::now();
    let tr = ArtifactDraft::new(
        m.id.clone(),
        &body,
        Provenance {
            processor: "transcribe.local".into(),
            processor_version: "eval".into(),
            model: Some("zipformer-vi-30m + large-v3-turbo".into()),
            ..Default::default()
        },
    )
    .unwrap()
    .into_artifact(
        Id::new(),
        almond_types::time::HlcClock::new().now(),
        core.device_id().clone(),
    );
    core.store()
        .append_local(EventBody::ArtifactAdd(tr))
        .unwrap();

    // Log job progress; wait for the verdicts.
    let mut started: BTreeMap<String, Instant> = BTreeMap::new();
    let mut took: BTreeMap<String, Duration> = BTreeMap::new();
    let deadline = Duration::from_secs(60 * 60);
    loop {
        while let Ok(n) = rx.try_recv() {
            if n.name != "job.progress" {
                continue;
            }
            let p = n.payload["job"]["processor"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let status = n.payload["status"].as_str().unwrap_or("").to_string();
            match status.as_str() {
                "claimed" => {
                    started.entry(p.clone()).or_insert_with(Instant::now);
                }
                "done" | "failed" | "cancelled" => {
                    if let Some(s) = started.get(&p) {
                        took.insert(p.clone(), s.elapsed());
                    }
                }
                _ => {}
            }
            eprintln!(
                "[{:>6.1}s] {:<22} {:<9} {:>4.0}% {}",
                t0.elapsed().as_secs_f32(),
                p,
                status,
                n.payload["progress"].as_f64().unwrap_or(0.0) * 100.0,
                n.payload["message"].as_str().unwrap_or("")
            );
        }
        let failed = api
            .call("jobs.list", json!({ "status": "failed" }))
            .await
            .unwrap();
        assert!(
            failed.as_array().unwrap().is_empty(),
            "failed jobs: {failed:#}"
        );
        if !core
            .store()
            .list_artifacts(&m.id, Some("verdicts"))
            .unwrap()
            .is_empty()
        {
            break;
        }
        assert!(t0.elapsed() < deadline, "chain did not finish in time");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let total = t0.elapsed();

    let arts: Vec<Artifact> = core.store().list_artifacts(&m.id, None).unwrap();
    let get = |k: &str| arts.iter().find(|a| a.kind.is(k)).unwrap().clone();
    let summary: Summary = get("summary").decode().unwrap();
    let claims: Claims = get("claims").decode().unwrap();
    let verdicts: Verdicts = get("verdicts").decode().unwrap();
    let mut labels: BTreeMap<String, usize> = BTreeMap::new();
    for v in verdicts
        .verdicts
        .iter()
        .filter(|v| v.verifier == "verify.aggregate")
    {
        *labels.entry(format!("{:?}", v.label)).or_default() += 1;
    }
    let bullets: usize = summary.sections.iter().map(|s| s.bullets.len()).sum();
    let uncited = summary
        .sections
        .iter()
        .flat_map(|s| &s.bullets)
        .filter(|b| b.cites.is_empty())
        .count();
    let title = core.store().get_meeting(&m.id).unwrap().unwrap().title;

    let out = std::env::var_os("ALMOND_EVAL_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("almond-processing-eval"));
    std::fs::create_dir_all(&out).unwrap();
    for a in &arts {
        if a.kind.is("transcript") {
            continue;
        }
        std::fs::write(
            out.join(format!("{}.json", a.kind.0)),
            serde_json::to_string_pretty(a).unwrap(),
        )
        .unwrap();
    }
    let props = api
        .call("actions.propose", json!({ "meeting_id": m.id }))
        .await
        .unwrap();
    std::fs::write(
        out.join("meeting.md"),
        props["proposals"][0]["preview"].as_str().unwrap_or(""),
    )
    .unwrap();

    eprintln!(
        "\n=== {} segments, total {:.1}s",
        body.segments.len(),
        total.as_secs_f32()
    );
    for (p, d) in &took {
        eprintln!("  {p:<22} {:>7.1}s", d.as_secs_f32());
    }
    eprintln!("title: {title}");
    eprintln!(
        "summary: {} sections, {bullets} bullets ({uncited} uncited), map parts: see provenance",
        summary.sections.len()
    );
    eprintln!(
        "claims: {}, aggregate labels: {labels:?}, verifier errors: {:?}",
        claims.claims.len(),
        verdicts.errors
    );
    eprintln!("artifacts written to {}", out.display());

    assert!(!title.trim().is_empty());
    assert!(bullets >= 5, "summary too short: {bullets} bullets");
    assert!(verdicts.errors.is_empty(), "{:?}", verdicts.errors);
    assert!(
        labels.values().sum::<usize>() == claims.claims.len(),
        "one aggregate verdict per claim"
    );
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}
