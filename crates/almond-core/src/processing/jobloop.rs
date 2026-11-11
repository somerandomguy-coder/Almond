//! The job loop.
//!
//! - **Scanner**: polls the event log (every 500 ms, or at once when kicked) from a cursor, so it
//!   sees local writes and events applied by sync alike. For every new event it emits
//!   `artifact.added` (artifacts), `job.progress` (other devices' job events), plans jobs for the
//!   meetings that changed (the planner is the only one requesting automatic jobs), updates the
//!   meeting title from a new `title` artifact, and wakes the worker.
//! - **Worker**: runs everything this device can claim (`JobRunner::run_pending`), when woken and
//!   every 15 s (expired leases of vanished devices, audio blobs that arrived by sync).
//! - **Progress forwarder**: runner progress → `job.progress`, plus the last value per job for
//!   `jobs.list`.
//! - Capabilities are re-detected every minute (Ollama started later, model pulled, ...), every
//!   20 s while a remote Ollama is configured (it comes and goes with the network).

use crate::Core;
use almond_pipeline::planner::latest_live;
use almond_types::api::{JobProgress, notifications};
use almond_types::artifact::body::Title;
use almond_types::event::{Event, EventBody};
use almond_types::job::JobStatus;
use almond_types::store::{MeetingQuery, Store};
use almond_types::sync::VersionVector;
use almond_types::{Artifact, ArtifactKind, DeviceId, Id, Result};
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, broadcast};

const SCAN_EVERY: Duration = Duration::from_millis(500);
const WORK_EVERY: Duration = Duration::from_secs(15);
const CAPS_EVERY: Duration = Duration::from_secs(60);
/// With a remote Ollama configured: notice quickly when it comes and goes (laptop leaves home).
const CAPS_EVERY_REMOTE: Duration = Duration::from_secs(20);
const BATCH: usize = 2_000;

pub(crate) fn start(core: &Arc<Core>) {
    let st = &core.processing;
    let work = Arc::new(Notify::new());
    st.spawn(forward_progress(
        Arc::downgrade(core),
        st.runner.subscribe(),
    ));
    st.spawn(scanner(Arc::downgrade(core), work.clone()));
    st.spawn(worker(Arc::downgrade(core), work));
}

async fn forward_progress(core: Weak<Core>, mut rx: broadcast::Receiver<JobProgress>) {
    loop {
        let p = match rx.recv().await {
            Ok(p) => p,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let Some(core) = core.upgrade() else { return };
        {
            let mut m = core.processing.progress.lock().unwrap();
            match p.status {
                JobStatus::Pending | JobStatus::Claimed => {
                    m.insert(p.job.id.clone(), p.clone());
                }
                _ => {
                    m.remove(&p.job.id);
                }
            }
        }
        core.notifier.emit(notifications::JOB_PROGRESS, &p);
    }
}

/// Event-log cursor: the contiguous prefix per device plus events seen beyond it (sync may
/// deliver non-contiguous events).
struct Cursor {
    vv: VersionVector,
    seen: HashSet<(DeviceId, u64)>,
}

impl Cursor {
    fn mark(&mut self, ev: &Event) {
        self.seen.insert((ev.device.clone(), ev.seq));
        loop {
            let next = (ev.device.clone(), self.vv.get(&ev.device) + 1);
            if !self.seen.remove(&next) {
                break;
            }
            self.vv.observe(&next.0, next.1);
        }
    }
}

async fn scanner(weak: Weak<Core>, work: Arc<Notify>) {
    let (mut cursor, wake) = {
        let Some(core) = weak.upgrade() else { return };
        core.processing.refresh_capabilities(&core).await;
        let vv = match core.store().version_vector() {
            Ok(vv) => vv,
            Err(e) => {
                tracing::error!("processing: cannot read the event log: {e}");
                return;
            }
        };
        if let Err(e) = plan_all(&core) {
            tracing::warn!("processing: initial planning failed: {e}");
        }
        work.notify_one();
        (
            Cursor {
                vv,
                seen: HashSet::new(),
            },
            core.processing.wake.clone(),
        )
    };
    let mut caps_at = Instant::now();
    loop {
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(SCAN_EVERY) => {}
        }
        let Some(core) = weak.upgrade() else { return };
        let every = if super::remote_ollama_setting(&core).is_some() {
            CAPS_EVERY_REMOTE
        } else {
            CAPS_EVERY
        };
        if caps_at.elapsed() >= every {
            caps_at = Instant::now();
            core.processing.refresh_capabilities(&core).await;
        }
        match scan_once(&core, &mut cursor) {
            Ok(true) => work.notify_one(),
            Ok(false) => {}
            Err(e) => tracing::warn!("processing: scan failed: {e}"),
        }
    }
}

