//! The job runner: requests, claims (leases), runs, retries and finishes jobs through the
//! replicated event log, so work requested on one device can run on another after sync.
//!
//! Lifecycle (all as `Store` events):
//! ```text
//! job.requested ──► job.claimed{lease} ──► artifact.add* ──► job.finished{done}
//!       │                 │  (renewed while running)    └──► job.finished{failed|cancelled}
//!       │                 └── lease expires (device vanished) ──► another device reclaims
//!       └── device lacks capabilities (phone) ──► stays pending until a capable peer syncs
//! ```
//! Each device advertises capabilities (`llm`, `llm:qwen3.5:4b`, `gpu`, ...) plus the implicit
//! `device:<id>`, so a job can be pinned to one device (`RerunParams::allow_offload = false`).
//!
//! Re-running a processor produces artifacts that `supersede` the previous live artifact of the
//! same kind from the same processor; old versions stay for comparison.

use crate::jobs::{JobState, job_states};
use crate::planner::{PlanPolicy, latest_live, live_artifacts, plan};
use crate::registry::ProcessorRegistry;
use almond_types::api::{JobProgress, RerunParams};
use almond_types::event::EventBody;
use almond_types::job::{Capability, Job, JobLease, JobOutcome, JobStatus};
use almond_types::pipeline::JobCtx;
use almond_types::store::{BlobStore, Store};
use almond_types::time::{HlcClock, UnixMs, now_ms};
use almond_types::{Artifact, DeviceId, Error, Id, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Lease length; renewed every `lease_ms / 3` while running.
    pub lease_ms: i64,
    /// In-process attempts for transient errors (network/io/other).
    pub max_attempts: u32,
    /// Base backoff between attempts (doubles each time).
    pub retry_backoff: Duration,
    /// Claim rounds (across devices) after which an unfinished job is failed.
    pub max_claims: usize,
    pub job_timeout: Option<Duration>,
    /// After a job succeeds, request downstream jobs for the meeting.
    pub auto_plan: bool,
    pub policy: PlanPolicy,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        RunnerConfig {
            lease_ms: 15 * 60_000,
            max_attempts: 3,
            retry_backoff: Duration::from_secs(2),
            max_claims: 5,
            job_timeout: None,
            auto_plan: true,
            policy: PlanPolicy::default(),
        }
    }
}

/// Parameters for [`JobRunner::request`].
#[derive(Debug, Clone)]
pub struct JobRequest {
    pub meeting_id: Id,
    pub processor: String,
    pub inputs: Vec<Id>,
    pub params: Value,
    pub priority: i32,
    /// None = take from the processor descriptor.
    pub requires: Option<Vec<Capability>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunResult {
    Finished(JobOutcome),
    /// Not run (lost the claim race, inputs missing, already done elsewhere, ...).
    Skipped {
        job_id: Id,
        reason: String,
    },
}

type NowFn = Arc<dyn Fn() -> UnixMs + Send + Sync>;

/// Device policy: `true` = leave this job for another device for now (e.g. a laptop leaving
/// heavy jobs for the home PC). Asked only for jobs this device could claim; a job this device
/// already holds a lease on is never deferred.
pub type DeferFn = Arc<dyn Fn(&Job) -> bool + Send + Sync>;

pub struct JobRunner {
    store: Arc<dyn Store>,
    registry: Arc<ProcessorRegistry>,
    blobs: Arc<dyn BlobStore>,
    caps: RwLock<Vec<Capability>>,
    defer: RwLock<Option<DeferFn>>,
    cfg: RunnerConfig,
    clock: HlcClock,
    now: NowFn,
    running: Mutex<HashMap<Id, Arc<AtomicBool>>>,
    progress_tx: broadcast::Sender<JobProgress>,
}

fn is_transient(e: &Error) -> bool {
    matches!(e, Error::Network(_) | Error::Io(_) | Error::Other(_))
}

impl JobRunner {
    pub fn new(
        store: Arc<dyn Store>,
        registry: Arc<ProcessorRegistry>,
        blobs: Arc<dyn BlobStore>,
        capabilities: Vec<Capability>,
        cfg: RunnerConfig,
    ) -> Self {
        let (progress_tx, _) = broadcast::channel(256);
        JobRunner {
            store,
            registry,
            blobs,
            caps: RwLock::new(capabilities),
            defer: RwLock::new(None),
            cfg,
            clock: HlcClock::new(),
            now: Arc::new(now_ms),
            running: Mutex::new(HashMap::new()),
            progress_tx,
        }
    }

