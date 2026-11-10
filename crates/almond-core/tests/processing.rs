//! Processing module through the core: job loop, mock STT + mock LLM through the whole chain
//! (transcript → summary, notes, action items, title → claims → verdicts), the API methods, and
//! offloading jobs from a core without an LLM to one with an LLM over localhost sync.

mod common;

use almond_core::Api;
use almond_core::types::api::notifications;
use almond_types::artifact::body::{Summary, Title, VerdictLabel, Verdicts};
use almond_types::event::EventBody;
use almond_types::job::Capability;
use almond_types::llm::{ChatRequest, ChatResponse, LlmModelInfo, LlmProvider};
use almond_types::meeting::{AudioChannel, AudioTrack};
use almond_types::mock::{MockProcessor, fixtures};
use almond_types::pipeline::ProcessorDescriptor;
use almond_types::store::{BlobStore, Store};
use almond_types::{Artifact, ArtifactDraft, Error, Meeting, Result};
use async_trait::async_trait;
use common::open_core;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Answers each structured task by the shape of its JSON schema, from the fixture transcript
/// (4 segments: budget 200 triệu, hire 2 engineers by December, Lan sends the plan by Friday).
#[derive(Default)]
struct TaskMockLlm {
    calls: AtomicU32,
}

#[async_trait]
impl LlmProvider for TaskMockLlm {
    fn name(&self) -> &str {
        "mock"
    }
    async fn health(&self) -> Result<()> {
        Ok(())
    }
    async fn list_models(&self) -> Result<Vec<LlmModelInfo>> {
        Ok(vec![LlmModelInfo {
            name: "mock:latest".into(),
            digest: Some("d1".into()),
            size_bytes: Some(1),
            context_length: Some(32_768),
        }])
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let props = req
            .json_schema
            .as_ref()
            .and_then(|s| s.get("properties"))
            .cloned()
            .unwrap_or(Value::Null);
        let has = |k: &str| props.get(k).is_some();
        let reply = if has("sections") {
            json!({"sections": [{"heading": "Quyết định", "bullets": [
                // Deliberately wrong number: the transcript says 200 triệu.
                {"text": "Ngân sách marketing Q4 là 300 triệu đồng.", "cites": [1]},
                {"text": "Tuyển 2 kỹ sư backend trước tháng 12.", "cites": [2]},
                {"text": "Lan gửi kế hoạch trước thứ Sáu.", "cites": [3]}
            ]}]})
        } else if has("additions") {
            json!({"user_cites": [{"note": 0, "cites": [1]}], "additions": [
                {"after": 0, "style": "bullet", "level": 1,
                 "text": "Ngân sách marketing là 200 triệu đồng.", "cites": [1]}
            ]})
        } else if has("items") {
            json!({"items": [{"text": "Gửi bản kế hoạch", "owner": "Lan", "due": "thứ Sáu", "cites": [3]}]})
        } else if has("title") {
            json!({"title": "Ngân sách Q4 và tuyển dụng"})
        } else if has("label") {
            json!({"reason": "not stated", "quote": "", "segment": -1, "label": "neutral", "confidence": 0.5})
        } else {
            json!({})
        };
        Ok(ChatResponse {
            content: reply.to_string(),
            model: self.list_models().await?.remove(0),
            prompt_tokens: Some(10),
            completion_tokens: Some(10),
        })
    }
    async fn chat_stream(&self, _: ChatRequest) -> Result<BoxStream<'static, Result<String>>> {
        Err(Error::Unsupported("stream".into()))
    }
}

/// An unreachable LLM (a device without Ollama).
struct NoLlm;

#[async_trait]
impl LlmProvider for NoLlm {
    fn name(&self) -> &str {
        "none"
    }
    async fn health(&self) -> Result<()> {
        Err(Error::Network("no llm here".into()))
    }
    async fn list_models(&self) -> Result<Vec<LlmModelInfo>> {
        Err(Error::Network("no llm here".into()))
    }
    async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
        Err(Error::Network("no llm here".into()))
    }
    async fn chat_stream(&self, _: ChatRequest) -> Result<BoxStream<'static, Result<String>>> {
        Err(Error::Network("no llm here".into()))
    }
}

