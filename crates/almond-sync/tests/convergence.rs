//! Randomized convergence: 3 SQLite stores syncing over the in-process `MemNetwork`, with random
//! partitions, concurrent edits of the same records, out-of-order third-party events (gaps),
//! notes CRDT edits, jobs, calendar, and blobs with different per-device blob policies.

use almond_store::{FsBlobStore, SqliteStore, notes_update_event};
use almond_sync::{BlobPolicy, SyncEngine, SyncOptions};
use almond_types::event::{Event, EventBody, EventKind};
use almond_types::job::{Job, JobLease, JobOutcome, JobStatus};
use almond_types::mock::{MemNetwork, MockCalendar, fixtures};
use almond_types::store::{BlobStore, MeetingQuery, Store};
use almond_types::sync::{DeviceInfo, Transport, VersionVector};
use almond_types::{BlobHash, DeviceId, Hlc, Id};
use std::sync::Arc;

/// xorshift64* â€” deterministic, no extra deps.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

struct Dev {
    info: DeviceInfo,
    store: Arc<SqliteStore>,
    blobs: Arc<FsBlobStore>,
    engine: Arc<SyncEngine>,
    policy: BlobPolicy,
}

fn opts(policy: BlobPolicy) -> SyncOptions {
    SyncOptions {
        blob_policy: policy,
        batch_events: 7, // tiny batches â†’ many round trips, exercises cursors
        blob_chunk: 1000,
        ..Default::default()
    }
}

fn setup(net: &MemNetwork, dir: &std::path::Path, policies: &[BlobPolicy]) -> Vec<Dev> {
    policies
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let info = fixtures::device(&format!("d{i}"));
            let store = Arc::new(SqliteStore::open_in_memory(info.id.clone()).unwrap());
            let blobs = Arc::new(FsBlobStore::open(dir.join(format!("blobs{i}"))).unwrap());
            let t: Arc<dyn Transport> = Arc::new(net.transport(info.clone()));
            let engine = SyncEngine::new(store.clone(), blobs.clone(), t, info.clone(), opts(*p));
            engine.spawn_server();
            Dev {
                info,
                store,
                blobs,
                engine,
                policy: *p,
            }
        })
        .collect()
}

const NOTES_DOC: &str = "01JNOTESDOC0000000000000000";
const SOURCES: [&str; 2] = ["google:a@x.com", "phone:dev"];

fn random_op(rng: &mut Rng, d: &Dev, round: u64) {
    let s = &d.store;
    let meetings = s.list_meetings(&MeetingQuery::default()).unwrap();
    match rng.below(9) {
        0 => {
            let mut m = fixtures::meeting(rng.below(1_000_000) as i64);
            m.title = format!("meeting r{round} by {}", d.info.name);
            s.append_local(EventBody::MeetingUpsert(m)).unwrap();
        }
        1 if !meetings.is_empty() => {
            // concurrent edits of the same meeting on several devices â†’ LWW
            let mut m = meetings[rng.below(meetings.len() as u64) as usize].clone();
            m.title = format!("edit r{round} by {}", d.info.name);
            m.tags = vec![format!("t{}", rng.below(3))];
            s.append_local(EventBody::MeetingUpsert(m)).unwrap();
        }
        2 if !meetings.is_empty() && rng.chance(40) => {
            let m = &meetings[rng.below(meetings.len() as u64) as usize];
            s.append_local(EventBody::MeetingDelete { id: m.id.clone() })
                .unwrap();
        }
        3 if !meetings.is_empty() => {
            let m = &meetings[rng.below(meetings.len() as u64) as usize];
            s.append_local(EventBody::ArtifactAdd(fixtures::transcript(
                &m.id, &d.info.id,
            )))
            .unwrap();
        }
        4 => {
            let doc = s.notes_doc(&Id(NOTES_DOC.into())).unwrap();
            doc.set_peer_id(rng.next()).unwrap();
            let vv = doc.oplog_vv();
            let t = doc.get_text("body");
            let pos = rng.below(t.len_unicode() as u64 + 1) as usize;
            t.insert(pos, &format!("[{}{round}]", d.info.name)).unwrap();
            doc.commit();
            let u = almond_store::export_since(&doc, &vv).unwrap();
            s.append_local(notes_update_event(&Id(NOTES_DOC.into()), &u))
                .unwrap();
        }
        5 => {
            let src = SOURCES[rng.below(2) as usize];
            let mut e = MockCalendar::with_upcoming(src, 1_000_000, 0).events[0].clone();
            e.event_id = format!("e{}", rng.below(4));
            e.title = format!("cal r{round} by {}", d.info.name);
            if rng.chance(25) {
                s.append_local(EventBody::CalendarDelete {
                    source: e.source,
                    event_id: e.event_id,
                })
                .unwrap();
            } else {
                s.append_local(EventBody::CalendarUpsert(e)).unwrap();
            }
        }
        6 => {
            let jobs = s.jobs(None).unwrap();
            if jobs.is_empty() || rng.chance(30) {
                s.append_local(EventBody::JobRequested(Job {
                    id: Id::new(),
                    meeting_id: Id::new(),
                    processor: "summarize".into(),
                    inputs: vec![],
                    params: serde_json::json!({}),
                    requires: vec![],
                    requested_by: d.info.id.clone(),
                    requested_at: Hlc::default(),
                    priority: rng.below(3) as i32,
                }))
                .unwrap();
            } else {
                let (j, _) = &jobs[rng.below(jobs.len() as u64) as usize];
                if rng.chance(50) {
                    s.append_local(EventBody::JobClaimed(JobLease {
                        job_id: j.id.clone(),
                        device: d.info.id.clone(),
                        claimed_at: Hlc {
                            wall_ms: rng.below(1000) as i64,
                            logical: 0,
                        },
                        expires_at: i64::MAX,
                    }))
                    .unwrap();
                } else {
                    s.append_local(EventBody::JobFinished(JobOutcome {
                        job_id: j.id.clone(),
                        device: d.info.id.clone(),
                        status: if rng.chance(70) {
                            JobStatus::Done
                        } else {
                            JobStatus::Failed
                        },
                        outputs: vec![],
                        error: None,
                    }))
                    .unwrap();
                }
            }
        }
        7 => {
            let len = 1 + rng.below(5000) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            let h = d.blobs.put(&bytes).unwrap();
            let mime = if rng.chance(50) {
                "audio/ogg"
            } else {
                "text/markdown"
            };
            s.append_local(EventBody::BlobAvailable {
                hash: h,
                size: len as u64,
                mime: mime.into(),
            })
            .unwrap();
        }
        _ => {}
    }
}