    /// Override the wall clock (tests: lease expiry).
    pub fn with_clock(mut self, now: impl Fn() -> UnixMs + Send + Sync + 'static) -> Self {
        self.now = Arc::new(now);
        self
    }

    pub fn device(&self) -> DeviceId {
        self.store.device_id()
    }

    pub fn registry(&self) -> &Arc<ProcessorRegistry> {
        &self.registry
    }

    /// Replace the advertised capabilities (e.g. after Ollama started or a model was pulled).
    pub fn set_capabilities(&self, caps: Vec<Capability>) {
        *self.caps.write().unwrap_or_else(|p| p.into_inner()) = caps;
    }

    /// Set (or clear) the device's defer policy, see [`DeferFn`].
    pub fn set_defer(&self, f: Option<DeferFn>) {
        *self.defer.write().unwrap() = f;
    }

    fn deferred(&self, job: &Job) -> bool {
        self.defer.read().unwrap().as_ref().is_some_and(|f| f(job))
    }

    /// Advertised capabilities incl. the implicit `device:<id>`.
    pub fn capabilities(&self) -> Vec<Capability> {
        let mut c = self.caps.read().unwrap_or_else(|p| p.into_inner()).clone();
        c.push(Capability::new(format!("device:{}", self.device())));
        c
    }

    /// Live progress / status updates for the UI (`job.progress` notifications).
    pub fn subscribe(&self) -> broadcast::Receiver<JobProgress> {
        self.progress_tx.subscribe()
    }

    fn emit(&self, job: &Job, status: JobStatus, progress: f32, message: &str) {
        let _ = self.progress_tx.send(JobProgress {
            job: job.clone(),
            status,
            device: Some(self.device()),
            progress,
            message: message.into(),
        });
    }

    /// Has the processor locally and every required capability.
    pub fn can_run(&self, job: &Job) -> bool {
        let caps = self.capabilities();
        self.registry.is_runnable(&job.processor)
            && job
                .requires
                .iter()
                .all(|r| caps.iter().any(|c| c.satisfies(r)))
    }

    // ------------------------------------------------------------------ requesting

    /// Append `job.requested`. Works for processors this device can't run (descriptor-only or
    /// explicit `requires`); a capable peer claims it after sync.
    pub fn request(&self, r: JobRequest) -> Result<Job> {
        let requires = match r.requires {
            Some(x) => x,
            None => {
                self.registry
                    .descriptor(&r.processor)
                    .ok_or_else(|| {
                        Error::NotFound(format!(
                            "processor {} (unknown here; pass `requires` explicitly)",
                            r.processor
                        ))
                    })?
                    .requires
            }
        };
        let job = Job {
            id: Id::new(),
            meeting_id: r.meeting_id,
            processor: r.processor,
            inputs: r.inputs,
            params: r.params,
            requires,
            requested_by: self.device(),
            requested_at: self.clock.now(),
            priority: r.priority,
        };
        self.store
            .append_local(EventBody::JobRequested(job.clone()))?;
        self.emit(&job, JobStatus::Pending, 0.0, "requested");
        Ok(job)
    }

