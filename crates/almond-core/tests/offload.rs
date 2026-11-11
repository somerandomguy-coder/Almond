//! Offload policy (`processing.offload`) between two real cores syncing over localhost iroh:
//! the "laptop" (no GPU, so `prefer_peer` by default) can run a heavy job itself but leaves it
//! for the paired "PC" (GPU), which only ever *initiates* the syncs; after
//! `processing.offload_wait_min` the laptop runs such jobs itself.

use almond_core::{Api, Core, CoreConfig};
use almond_pipeline::JobRequest;
use almond_types::job::Capability;
use almond_types::mock::{MockProcessor, fixtures};
use almond_types::pipeline::ProcessorDescriptor;
use almond_types::store::Store;
use almond_types::{ArtifactDraft, DeviceId, Id};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

const HEAVY: &str = "test.heavy";
const KIND: &str = "offload_test";

async fn open(name: &str, caps: &[&str]) -> (Api, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "almond-core-test-{name}-{}-{}",
        std::process::id(),
        almond_types::time::now_ms()
    ));
    let mut cfg = CoreConfig::new(&dir, name);
    cfg.network = false;
    cfg.capabilities = caps.iter().map(|c| Capability::new(*c)).collect();
    let api = Core::open(cfg).await.expect("open core");
    api.core
        .registry
        .register(Arc::new(MockProcessor {
            desc: ProcessorDescriptor {
                name: HEAVY.into(),
                version: "0".into(),
                consumes: vec![],
                produces: vec![KIND.into()],
                needs_audio: false,
                requires: vec![Capability::new("test:heavy")],
                description: "stands in for transcription".into(),
            },
            output: vec![ArtifactDraft {
                meeting_id: Id::new(),
                kind: almond_types::ArtifactKind(KIND.into()),
                schema: 1,
                body: json!({ "ok": true }),
                provenance: Default::default(),
                supersedes: None,
            }],
        }))
        .unwrap();
    // Pick up the static capabilities (and the new processor) now.
    api.core.processing.refresh_capabilities(&api.core).await;
    (api, dir)
}

async fn call(api: &Api, m: &str, p: Value) -> Value {
    match api.call(m, p.clone()).await {
        Ok(v) => v,
        Err(e) => panic!("{m}({p}) failed: {e}"),
    }
}

/// A meeting on `api`'s device with one heavy job requested.
fn meeting_with_job(api: &Api) -> Id {
    let m = fixtures::meeting(almond_types::time::now_ms());
    let core = &api.core;
    core.store()
        .append_local(almond_types::event::EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    core.processing
        .runner()
        .request(JobRequest {
            meeting_id: m.id.clone(),
            processor: HEAVY.into(),
            inputs: vec![],
            params: Value::Null,
            priority: 0,
            requires: None,
        })
        .unwrap();
    core.processing.kick();
    m.id
}

fn made_by(api: &Api, m: &Id) -> Vec<DeviceId> {
    api.core
        .store()
        .list_artifacts(m, Some(KIND))
        .unwrap()
        .into_iter()
        .map(|a| a.device)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn laptop_leaves_heavy_jobs_for_the_pc_then_falls_back() {
    let (laptop, dir_l) = open("offload-laptop", &["test:heavy"]).await;
    let (pc, dir_p) = open("offload-pc", &["test:heavy", "gpu"]).await;
    let (lid, pid) = (laptop.core.device_id().clone(), pc.core.device_id().clone());
    let st = call(&laptop, "llm.status", json!({})).await;
    assert_eq!(
        st["offload"], "prefer_peer",
        "no GPU: prefer_peer by default"
    );
    assert_eq!(call(&pc, "llm.status", json!({})).await["offload"], "local");

    // Pair: the laptop shows the code, the PC joins (and syncs, so the laptop learns the PC's
    // capabilities). The laptop never starts a sync in this test.
    let begin = call(&laptop, "sync.pairBegin", json!({})).await;
    call(
        &pc,
        "sync.pairAccept",
        json!({ "qr_payload": begin["qr_payload"] }),
    )
    .await;
    let t0 = Instant::now();
    while !laptop
        .core
        .store()
        .devices()
        .unwrap()
        .iter()
        .any(|d| d.id == pid && d.capabilities.iter().any(|c| c.0 == "gpu"))
    {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "PC's caps never synced"
        );
        call(&pc, "sync.now", json!({ "peers": [lid.0] })).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // 1. The laptop could run the job but leaves it for the PC.
    let m = meeting_with_job(&laptop);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(made_by(&laptop, &m).is_empty(), "laptop ran it itself");

    // The PC syncs, runs it, syncs the artifact back.
    let t0 = Instant::now();
    loop {
        call(&pc, "sync.now", json!({ "peers": [lid.0] })).await;
        let by = made_by(&laptop, &m);
        if !by.is_empty() {
            assert!(by.iter().all(|d| *d == pid), "{by:?}");
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "PC never delivered");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // 2. Waited long enough (0 min here): the laptop runs it itself.
    call(
        &laptop,
        "settings.set",
        json!({ "key": "processing.offload_wait_min", "value": 0 }),
    )
    .await;
    let m2 = meeting_with_job(&laptop);
    let t0 = Instant::now();
    while made_by(&laptop, &m2).is_empty() {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "laptop never fell back"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(made_by(&laptop, &m2), vec![lid.clone()]);

    // 3. "local" never waits.
    call(
        &laptop,
        "settings.set",
        json!({ "key": "processing.offload_wait_min", "value": 60 }),
    )
    .await;
    call(
        &laptop,
        "settings.set",
        json!({ "key": "processing.offload", "value": "local" }),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(6)).await; // past the policy's 5 s snapshot cache
    let m3 = meeting_with_job(&laptop);
    let t0 = Instant::now();
    while made_by(&laptop, &m3).is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(30), "local mode waited");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    for (api, dir) in [(laptop, dir_l), (pc, dir_p)] {
        api.core.shutdown().await;
        drop(api);
        let _ = std::fs::remove_dir_all(dir);
    }
}