/// Events authored by a device that is not part of the test, delivered out of order to random
/// stores (exercises gap buffering + relaying of unknown kinds).
fn ghost_event(seq: u64) -> Event {
    Event {
        device: DeviceId("ghost".into()),
        seq,
        hlc: Hlc {
            wall_ms: seq as i64,
            logical: 0,
        },
        kind: EventKind(if seq.is_multiple_of(2) {
            "future.kind".into()
        } else {
            EventKind::MEETING_UPSERT.into()
        }),
        body: if seq.is_multiple_of(2) {
            serde_json::json!({"seq": seq})
        } else {
            serde_json::to_value(fixtures::meeting(seq as i64)).unwrap()
        },
    }
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    vv: VersionVector,
    events: u64,
    meetings: Vec<String>,
    artifacts: Vec<Id>,
    calendar: Vec<(String, String, String)>,
    jobs: Vec<(Id, JobStatus)>,
    notes: String,
    blobs: Vec<BlobHash>,
}

fn snapshot(d: &Dev) -> Snapshot {
    let s = &d.store;
    let meetings = s.list_meetings(&MeetingQuery::default()).unwrap();
    let mut artifacts = vec![];
    for m in &meetings {
        artifacts.extend(
            s.list_artifacts(&m.id, None)
                .unwrap()
                .into_iter()
                .map(|a| a.id),
        );
    }
    Snapshot {
        vv: s.version_vector().unwrap(),
        events: s.event_count().unwrap(),
        meetings: meetings
            .iter()
            .map(|m| serde_json::to_string(m).unwrap())
            .collect(),
        artifacts,
        calendar: s
            .calendar_events(i64::MIN, i64::MAX)
            .unwrap()
            .into_iter()
            .map(|e| (e.source, e.event_id, e.title))
            .collect(),
        jobs: s
            .jobs(None)
            .unwrap()
            .into_iter()
            .map(|(j, st)| (j.id, st))
            .collect(),
        notes: s
            .notes_doc(&Id(NOTES_DOC.into()))
            .unwrap()
            .get_text("body")
            .to_string(),
        blobs: s
            .known_blobs()
            .unwrap()
            .into_iter()
            .map(|b| b.hash)
            .collect(),
    }
}