    /// `artifacts.rerun`: same processor, (by default) current live inputs, new params.
    /// Outputs supersede the processor's previous artifacts.
    pub fn rerun(&self, p: RerunParams) -> Result<Job> {
        let d = self
            .registry
            .descriptor(&p.processor)
            .ok_or_else(|| Error::NotFound(format!("processor {}", p.processor)))?;
        let inputs = if p.inputs.is_empty() {
            let arts = self.store.list_artifacts(&p.meeting_id, None)?;
            d.consumes
                .iter()
                .map(|k| {
                    latest_live(&arts, k)
                        .map(|a| a.id.clone())
                        .ok_or_else(|| Error::NotFound(format!("no live {k} artifact")))
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            p.inputs
        };
        let mut requires = d.requires;
        if !p.allow_offload {
            requires.push(Capability::new(format!("device:{}", self.device())));
        }
        self.request(JobRequest {
            meeting_id: p.meeting_id,
            processor: p.processor,
            inputs,
            params: p.params,
            priority: self.cfg.policy.priority + 10, // user-initiated: ahead of background work
            requires: Some(requires),
        })
    }

    /// Request every job the DAG wants for `meeting` that doesn't exist yet.
    pub fn plan_meeting(&self, meeting: &Id) -> Result<Vec<Job>> {
        let Some(m) = self.store.get_meeting(meeting)? else {
            return Ok(vec![]);
        };
        let arts = self.store.list_artifacts(meeting, None)?;
        let states = job_states(self.store.as_ref())?;
        plan(&self.registry, &m, &arts, &states, &self.cfg.policy)
            .into_iter()
            .map(|p| {
                self.request(JobRequest {
                    meeting_id: meeting.clone(),
                    processor: p.processor,
                    inputs: p.inputs,
                    params: p.params,
                    priority: p.priority,
                    requires: Some(p.requires),
                })
            })
            .collect()
    }

    // ------------------------------------------------------------------ claiming

    pub fn job_states(&self) -> Result<BTreeMap<Id, JobState>> {
        job_states(self.store.as_ref())
    }

    /// `jobs.list` view.
    pub fn list(&self, status: Option<JobStatus>) -> Result<Vec<JobProgress>> {
        let now = (self.now)();
        Ok(self
            .job_states()?
            .into_values()
            .filter(|s| status.is_none_or(|x| s.status() == x))
            .map(|s| {
                let st = s.status();
                JobProgress {
                    device: s
                        .outcomes
                        .first()
                        .map(|o| o.device.clone())
                        .or_else(|| s.live_lease(now).map(|l| l.device.clone())),
                    progress: if st == JobStatus::Done { 1.0 } else { 0.0 },
                    message: s
                        .outcomes
                        .iter()
                        .find_map(|o| o.error.clone())
                        .unwrap_or_default(),
                    status: st,
                    job: s.job,
                }
            })
            .collect())
    }

    fn inputs_ready(&self, job: &Job) -> Result<bool> {
        let Some(m) = self.store.get_meeting(&job.meeting_id)? else {
            return Ok(false);
        };
        if !self.audio_ready(job, &m)? {
            return Ok(false);
        }
        for i in &job.inputs {
            if self.store.get_artifact(i)?.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// For audio processors: every audio track of the meeting is in the local blob store (blobs
    /// sync lazily, so a job may become runnable here only after the audio arrives).
    fn audio_ready(&self, job: &Job, m: &almond_types::Meeting) -> Result<bool> {
        let needs_audio = self
            .registry
            .descriptor(&job.processor)
            .is_some_and(|d| d.needs_audio);
        if !needs_audio {
            return Ok(true);
        }
        if m.audio.is_empty() {
            return Ok(false);
        }
        for t in &m.audio {
            if !self.blobs.has(&t.blob)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Jobs this device could claim now, highest priority first.
    pub fn claimable(&self) -> Result<Vec<Job>> {
        let now = (self.now)();
        let me = self.device();
        let running = self.running.lock().unwrap().clone();
        let mut v = vec![];
        for s in self.job_states()?.into_values() {
            if s.is_finished() || running.contains_key(&s.job.id) || !self.can_run(&s.job) {
                continue;
            }
            match s.live_lease(now) {
                Some(l) if l.device != me => continue, // someone else is on it
                Some(_) => {}                          // ours: never defer a job we hold
                None if self.deferred(&s.job) => continue, // left for a stronger peer
                None => {}
            }
            if !self.inputs_ready(&s.job)? {
                continue; // inputs not synced yet
            }
            v.push(s.job);
        }
        v.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then(a.requested_at.cmp(&b.requested_at))
        });
        Ok(v)
    }

    fn claim(&self, job: &Job) -> Result<JobLease> {
        let lease = JobLease {
            job_id: job.id.clone(),
            device: self.device(),
            claimed_at: self.clock.now(),
            expires_at: (self.now)() + self.cfg.lease_ms,
        };
        self.store
            .append_local(EventBody::JobClaimed(lease.clone()))?;
        Ok(lease)
    }

    fn finish(
        &self,
        job: &Job,
        status: JobStatus,
        outputs: Vec<Id>,
        error: Option<String>,
    ) -> Result<JobOutcome> {
        let o = JobOutcome {
            job_id: job.id.clone(),
            device: self.device(),
            status,
            outputs,
            error,
        };
        self.store.append_local(EventBody::JobFinished(o.clone()))?;
        self.emit(
            job,
            status,
            if status == JobStatus::Done { 1.0 } else { 0.0 },
            o.error.as_deref().unwrap_or("finished"),
        );
        Ok(o)
    }

    // ------------------------------------------------------------------ running

    /// Claim and run one job to completion (with retries). Returns `Skipped` if this device
    /// shouldn't run it (lost the claim race, inputs missing, finished elsewhere).
    pub async fn run_job(&self, job: &Job) -> Result<RunResult> {
        let skip = |reason: &str| {
            Ok(RunResult::Skipped {
                job_id: job.id.clone(),
                reason: reason.into(),
            })
        };
        let Some(proc_) = self.registry.get(&job.processor) else {
            return skip("processor not available on this device");
        };
        if !self.can_run(job) {
            return skip("missing capabilities");
        }
        let state = self.job_states()?.remove(&job.id);
        if let Some(s) = &state {
            if s.is_finished() {
                return skip("already finished");
            }
            if s.claim_count() >= self.cfg.max_claims {
                let o = self.finish(
                    job,
                    JobStatus::Failed,
                    vec![],
                    Some(format!("abandoned after {} claims", s.claim_count())),
                )?;
                return Ok(RunResult::Finished(o));
            }
        }
        let Some(meeting) = self.store.get_meeting(&job.meeting_id)? else {
            return skip("meeting not synced yet");
        };
        if !self.audio_ready(job, &meeting)? {
            return skip("audio not on this device yet");
        }
        let mut inputs: Vec<Artifact> = vec![];
        for id in &job.inputs {
            match self.store.get_artifact(id)? {
                Some(a) => inputs.push(a),
                None => return skip("inputs not synced yet"),
            }
        }

        self.claim(job)?;
        let me = self.device();
        if !self
            .job_states()?
            .get(&job.id)
            .is_some_and(|s| s.held_by(&me, (self.now)()))
        {
            return skip("lost claim to another device");
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.running
            .lock()
            .unwrap()
            .insert(job.id.clone(), cancel.clone());
        self.emit(job, JobStatus::Claimed, 0.0, "claimed");

        let res = self
            .attempts(job, proc_.as_ref(), &meeting, &inputs, cancel)
            .await;
        self.running.lock().unwrap().remove(&job.id);

        let outcome = match res {
            Ok(drafts) => {
                // Someone else already published results for this job (offline race)?
                if self.job_states()?.get(&job.id).is_some_and(|s| {
                    s.outcomes
                        .iter()
                        .any(|o| o.status == JobStatus::Done && o.device != me)
                }) {
                    return skip("finished elsewhere first; discarding duplicate outputs");
                }
                let ids = self.publish(job, drafts)?;
                let o = self.finish(job, JobStatus::Done, ids, None)?;
                if self.cfg.auto_plan {
                    self.plan_meeting(&job.meeting_id)?;
                }
                o
            }
            Err(Error::Cancelled) => {
                self.finish(job, JobStatus::Cancelled, vec![], Some("cancelled".into()))?
            }
            Err(e) => self.finish(job, JobStatus::Failed, vec![], Some(e.to_string()))?,
        };
        Ok(RunResult::Finished(outcome))
    }

    async fn attempts(
        &self,
        job: &Job,
        p: &dyn almond_types::pipeline::Processor,
        meeting: &almond_types::Meeting,
        inputs: &[Artifact],
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<almond_types::ArtifactDraft>> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let tx = self.progress_tx.clone();
            let (job2, dev) = (job.clone(), self.device());
            let ctx = JobCtx {
                job_id: job.id.clone(),
                meeting: meeting.clone(),
                params: job.params.clone(),
                cancel: cancel.clone(),
                progress: Arc::new(move |f, msg: &str| {
                    let _ = tx.send(JobProgress {
                        job: job2.clone(),
                        status: JobStatus::Claimed,
                        device: Some(dev.clone()),
                        progress: f,
                        message: msg.into(),
                    });
                }),
                blobs: self.blobs.clone(),
            };
            let res = self.run_with_renewal(job, p, &ctx, inputs.to_vec()).await;
            match res {
                Err(e) if is_transient(&e) && attempt < self.cfg.max_attempts => {
                    let wait = self.cfg.retry_backoff * 2u32.pow(attempt - 1);
                    self.emit(
                        job,
                        JobStatus::Claimed,
                        0.0,
                        &format!("attempt {attempt} failed ({e}); retrying in {wait:?}"),
                    );
                    tokio::time::sleep(wait).await;
                    ctx.check_cancel()?;
                    self.claim(job)?; // renew before the next attempt
                }
                other => return other,
            }
        }
    }

    async fn run_with_renewal(
        &self,
        job: &Job,
        p: &dyn almond_types::pipeline::Processor,
        ctx: &JobCtx,
        inputs: Vec<Artifact>,
    ) -> Result<Vec<almond_types::ArtifactDraft>> {
        let fut = p.run(ctx, inputs);
        let fut = async {
            match self.cfg.job_timeout {
                Some(t) => tokio::time::timeout(t, fut)
                    .await
                    .unwrap_or_else(|_| Err(Error::Network(format!("job timed out after {t:?}")))),
                None => fut.await,
            }
        };
        tokio::pin!(fut);
        let every = Duration::from_millis((self.cfg.lease_ms / 3).max(1_000) as u64);
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        loop {
            tokio::select! {
                r = &mut fut => return r,
                _ = tick.tick() => { self.claim(job)?; }
            }
        }
    }

    /// Persist drafts as artifacts (ids, HLC, device, supersedes, provenance inputs).
    fn publish(&self, job: &Job, drafts: Vec<almond_types::ArtifactDraft>) -> Result<Vec<Id>> {
        let existing = self.store.list_artifacts(&job.meeting_id, None)?;
        if let Some(max) = existing.iter().map(|a| a.created_at).max() {
            self.clock.observe(max);
        }
        let live = live_artifacts(&existing);
        let mut ids = vec![];
        for mut d in drafts {
            d.meeting_id = job.meeting_id.clone();
            if d.provenance.inputs.is_empty() {
                d.provenance.inputs = job.inputs.clone();
            }
            if d.provenance.processor.is_empty() {
                d.provenance.processor = job.processor.clone();
            }
            if d.supersedes.is_none() {
                d.supersedes = live
                    .iter()
                    .find(|a| a.kind == d.kind && a.provenance.processor == d.provenance.processor)
                    .map(|a| a.id.clone());
            }
            let a = d.into_artifact(Id::new(), self.clock.now(), self.device());
            ids.push(a.id.clone());
            self.store.append_local(EventBody::ArtifactAdd(a))?;
        }
        Ok(ids)
    }

    /// Claim + run everything runnable here until nothing is left (incl. downstream jobs the
    /// DAG adds as results arrive). Each job is attempted at most once per call.
    pub async fn run_pending(&self) -> Result<Vec<RunResult>> {
        let mut tried: HashSet<Id> = HashSet::new();
        let mut out = vec![];
        loop {
            let Some(job) = self
                .claimable()?
                .into_iter()
                .find(|j| !tried.contains(&j.id))
            else {
                return Ok(out);
            };
            tried.insert(job.id.clone());
            out.push(self.run_job(&job).await?);
        }
    }

    /// Cancel a job: signals it if running here, otherwise records `job.finished{cancelled}`.
    pub fn cancel(&self, id: &Id) -> Result<()> {
        if let Some(flag) = self.running.lock().unwrap().get(id) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }
        let s = self
            .job_states()?
            .remove(id)
            .ok_or_else(|| Error::NotFound(format!("job {id}")))?;
        if !s.is_finished() {
            self.finish(
                &s.job,
                JobStatus::Cancelled,
                vec![],
                Some("cancelled".into()),
            )?;
        }
        Ok(())
    }
}