async fn worker(weak: Weak<Core>, work: Arc<Notify>) {
    loop {
        tokio::select! {
            _ = work.notified() => {}
            _ = tokio::time::sleep(WORK_EVERY) => {}
        }
        let Some(runner) = weak.upgrade().map(|c| c.processing.runner.clone()) else {
            return;
        };
        match runner.run_pending().await {
            Ok(results) => {
                for r in results {
                    if let almond_pipeline::RunResult::Finished(o) = r
                        && let Some(e) = &o.error
                    {
                        tracing::warn!("job {} {:?}: {e}", o.job_id, o.status);
                    }
                }
            }
            Err(e) => tracing::warn!("processing: running jobs failed: {e}"),
        }
    }
}

/// Plan every meeting (at start: catches work requested while the app was closed).
fn plan_all(core: &Core) -> Result<usize> {
    let mut n = 0;
    for m in core.store().list_meetings(&MeetingQuery::default())? {
        n += plan(core, &m.id)?;
    }
    Ok(n)
}

/// Request the jobs the DAG wants for `meeting`. Returns how many were requested.
pub(crate) fn plan(core: &Core, meeting: &Id) -> Result<usize> {
    let _g = core.processing.plan_lock.lock().unwrap();
    Ok(core.processing.runner.plan_meeting(meeting)?.len())
}

/// Process new events. Returns true if there may be work for the worker.
fn scan_once(core: &Core, cur: &mut Cursor) -> Result<bool> {
    let me = core.device_id().clone();
    let mut dirty: BTreeSet<Id> = BTreeSet::new();
    let mut remote_jobs: Vec<(Id, DeviceId)> = vec![];
    let mut work = false;
    let mut local_new = false;
    loop {
        let evs = core.store().events_missing(&cur.vv, BATCH)?;
        let full = evs.len() >= BATCH;
        let fresh: Vec<Event> = evs
            .into_iter()
            .filter(|e| !cur.seen.contains(&(e.device.clone(), e.seq)))
            .collect();
        if fresh.is_empty() {
            break;
        }
        for ev in &fresh {
            cur.mark(ev);
            let local = ev.device == me;
            local_new |= local;
            let body = match EventBody::decode(ev) {
                Ok(Some(b)) => b,
                Ok(None) => continue, // unknown kind from a newer device
                Err(e) => {
                    tracing::warn!("undecodable {} event: {e}", ev.kind.0);
                    continue;
                }
            };
            match body {
                EventBody::ArtifactAdd(a) => {
                    core.notifier.emit(notifications::ARTIFACT_ADDED, &a);
                    if a.kind.is(ArtifactKind::TITLE)
                        && local
                        && let Err(e) = apply_title(core, &a)
                    {
                        tracing::warn!("updating meeting title: {e}");
                    }
                    dirty.insert(a.meeting_id);
                }
                EventBody::MeetingUpsert(m) => {
                    dirty.insert(m.id);
                }
                EventBody::BlobAvailable { .. } => work = true,
                EventBody::JobRequested(j) => {
                    work = true;
                    if !local {
                        remote_jobs.push((j.id, ev.device.clone()));
                    }
                }
                EventBody::JobClaimed(l) => {
                    work = true;
                    if !local {
                        remote_jobs.push((l.job_id, ev.device.clone()));
                    }
                }
                EventBody::JobFinished(o) => {
                    work = true;
                    if !local {
                        remote_jobs.push((o.job_id, ev.device.clone()));
                    }
                }
                _ => {}
            }
        }
        if !full {
            break;
        }
    }
    for m in &dirty {
        if plan(core, m)? > 0 {
            work = true;
        }
    }
    if !remote_jobs.is_empty() {
        let states = core.processing.runner.job_states()?;
        for (id, dev) in remote_jobs {
            if let Some(s) = states.get(&id) {
                let status = s.status();
                core.notifier.emit(
                    notifications::JOB_PROGRESS,
                    &JobProgress {
                        job: s.job.clone(),
                        status,
                        device: Some(dev),
                        progress: if status == JobStatus::Done { 1.0 } else { 0.0 },
                        message: s
                            .outcomes
                            .iter()
                            .find_map(|o| o.error.clone())
                            .unwrap_or_else(|| "synced".into()),
                    },
                );
            }
        }
    }
    if local_new {
        core.node.engine.notify_local_change();
    }
    Ok(work)
}

/// A new `title` artifact names the meeting unless the user named it: the title is empty or
/// still one that an earlier `title` artifact set.
fn apply_title(core: &Core, a: &Artifact) -> Result<()> {
    let t: Title = a.decode()?;
    let new = t.title.trim();
    let Some(mut m) = core.store().get_meeting(&a.meeting_id)? else {
        return Ok(());
    };
    let titles = core
        .store()
        .list_artifacts(&m.id, Some(ArtifactKind::TITLE))?;
    if latest_live(&titles, ArtifactKind::TITLE).is_some_and(|l| l.id != a.id) {
        return Ok(()); // superseded already
    }
    let cur = m.title.trim();
    let ours = cur.is_empty()
        || titles
            .iter()
            .any(|x| x.id != a.id && x.decode::<Title>().is_ok_and(|t| t.title.trim() == cur));
    if !ours || cur == new || new.is_empty() {
        return Ok(());
    }
    m.title = new.to_string();
    core.store().append_local(EventBody::MeetingUpsert(m))?;
    Ok(())
}