async fn run_seed(seed: u64) {
    let dir = tempfile::tempdir().unwrap();
    let net = MemNetwork::new();
    let devs = setup(
        &net,
        dir.path(),
        &[
            BlobPolicy::Everything,
            BlobPolicy::Everything,
            BlobPolicy::TextOnly,
        ],
    );
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let ghosts = 12u64;
    let mut ghost_order: Vec<u64> = (1..=ghosts).collect();
    for i in (1..ghost_order.len()).rev() {
        ghost_order.swap(i, rng.below(i as u64 + 1) as usize);
    }

    for round in 0..14u64 {
        for d in &devs {
            for _ in 0..rng.below(4) {
                random_op(&mut rng, d, round);
            }
        }
        if let Some(g) = ghost_order.pop() {
            let d = &devs[rng.below(3) as usize];
            d.store.apply_remote(vec![ghost_event(g)]).unwrap();
        }
        // random partition: each directed link is up with p = 0.4
        for i in 0..3 {
            for j in 0..3 {
                if i != j && rng.chance(40) {
                    let r = devs[i].engine.sync_with(&devs[j].info.id, None).await;
                    assert!(r.error.is_none(), "seed {seed}: {r:?}");
                }
            }
        }
    }
    while let Some(g) = ghost_order.pop() {
        devs[0].store.apply_remote(vec![ghost_event(g)]).unwrap();
    }

    // heal: everyone presses Sync (one pass each is enough thanks to the relay pass)
    for d in &devs {
        let reports = d.engine.sync_now(None, None).await;
        assert_eq!(reports.len(), 2);
        assert!(reports.iter().all(|r| r.error.is_none()), "{reports:?}");
    }

    let snaps: Vec<Snapshot> = devs.iter().map(snapshot).collect();
    assert_eq!(snaps[0], snaps[1], "seed {seed}: d0 vs d1");
    assert_eq!(snaps[0], snaps[2], "seed {seed}: d0 vs d2");
    assert_eq!(
        snaps[0].vv.get(&DeviceId("ghost".into())),
        ghosts,
        "gaps filled"
    );

    // blob policies: PCs have everything, the phone has only non-audio blobs
    for b in devs[0].store.known_blobs().unwrap() {
        for d in &devs {
            // a device always has blobs it authored itself
            let want = d.policy.auto_fetch(&b.mime) || b.holders.contains(&d.info.id);
            assert_eq!(
                d.blobs.has(&b.hash).unwrap(),
                want,
                "seed {seed} {} {}",
                d.info.name,
                b.mime
            );
            if want {
                assert_eq!(d.blobs.get(&b.hash).unwrap().unwrap().len() as u64, b.size);
            }
        }
    }
    // audio on demand on the phone
    if let Some(audio) = devs[0]
        .store
        .known_blobs()
        .unwrap()
        .into_iter()
        .find(|b| b.mime.starts_with("audio/") && !b.holders.contains(&devs[2].info.id))
    {
        devs[2].engine.request_blob(audio.hash.clone());
        let r = devs[2].engine.sync_with(&devs[0].info.id, None).await;
        assert!(r.error.is_none());
        assert_eq!(r.blobs_received, 1);
        assert!(devs[2].blobs.has(&audio.hash).unwrap());
    }

    // a second sync is a no-op
    let r = devs[0].engine.sync_with(&devs[1].info.id, None).await;
    assert_eq!((r.events_received, r.events_sent), (0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_stores_converge_under_random_partitions() {
    for seed in 1..=8 {
        run_seed(seed).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resumes_interrupted_blob_download() {
    let dir = tempfile::tempdir().unwrap();
    let net = MemNetwork::new();
    let devs = setup(
        &net,
        dir.path(),
        &[BlobPolicy::Everything, BlobPolicy::Everything],
    );
    let big: Vec<u8> = (0..50_000u32).map(|i| (i * 7 % 256) as u8).collect();
    let h = devs[0].blobs.put(&big).unwrap();
    devs[0]
        .store
        .append_local(EventBody::BlobAvailable {
            hash: h.clone(),
            size: big.len() as u64,
            mime: "audio/ogg".into(),
        })
        .unwrap();
    // simulate an earlier interrupted download: first 12_000 bytes already on disk
    devs[1].blobs.write_partial(&h, 0, &big[..12_000]).unwrap();
    let r = devs[1].engine.sync_with(&devs[0].info.id, None).await;
    assert!(r.error.is_none(), "{r:?}");
    assert_eq!(r.blobs_received, 1);
    assert_eq!(devs[1].blobs.get(&h).unwrap().unwrap(), big);
    assert_eq!(devs[1].blobs.partial_len(&h).unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn progress_and_auto_sync() {
    let dir = tempfile::tempdir().unwrap();
    let net = MemNetwork::new();
    let devs = setup(
        &net,
        dir.path(),
        &[BlobPolicy::Everything, BlobPolicy::Everything],
    );
    for i in 0..30 {
        devs[0]
            .store
            .append_local(EventBody::MeetingUpsert(fixtures::meeting(i)))
            .unwrap();
    }
    let seen = Arc::new(std::sync::Mutex::new(vec![]));
    let seen2 = seen.clone();
    let progress: almond_sync::ProgressFn =
        Arc::new(move |p| seen2.lock().unwrap().push(p.clone()));
    let reports = devs[1].engine.sync_now(None, Some(progress)).await;
    assert_eq!(reports[0].events_received, 30);
    let seen = seen.lock().unwrap().clone();
    assert!(matches!(
        seen.first(),
        Some(almond_sync::SyncProgress::Started { .. })
    ));
    assert!(matches!(
        seen.last(),
        Some(almond_sync::SyncProgress::Finished(_))
    ));
    assert!(
        seen.iter()
            .filter(|p| matches!(p, almond_sync::SyncProgress::Events { .. }))
            .count()
            >= 5
    );

    // auto-sync while open picks up a local change quickly
    let handle = devs[0]
        .engine
        .start_auto_sync(std::time::Duration::from_secs(3600), None);
    let m = fixtures::meeting(99);
    devs[0]
        .store
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    devs[0].engine.notify_local_change();
    let mut ok = false;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if devs[1].store.get_meeting(&m.id).unwrap().is_some() {
            ok = true;
            break;
        }
    }
    drop(handle);
    assert!(ok, "auto-sync delivered the change");
}
