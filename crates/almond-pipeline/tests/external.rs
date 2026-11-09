//! External processor host against `examples/external_processor.py` over stdio and HTTP.
//! Skips (with a message) when no Python 3 interpreter is found.

use almond_pipeline::external::{connect_http, spawn_stdio};
use almond_pipeline::{JobRunner, ProcessorRegistry, RunResult, RunnerConfig};
use almond_types::event::EventBody;
use almond_types::job::JobStatus;
use almond_types::mock::{MemBlobStore, MemStore, fixtures};
use almond_types::pipeline::{JobCtx, Processor};
use almond_types::store::Store;
use almond_types::{DeviceId, Id};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;

fn script() -> String {
    format!(
        "{}/examples/external_processor.py",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn python() -> Option<String> {
    for p in ["python3", "python", "py"] {
        if std::process::Command::new(p)
            .arg("--version")
            .output()
            .is_ok_and(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).contains("Python 3")
            })
        {
            return Some(p.to_string());
        }
    }
    eprintln!("SKIP: no python3 interpreter found");
    None
}

#[tokio::test]
async fn stdio_processor_runs_through_the_job_runner() {
    let Some(py) = python() else { return };
    let procs = spawn_stdio(&py, &[script()]).await.unwrap();
    assert_eq!(procs.len(), 1);
    let d = procs[0].descriptor();
    assert_eq!(d.name, "stats.talk_time");
    assert_eq!(d.consumes, vec!["transcript"]);

    let store = Arc::new(MemStore::new(DeviceId("pc".into())));
    let reg = Arc::new(ProcessorRegistry::new());
    reg.register(procs[0].clone()).unwrap();
    let runner = JobRunner::new(
        store.clone(),
        reg,
        Arc::new(MemBlobStore::default()),
        vec![],
        RunnerConfig::default(),
    );
    let m = fixtures::meeting(0);
    store
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    let t = fixtures::transcript(&m.id, &store.device_id());
    store
        .append_local(EventBody::ArtifactAdd(t.clone()))
        .unwrap();

    let mut rx = runner.subscribe();
    runner.plan_meeting(&m.id).unwrap();
    let res = runner.run_pending().await.unwrap();
    match &res[..] {
        [RunResult::Finished(o)] => assert_eq!(o.status, JobStatus::Done, "{o:?}"),
        other => panic!("{other:?}"),
    }
    let mut msgs = vec![];
    while let Ok(p) = rx.try_recv() {
        msgs.push(p.message);
    }
    assert!(
        msgs.iter().any(|m| m == "segment 4/4"),
        "progress forwarded: {msgs:?}"
    );

    let a = &store.list_artifacts(&m.id, Some("talk_time")).unwrap()[0];
    assert_eq!(a.provenance.processor, "stats.talk_time");
    assert_eq!(a.provenance.inputs, vec![t.id.clone()]);
    let speakers = a.body["speakers"].as_array().unwrap();
    assert_eq!(speakers.len(), 3);
    assert_eq!(speakers[0]["speaker"], "me");
    assert_eq!(speakers[0]["segments"], 2);
    assert_eq!(speakers[0]["cites"][0]["artifact"], t.id.0.as_str());
}

#[tokio::test]
async fn stdio_errors_map_to_error_variants() {
    let Some(py) = python() else { return };
    let procs = spawn_stdio(&py, &[script()]).await.unwrap();
    let ctx = JobCtx {
        job_id: Id::new(),
        meeting: fixtures::meeting(0),
        params: serde_json::Value::Null,
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(|_, _| {}),
        blobs: Arc::new(MemBlobStore::default()),
    };
    // No transcript among inputs -> -32602 -> Invalid.
    let e = procs[0].run(&ctx, vec![]).await.unwrap_err();
    assert!(matches!(e, almond_types::Error::Invalid(_)), "{e}");
}

#[tokio::test]
async fn http_processor() {
    let Some(py) = python() else { return };
    let mut child = tokio::process::Command::new(&py)
        .args([script(), "--http".into(), "0".into()])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let url = ready.trim_start_matches("listening on ").trim().to_string();
    let procs = connect_http(&url).await.unwrap();
    assert_eq!(procs[0].descriptor().name, "stats.talk_time");

    let m = fixtures::meeting(0);
    let t = fixtures::transcript(&m.id, &DeviceId("pc".into()));
    let seen = Arc::new(Mutex::new(0));
    let s2 = seen.clone();
    let ctx = JobCtx {
        job_id: Id::new(),
        meeting: m.clone(),
        params: serde_json::json!({}),
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(move |_, _| *s2.lock().unwrap() += 1),
        blobs: Arc::new(MemBlobStore::default()),
    };
    let drafts = procs[0].run(&ctx, vec![t]).await.unwrap();
    assert_eq!(drafts.len(), 1);
    assert!(drafts[0].kind.is("talk_time"));
    assert_eq!(drafts[0].meeting_id, m.id);
    assert_eq!(*seen.lock().unwrap(), 0, "HTTP has no progress channel");
    child.kill().await.ok();
}
