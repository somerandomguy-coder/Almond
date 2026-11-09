//! Job lifecycle over MemStore + MockLlm: offload phone -> PC via sync, DAG fan-out, retries,
//! failures, lease expiry, cancel, rerun with supersede.

use almond_llm::{LlmProcessor, LlmTasks, PromptLibrary, TaskConfig};
use almond_pipeline::{JobRequest, JobRunner, ProcessorRegistry, RunResult, RunnerConfig};
use almond_types::api::RerunParams;
use almond_types::artifact::body::{ActionItems, EnhancedNotes, Summary, Title};
use almond_types::event::EventBody;
use almond_types::job::{Capability, JobLease, JobStatus};
use almond_types::mock::{MemBlobStore, MemStore, MockLlm, fixtures};
use almond_types::pipeline::{JobCtx, Processor, ProcessorDescriptor};
use almond_types::store::Store;
use almond_types::{
    Artifact, ArtifactDraft, ArtifactKind, DeviceId, Error, Id, Provenance, Result,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A canned LLM answering every schema at once (each task reads only its own fields).
fn mock_llm() -> Arc<MockLlm> {
    Arc::new(MockLlm {
        reply: String::new(),
        json_reply: json!({
            "sections": [{"heading": "Quyết định", "bullets": [
                {"text": "Ngân sách marketing 200 triệu đồng.", "cites": [1]},
                {"text": "Tuyển 2 kỹ sư backend trước tháng 12.", "cites": [2, 77]}
            ]}],
            "user_cites": [{"note": 0, "cites": [1]}],
            "additions": [{"after": 0, "style": "bullet", "level": 1, "text": "Lan gửi kế hoạch trước thứ Sáu", "cites": [3]}],
            "items": [{"text": "Gửi bản kế hoạch", "owner": "Lan", "due": "thứ Sáu", "cites": [3]}],
            "title": "Kế hoạch ngân sách Q4"
        }),
    })
}

fn llm_processors() -> Vec<Arc<dyn Processor>> {
    LlmProcessor::all(LlmTasks::new(
        mock_llm(),
        PromptLibrary::builtin(),
        TaskConfig::default(),
    ))
}

fn fast_cfg() -> RunnerConfig {
    RunnerConfig {
        retry_backoff: Duration::from_millis(1),
        ..Default::default()
    }
}

struct Device {
    store: Arc<MemStore>,
    runner: JobRunner,
}

fn device(
    name: &str,
    procs: Vec<Arc<dyn Processor>>,
    descs: Vec<ProcessorDescriptor>,
    caps: &[&str],
) -> Device {
    let store = Arc::new(MemStore::new(DeviceId(name.into())));
    let reg = Arc::new(ProcessorRegistry::new());
    reg.register_all(procs).unwrap();
    for d in descs {
        reg.register_descriptor(d).unwrap();
    }
    let runner = JobRunner::new(
        store.clone(),
        reg,
        Arc::new(MemBlobStore::default()),
        caps.iter().map(|c| Capability::new(*c)).collect(),
        fast_cfg(),
    );
    Device { store, runner }
}

/// One-way sync `from` -> `to`.
fn sync(from: &MemStore, to: &MemStore) {
    let missing = from
        .events_missing(&to.version_vector().unwrap(), usize::MAX)
        .unwrap();
    to.apply_remote(missing).unwrap();
}

fn seed(store: &MemStore) -> (almond_types::Meeting, Artifact) {
    let m = fixtures::meeting(1_762_646_400_000);
    store
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    let t = fixtures::transcript(&m.id, &store.device_id());
    store
        .append_local(EventBody::ArtifactAdd(t.clone()))
        .unwrap();
    (m, t)
}

fn finished(r: &[RunResult]) -> Vec<(String, JobStatus)> {
    r.iter()
        .filter_map(|x| match x {
            RunResult::Finished(o) => Some((o.job_id.0.clone(), o.status)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn phone_requests_pc_claims_after_sync() {
    // Phone: knows the LLM processors only as descriptors, has no `llm` capability.
    let descs: Vec<ProcessorDescriptor> = llm_processors().iter().map(|p| p.descriptor()).collect();
    let phone = device("phone", vec![], descs, &["stt:vi"]);
    let pc = device("pc", llm_processors(), vec![], &["gpu", "llm:qwen3.5:9b"]);

    let (m, t) = seed(&phone.store);
    let jobs = phone.runner.plan_meeting(&m.id).unwrap();
    assert_eq!(jobs.len(), 4, "transcript fans out to 4 LLM processors");
    assert!(
        jobs.iter()
            .all(|j| j.requires == vec![Capability::new("llm")])
    );
    assert!(
        phone.runner.claimable().unwrap().is_empty(),
        "phone can't run them"
    );
    assert!(phone.runner.run_pending().await.unwrap().is_empty());
    // Planning again is idempotent.
    assert!(phone.runner.plan_meeting(&m.id).unwrap().is_empty());

    // PC offline: nothing to do. Then sync phone -> PC.
    assert!(pc.runner.claimable().unwrap().is_empty());
    sync(&phone.store, &pc.store);
    assert_eq!(pc.runner.claimable().unwrap().len(), 4);
    let mut rx = pc.runner.subscribe();
    let res = pc.runner.run_pending().await.unwrap();
    let f = finished(&res);
    assert_eq!(f.len(), 4);
    assert!(f.iter().all(|(_, s)| *s == JobStatus::Done), "{res:?}");
    let mut saw_progress = false;
    while let Ok(p) = rx.try_recv() {
        saw_progress |= p.status == JobStatus::Claimed && p.message == "summary done";
    }
    assert!(saw_progress, "processor progress is forwarded");

    // Results flow back to the phone.
    sync(&pc.store, &phone.store);
    let states = phone.runner.job_states().unwrap();
    assert!(states.values().all(|s| s.status() == JobStatus::Done));
    assert!(
        states
            .values()
            .all(|s| s.outcomes[0].device == DeviceId("pc".into()))
    );
    let get = |k: &str| phone.store.list_artifacts(&m.id, Some(k)).unwrap();
    let s: Summary = get("summary")[0].decode().unwrap();
    assert_eq!(s.sections[0].bullets.len(), 2);
    assert_eq!(
        s.sections[0].bullets[1].cites.len(),
        1,
        "invalid cite 77 dropped"
    );
    assert_eq!(s.sections[0].bullets[0].cites[0].artifact, t.id);
    let n: EnhancedNotes = get("enhanced_notes")[0].decode().unwrap();
    assert!(n.blocks.iter().any(|b| b.text.contains("Lan gửi")));
    let a: ActionItems = get("action_items")[0].decode().unwrap();
    assert_eq!(a.items[0].owner.as_deref(), Some("Lan"));
    let ti: Title = get("title")[0].decode().unwrap();
    assert_eq!(ti.title, "Kế hoạch ngân sách Q4");
    let art = &get("summary")[0];
    assert_eq!(art.device, DeviceId("pc".into()));
    assert_eq!(art.provenance.inputs, vec![t.id.clone()]);
    assert!(
        art.provenance
            .model
            .as_deref()
            .unwrap()
            .starts_with("mock:")
    );
}

/// Generic test processor.
struct FnProc {
    desc: ProcessorDescriptor,
    #[allow(clippy::type_complexity)]
    f: Box<dyn Fn(&JobCtx, &[Artifact]) -> Result<Vec<ArtifactDraft>> + Send + Sync>,
    delay: Duration,
}

#[async_trait]
impl Processor for FnProc {
    fn descriptor(&self) -> ProcessorDescriptor {
        self.desc.clone()
    }
    async fn run(&self, ctx: &JobCtx, inputs: Vec<Artifact>) -> Result<Vec<ArtifactDraft>> {
        let mut waited = Duration::ZERO;
        while waited < self.delay {
            ctx.check_cancel()?;
            tokio::time::sleep(Duration::from_millis(5)).await;
            waited += Duration::from_millis(5);
        }
        (self.f)(ctx, &inputs)
    }
}

fn desc(name: &str, consumes: &str, produces: &str) -> ProcessorDescriptor {
    ProcessorDescriptor {
        name: name.into(),
        version: "1".into(),
        consumes: vec![consumes.into()],
        produces: vec![produces.into()],
        needs_audio: false,
        requires: vec![],
        description: String::new(),
    }
}

fn draft(meeting: &Id, kind: &str, body: Value) -> ArtifactDraft {
    ArtifactDraft {
        meeting_id: meeting.clone(),
        kind: ArtifactKind::new(kind),
        schema: 1,
        body,
        provenance: Provenance::default(),
        supersedes: None,
    }
}

#[tokio::test]
async fn dag_fans_out_downstream_and_retries_transient_errors() {
    let calls = Arc::new(AtomicU32::new(0));
    let c2 = calls.clone();
    let claims = FnProc {
        desc: desc("extract_claims.test", "summary", "claims"),
        f: Box::new(move |ctx, inputs| {
            // Fails twice with a transient error, then succeeds.
            if c2.fetch_add(1, Ordering::SeqCst) < 2 {
                return Err(Error::Network("flaky".into()));
            }
            assert!(inputs[0].kind.is("summary"));
            Ok(vec![draft(
                &ctx.meeting.id,
                "claims",
                json!({"claims": []}),
            )])
        }),
        delay: Duration::ZERO,
    };
    let mut procs = llm_processors();
    procs.push(Arc::new(claims));
    let pc = device("pc", procs, vec![], &["llm"]);
    let (m, _) = seed(&pc.store);
    pc.runner.plan_meeting(&m.id).unwrap();
    let res = pc.runner.run_pending().await.unwrap();
    // 4 LLM jobs + the downstream claims job planned after the summary landed.
    assert_eq!(finished(&res).len(), 5, "{res:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let claims = pc.store.list_artifacts(&m.id, Some("claims")).unwrap();
    assert_eq!(claims.len(), 1);
    let summary = &pc.store.list_artifacts(&m.id, Some("summary")).unwrap()[0];
    assert_eq!(claims[0].provenance.inputs, vec![summary.id.clone()]);
    assert_eq!(claims[0].provenance.processor, "extract_claims.test");
}

#[tokio::test]
async fn permanent_failure_is_recorded_and_not_replanned() {
    let bad = FnProc {
        desc: desc("broken.test", "transcript", "thing"),
        f: Box::new(|_, _| Err(Error::Invalid("bad params".into()))),
        delay: Duration::ZERO,
    };
    let pc = device("pc", vec![Arc::new(bad)], vec![], &[]);
    let (m, _) = seed(&pc.store);
    pc.runner.plan_meeting(&m.id).unwrap();
    let res = pc.runner.run_pending().await.unwrap();
    assert_eq!(finished(&res)[0].1, JobStatus::Failed);
    let s = pc.runner.list(Some(JobStatus::Failed)).unwrap();
    assert_eq!(s.len(), 1);
    assert!(s[0].message.contains("bad params"));
    assert!(pc.runner.plan_meeting(&m.id).unwrap().is_empty());
    assert!(pc.runner.run_pending().await.unwrap().is_empty());
}

#[tokio::test]
async fn expired_lease_is_reclaimed_live_lease_is_respected() {
    let now = Arc::new(AtomicI64::new(1_000_000));
    let n2 = now.clone();
    let mut pc = device("pc", llm_processors(), vec![], &["llm"]);
    pc.runner = JobRunner::new(
        pc.store.clone(),
        pc.runner.registry().clone(),
        Arc::new(MemBlobStore::default()),
        vec![Capability::new("llm")],
        fast_cfg(),
    )
    .with_clock(move || n2.load(Ordering::SeqCst));
    let (m, t) = seed(&pc.store);
    let job = pc
        .runner
        .request(JobRequest {
            meeting_id: m.id.clone(),
            processor: "title.llm".into(),
            inputs: vec![t.id.clone()],
            params: Value::Null,
            priority: 0,
            requires: None,
        })
        .unwrap();
    // The laptop claimed it (event synced in) and then vanished.
    let laptop = MemStore::new(DeviceId("laptop".into()));
    laptop
        .append_local(EventBody::JobClaimed(JobLease {
            job_id: job.id.clone(),
            device: DeviceId("laptop".into()),
            claimed_at: Default::default(),
            expires_at: 1_060_000,
        }))
        .unwrap();
    sync(&laptop, &pc.store);
    assert!(
        pc.runner.claimable().unwrap().is_empty(),
        "laptop holds a live lease"
    );
    now.store(1_060_001, Ordering::SeqCst);
    assert_eq!(pc.runner.claimable().unwrap().len(), 1, "lease expired");
    let res = pc.runner.run_pending().await.unwrap();
    assert_eq!(finished(&res)[0].1, JobStatus::Done);
    let s = &pc.runner.job_states().unwrap()[&job.id];
    assert_eq!(s.claim_count(), 2);
}

#[tokio::test]
async fn cancel_pending_and_running() {
    let slow = FnProc {
        desc: desc("slow.test", "transcript", "thing"),
        f: Box::new(|ctx, _| Ok(vec![draft(&ctx.meeting.id, "thing", json!({}))])),
        delay: Duration::from_secs(30),
    };
    let pc = Arc::new(device("pc", vec![Arc::new(slow)], vec![], &[]));
    let (m, t) = seed(&pc.store);
    let req = || JobRequest {
        meeting_id: m.id.clone(),
        processor: "slow.test".into(),
        inputs: vec![t.id.clone()],
        params: Value::Null,
        priority: 0,
        requires: None,
    };
    // Pending -> cancelled without running.
    let j1 = pc.runner.request(req()).unwrap();
    pc.runner.cancel(&j1.id).unwrap();
    assert_eq!(
        pc.runner.job_states().unwrap()[&j1.id].status(),
        JobStatus::Cancelled
    );

    // Running -> cancel flag.
    let j2 = pc.runner.request(req()).unwrap();
    let pc2 = pc.clone();
    let j2c = j2.clone();
    let h = tokio::spawn(async move { pc2.runner.run_job(&j2c).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    pc.runner.cancel(&j2.id).unwrap();
    let r = tokio::time::timeout(Duration::from_secs(5), h)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match r {
        RunResult::Finished(o) => assert_eq!(o.status, JobStatus::Cancelled),
        other => panic!("{other:?}"),
    }
    assert!(
        pc.store
            .list_artifacts(&m.id, Some("thing"))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn rerun_supersedes_previous_version_and_can_pin_device() {
    let seen = Arc::new(Mutex::new(vec![]));
    let s2 = seen.clone();
    let p = FnProc {
        desc: desc("summarize.test", "transcript", "summary"),
        f: Box::new(move |ctx, _| {
            s2.lock().unwrap().push(ctx.params.clone());
            Ok(vec![draft(
                &ctx.meeting.id,
                "summary",
                json!({"lang": "vi", "sections": [], "v": ctx.params["v"]}),
            )])
        }),
        delay: Duration::ZERO,
    };
    let pc = device("pc", vec![Arc::new(p)], vec![], &[]);
    let (m, _) = seed(&pc.store);
    pc.runner.plan_meeting(&m.id).unwrap();
    pc.runner.run_pending().await.unwrap();
    let v1 = pc.store.list_artifacts(&m.id, Some("summary")).unwrap();
    assert_eq!(v1.len(), 1);
    assert_eq!(v1[0].supersedes, None);

    let job = pc
        .runner
        .rerun(RerunParams {
            meeting_id: m.id.clone(),
            processor: "summarize.test".into(),
            inputs: vec![],
            params: json!({"v": 2}),
            allow_offload: false,
        })
        .unwrap();
    assert!(job.requires.contains(&Capability::new("device:pc")));
    // Another device that *could* run the processor still may not: the job is pinned.
    let twin = FnProc {
        desc: desc("summarize.test", "transcript", "summary"),
        f: Box::new(|_, _| Ok(vec![])),
        delay: Duration::ZERO,
    };
    let other = device("other", vec![Arc::new(twin)], vec![], &[]);
    sync(&pc.store, &other.store);
    assert!(!other.runner.can_run(&job));
    assert!(other.runner.claimable().unwrap().is_empty());

    pc.runner.run_pending().await.unwrap();
    let all = pc.store.list_artifacts(&m.id, Some("summary")).unwrap();
    assert_eq!(all.len(), 2, "old version kept for comparison");
    let new = all.iter().find(|a| a.body["v"] == 2).unwrap();
    assert_eq!(new.supersedes.as_ref(), Some(&v1[0].id));
    // The DAG treats the new version as the live one; nothing else to plan.
    assert!(pc.runner.plan_meeting(&m.id).unwrap().is_empty());
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn unknown_processor_needs_explicit_requires() {
    let phone = device("phone", vec![], vec![], &[]);
    let (m, t) = seed(&phone.store);
    let mut r = JobRequest {
        meeting_id: m.id.clone(),
        processor: "transcribe.phowhisper".into(),
        inputs: vec![t.id.clone()],
        params: Value::Null,
        priority: 5,
        requires: None,
    };
    assert!(matches!(
        phone.runner.request(r.clone()),
        Err(Error::NotFound(_))
    ));
    r.requires = Some(vec![Capability::new("gpu"), Capability::new("stt:vi")]);
    let j = phone.runner.request(r).unwrap();
    assert_eq!(
        phone.runner.list(Some(JobStatus::Pending)).unwrap()[0]
            .job
            .id,
        j.id
    );
}