/// `transcribe.local` stand-in: returns the fixture transcript.
fn mock_transcriber() -> Arc<MockProcessor> {
    let m = fixtures::meeting(0);
    let t = fixtures::transcript(&m.id, &almond_types::DeviceId("x".into()));
    Arc::new(MockProcessor {
        desc: ProcessorDescriptor {
            name: "transcribe.local".into(),
            version: "0".into(),
            consumes: vec![],
            produces: vec!["transcript".into()],
            needs_audio: true,
            requires: vec![],
            description: "mock".into(),
        },
        output: vec![ArtifactDraft {
            meeting_id: m.id,
            kind: t.kind,
            schema: t.schema,
            body: t.body,
            provenance: Default::default(),
            supersedes: None,
        }],
    })
}

async fn wait_for(api: &Api, meeting: &Meeting, kind: &str, secs: u64) -> Vec<Artifact> {
    let t0 = Instant::now();
    loop {
        let v: Vec<Artifact> = serde_json::from_value(
            api.call(
                "artifacts.list",
                json!({ "meeting_id": meeting.id, "kind": kind }),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        if !v.is_empty() {
            return v;
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            let all = api
                .call("artifacts.list", json!({ "meeting_id": meeting.id }))
                .await
                .unwrap();
            let jobs = api.call("jobs.list", json!({})).await.unwrap();
            panic!(
                "no {kind} artifact after {secs}s\nartifacts: {}\njobs: {jobs:#}",
                all.as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a["kind"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A finished meeting with one (fake) audio track in the blob store.
fn meeting_with_audio(api: &Api) -> Meeting {
    let hash = api.core.blobs().put(b"OggS fake opus").unwrap();
    let mut m = fixtures::meeting(almond_types::time::now_ms() - 30 * 60_000);
    m.title = String::new();
    m.audio = vec![AudioTrack {
        channel: AudioChannel::Mixed,
        blob: hash,
        codec: "opus".into(),
        sample_rate: 16_000,
        offset_ms: 0,
        duration_ms: 20_000,
    }];
    api.core
        .store()
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    m
}

#[tokio::test(flavor = "multi_thread")]
async fn meeting_with_audio_runs_the_whole_chain() {
    let (api, dir) = open_core("proc-chain").await;
    let core = api.core.clone();
    let llm = Arc::new(TaskMockLlm::default());
    core.processing
        .set_llm_provider(&core, llm.clone() as Arc<dyn LlmProvider>)
        .await;
    core.registry.register(mock_transcriber()).unwrap();
    let mut rx = api.subscribe();

    let caps = core.processing.capabilities().capabilities;
    assert!(caps.contains(&Capability::new("llm")), "{caps:?}");
    assert!(caps.contains(&Capability::new("llm:mock:latest")));

    let m = meeting_with_audio(&api);
    let verdicts = wait_for(&api, &m, "verdicts", 60).await;

    // Every artifact of the chain exists.
    let all: Vec<Artifact> = serde_json::from_value(
        api.call("artifacts.list", json!({ "meeting_id": m.id }))
            .await
            .unwrap(),
    )
    .unwrap();
    for k in [
        "transcript",
        "summary",
        "enhanced_notes",
        "action_items",
        "title",
        "claims",
        "verdicts",
    ] {
        assert!(
            all.iter().any(|a| a.kind.is(k)),
            "missing {k}: {:?}",
            all.iter().map(|a| &a.kind.0).collect::<Vec<_>>()
        );
    }
    let summary = all.iter().find(|a| a.kind.is("summary")).unwrap();
    assert_eq!(
        summary.provenance.model.as_deref(),
        Some("mock:mock:latest@d1")
    );
    let s: Summary = summary.decode().unwrap();
    assert_eq!(s.sections[0].bullets.len(), 3);

    // The wrong number (300 vs 200 triệu) is caught; verify.nli ran without errors.
    let v: Verdicts = verdicts[0].decode().unwrap();
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    assert!(
        v.verdicts
            .iter()
            .any(|x| x.verifier == "verify.numbers" && x.label == VerdictLabel::Contradicted),
        "{:#?}",
        v.verdicts
    );
    assert!(v.verdicts.iter().any(|x| x.verifier == "verify.nli"));
    assert!(v.verdicts.iter().any(|x| x.verifier == "verify.aggregate"));

    // The title artifact named the untitled meeting.
    let t0 = Instant::now();
    loop {
        let mm = core.store().get_meeting(&m.id).unwrap().unwrap();
        if mm.title == "Ngân sách Q4 và tuyển dụng" {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "title not applied");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Notifications: artifact.added for each artifact, job.progress for the jobs.
    // (The scanner polls the log, so the last notifications may trail the store by ~0.5 s.)
    let mut added = vec![];
    let mut progress = 0;
    let t0 = Instant::now();
    while !added.iter().any(|a| a == "verdicts") && t0.elapsed() < Duration::from_secs(5) {
        while let Ok(n) = rx.try_recv() {
            if n.name == notifications::ARTIFACT_ADDED {
                added.push(n.payload["kind"].as_str().unwrap().to_string());
            } else if n.name == notifications::JOB_PROGRESS {
                progress += 1;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for k in ["transcript", "summary", "title", "claims", "verdicts"] {
        assert!(added.iter().any(|a| a == k), "no artifact.added for {k}");
    }
    assert!(progress >= 7, "job.progress count {progress}");

    // jobs.list: every job done.
    let jobs = api.call("jobs.list", json!({})).await.unwrap();
    let jobs = jobs.as_array().unwrap();
    assert!(jobs.len() >= 7, "{jobs:#?}");
    assert!(jobs.iter().all(|j| j["status"] == "done"), "{jobs:#?}");
    let done = api
        .call("jobs.list", json!({ "status": "done" }))
        .await
        .unwrap();
    assert_eq!(done.as_array().unwrap().len(), jobs.len());

    // artifacts.get
    let got = api
        .call("artifacts.get", json!({ "id": summary.id }))
        .await
        .unwrap();
    assert_eq!(got["id"], json!(summary.id));
    let err = api
        .call(
            "artifacts.get",
            json!({ "id": "01J00000000000000000000000" }),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, -32004);

    // pipeline.processors / verify.verifiers
    let procs = api.call("pipeline.processors", json!({})).await.unwrap();
    let names: Vec<&str> = procs
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    for n in [
        "summarize.llm",
        "enhance_notes.llm",
        "action_items.llm",
        "title.llm",
        "verify.extract_claims",
        "verify.all",
    ] {
        assert!(names.contains(&n), "{names:?}");
    }
    let vs = api.call("verify.verifiers", json!({})).await.unwrap();
    assert!(
        vs.as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "verify.nli")
    );

    // Rerun the summary: the new one supersedes the old one, claims + verdicts follow.
    let calls_before = llm.calls.load(Ordering::Relaxed);
    let job = api
        .call(
            "artifacts.rerun",
            json!({ "meeting_id": m.id, "processor": "summarize.llm", "inputs": [],
                    "params": { "lang": "en" }, "allow_offload": false }),
        )
        .await
        .unwrap();
    assert_eq!(job["processor"], "summarize.llm");
    let t0 = Instant::now();
    let new_summary = loop {
        let v = core.store().list_artifacts(&m.id, Some("summary")).unwrap();
        if let Some(a) = v
            .iter()
            .find(|a| a.supersedes.as_ref() == Some(&summary.id))
        {
            break a.clone();
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "rerun produced nothing"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(llm.calls.load(Ordering::Relaxed) > calls_before);
    assert_eq!(
        new_summary.decode::<Summary>().unwrap().lang.unwrap().0,
        "en"
    );
    let t0 = Instant::now();
    while !core
        .store()
        .list_artifacts(&m.id, Some("claims"))
        .unwrap()
        .iter()
        .any(|c| c.provenance.inputs.contains(&new_summary.id))
    {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "no claims for rerun"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // verify.run on the summary with a verifier subset.
    let job = api
        .call(
            "verify.run",
            json!({ "artifact_id": new_summary.id, "verifiers": ["verify.numbers"] }),
        )
        .await
        .unwrap();
    assert_eq!(job["processor"], "verify.all");
    assert_eq!(job["params"]["verifiers"], json!(["verify.numbers"]));

    // Actions: propose (recorded once), execute writes the Markdown file.
    let acts = api.call("actions.list", json!({})).await.unwrap();
    assert_eq!(acts[0]["id"], "export.markdown");
    let props = api
        .call("actions.propose", json!({ "meeting_id": m.id }))
        .await
        .unwrap();
    let mut p = props["proposals"][0].clone();
    assert_eq!(p["action"], "export.markdown");
    assert!(p["preview"].as_str().unwrap().contains("## Summary"));
    let again = api
        .call("actions.propose", json!({ "meeting_id": m.id }))
        .await
        .unwrap();
    assert_eq!(again, props);
    assert_eq!(
        core.store()
            .list_artifacts(&m.id, Some("action_proposals"))
            .unwrap()
            .len(),
        1
    );
    let out = dir.join("export").join("notes.md");
    p["payload"]["path"] = json!(out);
    let res = api
        .call("actions.execute", json!({ "proposal": p }))
        .await
        .unwrap();
    assert_eq!(res["ok"], true, "{res}");
    let md = std::fs::read_to_string(&out).unwrap();
    assert!(md.contains("# Ngân sách Q4 và tuyển dụng"), "{md}");
    assert!(md.contains("Lan"), "{md}");
    assert_eq!(
        core.store()
            .list_artifacts(&m.id, Some("action_result"))
            .unwrap()
            .len(),
        1
    );

    // Models: slots default to the only chat model; selecting a missing model fails loudly.
    let slots = api.call("models.slots", json!({})).await.unwrap();
    assert!(
        slots
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["task"] == "llm_summary" && s["model"] == "ollama:mock:latest"),
        "{slots}"
    );
    let err = api
        .call(
            "models.select",
            json!({ "task": "llm_summary", "lang": null, "model": "nope:1b" }),
        )
        .await
        .unwrap_err();
    assert!(err.message.contains("ollama pull nope:1b"), "{err}");
    api.call(
        "models.select",
        json!({ "task": "llm_verify", "lang": "vi", "model": "ollama:mock:latest" }),
    )
    .await
    .unwrap();
    let stored = api
        .call("settings.get", json!({ "key": "models.slots" }))
        .await
        .unwrap();
    assert_eq!(stored[0]["lang"], "vi");
    let models = api.call("models.list", json!({})).await.unwrap();
    assert!(
        models
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"] == "ollama:mock:latest" && e["installed"] == true)
    );

    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_model_fails_the_job_loudly() {
    let (api, dir) = open_core("proc-missing").await;
    let core = api.core.clone();
    core.processing
        .set_llm_provider(
            &core,
            Arc::new(TaskMockLlm::default()) as Arc<dyn LlmProvider>,
        )
        .await;
    // A configured slot that names a model this device doesn't have.
    core.set_setting(
        "models.slots",
        &json!([{ "task": "llm_summary", "lang": null, "model": "gemma9:99b" }]),
    )
    .unwrap();
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
    let t0 = Instant::now();
    let failed = loop {
        let jobs = api
            .call("jobs.list", json!({ "status": "failed" }))
            .await
            .unwrap();
        if let Some(j) = jobs
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["job"]["processor"] == "summarize.llm")
        {
            break j.clone();
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "no failed job");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let msg = failed["message"].as_str().unwrap();
    assert!(
        msg.contains("gemma9:99b") && msg.contains("ollama pull"),
        "{msg}"
    );
    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

/// The "PC": a sync node with its own (file) identity running LLM + verify processors on the
/// mock LLM through a `JobRunner`. (Two `Core`s in one process would share the device key on
/// Windows, where the default secret store is the OS keyring; see
/// docs/contract-requests/i-processing.md P5.)
struct Pc {
    node: almond_sync::SyncNode,
    runner: almond_pipeline::JobRunner,
    dir: std::path::PathBuf,
}

async fn open_pc() -> Pc {
    use almond_llm::{LlmProcessor, LlmTasks, PromptLibrary, TaskConfig};
    use almond_verify::{
        ClaimExtractor, ExtractClaimsProcessor, VerifierRegistry, VerifyProcessor,
    };
    let dir = std::env::temp_dir().join(format!(
        "almond-core-test-proc-pc-{}-{}",
        std::process::id(),
        almond_types::time::now_ms()
    ));
    let mut cfg = almond_sync::NodeConfig::new(&dir, "Test PC");
    cfg.iroh = almond_sync::IrohConfig {
        port: 0,
        bind_ip: Some(std::net::Ipv4Addr::LOCALHOST.into()),
        mdns: false,
    };
    cfg.secrets = Some(Arc::new(almond_sync::secrets::FileSecretStore::new(
        dir.join("secrets.json"),
    )));
    let node = almond_sync::SyncNode::open(cfg).await.unwrap();
    let llm: Arc<dyn LlmProvider> = Arc::new(TaskMockLlm::default());
    let registry = Arc::new(almond_pipeline::ProcessorRegistry::new());
    let tasks = LlmTasks::new(
        llm.clone(),
        PromptLibrary::builtin(),
        TaskConfig {
            model: "mock:latest".into(),
            ..Default::default()
        },
    );
    registry.register_all(LlmProcessor::all(tasks)).unwrap();
    registry
        .register(Arc::new(ExtractClaimsProcessor::new(
            ClaimExtractor::rules(),
        )))
        .unwrap();
    let mut verify = VerifyProcessor::all(VerifierRegistry::with_llm(
        Default::default(),
        llm,
        "mock:latest",
    ))
    .requiring(vec![Capability::new("llm")]);
    verify.name = "verify.all".into();
    registry.register(Arc::new(verify)).unwrap();
    let runner = almond_pipeline::JobRunner::new(
        node.store.clone() as Arc<dyn Store>,
        registry,
        node.blobs.clone() as Arc<dyn BlobStore>,
        vec![Capability::new("llm"), Capability::new("llm:mock:latest")],
        Default::default(),
    );
    Pc { node, runner, dir }
}

#[tokio::test(flavor = "multi_thread")]
async fn weak_device_offloads_jobs_to_a_peer_with_an_llm() {
    // A: a core without an LLM (laptop/phone). B: the PC with a (mock) LLM.
    let (a, dir_a) = open_core("proc-offload-a").await;
    a.core
        .processing
        .set_llm_provider(&a.core, Arc::new(NoLlm) as Arc<dyn LlmProvider>)
        .await;
    assert!(
        !a.core
            .processing
            .capabilities()
            .capabilities
            .iter()
            .any(|c| c.0.starts_with("llm"))
    );
    let b = open_pc().await;
    let a_id = a.core.device_id().clone();
    let b_id = b.node.device.id.clone();
    assert_ne!(a_id, b_id);

    // Pair: A hosts, B joins with the ticket.
    let ticket = a.core.node.transport.start_pairing(Duration::from_secs(60));
    b.node.transport.pair_with_ticket(&ticket).await.unwrap();

    // A has a meeting with a transcript; its job loop requests the LLM jobs, which can't run on A.
    let m = fixtures::meeting(almond_types::time::now_ms());
    a.core
        .store()
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    a.core
        .store()
        .append_local(EventBody::ArtifactAdd(fixtures::transcript(
            &m.id,
            a.core.device_id(),
        )))
        .unwrap();
    let t0 = Instant::now();
    loop {
        let pending = a
            .call("jobs.list", json!({ "status": "pending" }))
            .await
            .unwrap();
        if pending.as_array().unwrap().len() >= 4 {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "A did not plan jobs"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut rx_a = a.subscribe();

    // Sync, B runs what it can claim, sync again ... until A has verdicts.
    let t0 = Instant::now();
    let mut b_ran = 0;
    loop {
        let r = b.node.engine.sync_with(&a_id, None).await;
        assert!(r.error.is_none(), "{r:?}");
        b_ran += b.runner.run_pending().await.unwrap().len();
        let r = b.node.engine.sync_with(&a_id, None).await;
        assert!(r.error.is_none(), "{r:?}");
        let v = a
            .core
            .store()
            .list_artifacts(&m.id, Some("verdicts"))
            .unwrap();
        if !v.is_empty() {
            assert_eq!(v[0].device, b_id, "verdicts computed by B");
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "offload did not complete"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(b_ran >= 5, "B ran {b_ran} jobs");
    let arts = a.core.store().list_artifacts(&m.id, None).unwrap();
    for k in [
        "summary",
        "enhanced_notes",
        "action_items",
        "title",
        "claims",
    ] {
        let x = arts
            .iter()
            .find(|x| x.kind.is(k))
            .unwrap_or_else(|| panic!("A lacks {k}"));
        if k != "claims" {
            assert_eq!(x.device, b_id, "{k} should come from B");
        }
    }
    let t: Title = arts
        .iter()
        .find(|x| x.kind.is("title"))
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(t.title, "Ngân sách Q4 và tuyển dụng");
    // A announced the synced artifacts to its UI.
    let t0 = Instant::now();
    let mut added = vec![];
    while !added.iter().any(|k| k == "verdicts") && t0.elapsed() < Duration::from_secs(5) {
        while let Ok(n) = rx_a.try_recv() {
            if n.name == notifications::ARTIFACT_ADDED {
                added.push(n.payload["kind"].as_str().unwrap().to_string());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        added.iter().any(|k| k == "summary") && added.iter().any(|k| k == "verdicts"),
        "A's artifact.added: {added:?}"
    );
    // A's job view shows the LLM jobs done by B.
    let jobs = a.call("jobs.list", json!({})).await.unwrap();
    let llm_jobs: Vec<&Value> = jobs
        .as_array()
        .unwrap()
        .iter()
        .filter(|j| j["job"]["processor"].as_str().unwrap().ends_with(".llm"))
        .collect();
    assert_eq!(llm_jobs.len(), 4, "{jobs:#}");
    assert!(
        llm_jobs
            .iter()
            .all(|j| j["status"] == "done" && j["device"] == json!(b_id)),
        "{jobs:#}"
    );

    a.core.shutdown().await;
    b.node.shutdown().await;
    drop(a);
    let _ = std::fs::remove_dir_all(dir_a);
    let _ = std::fs::remove_dir_all(&b.dir);
}
