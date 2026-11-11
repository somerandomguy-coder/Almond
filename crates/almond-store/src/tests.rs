use crate::*;
use almond_types::calendar::CalendarEvent;
use almond_types::event::{Event, EventBody, EventKind};
use almond_types::job::{Job, JobLease, JobOutcome, JobStatus};
use almond_types::mock::fixtures;
use almond_types::store::{MeetingQuery, Store};
use almond_types::sync::VersionVector;
use almond_types::time::now_ms;
use almond_types::{DeviceId, Hlc, Id};

fn dev(s: &str) -> DeviceId {
    DeviceId(s.into())
}

fn store(s: &str) -> SqliteStore {
    SqliteStore::open_in_memory(dev(s)).unwrap()
}

fn ev(device: &str, seq: u64, wall: i64, body: EventBody) -> Event {
    Event {
        device: dev(device),
        seq,
        hlc: Hlc {
            wall_ms: wall,
            logical: 0,
        },
        kind: EventKind(body.kind().into()),
        body: body.to_json().unwrap(),
    }
}

#[test]
fn two_stores_converge_and_apply_is_idempotent() {
    let a = store("a");
    let b = store("b");
    let m = fixtures::meeting(1_000);
    a.append_local(EventBody::MeetingUpsert(m.clone())).unwrap();
    let t = fixtures::transcript(&m.id, &a.device_id());
    a.append_local(EventBody::ArtifactAdd(t)).unwrap();

    let missing = a.events_missing(&b.version_vector().unwrap(), 100).unwrap();
    assert_eq!(missing.len(), 2);
    assert_eq!(b.apply_remote(missing.clone()).unwrap(), 2);
    assert_eq!(b.apply_remote(missing).unwrap(), 0, "idempotent");
    assert_eq!(b.get_meeting(&m.id).unwrap().unwrap(), m);
    assert_eq!(
        b.list_artifacts(&m.id, Some("transcript")).unwrap().len(),
        1
    );
    assert_eq!(b.version_vector().unwrap(), a.version_vector().unwrap());
    assert!(
        a.events_missing(&b.version_vector().unwrap(), 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn version_vector_only_advances_over_contiguous_seqs() {
    let s = store("me");
    let m = |i: i64| EventBody::MeetingUpsert(fixtures::meeting(i));
    assert_eq!(s.apply_remote(vec![ev("x", 3, 3, m(3))]).unwrap(), 1);
    assert_eq!(s.version_vector().unwrap().get(&dev("x")), 0);
    s.apply_remote(vec![ev("x", 1, 1, m(1))]).unwrap();
    assert_eq!(s.version_vector().unwrap().get(&dev("x")), 1);
    // buffered seq 3 is still served to others (they may need it)
    let all = s.events_missing(&VersionVector::default(), 10).unwrap();
    assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 3]);
    s.apply_remote(vec![ev("x", 2, 2, m(2))]).unwrap();
    assert_eq!(s.version_vector().unwrap().get(&dev("x")), 3);
    assert_eq!(s.list_meetings(&MeetingQuery::default()).unwrap().len(), 3);
    // seq 0 is invalid and ignored
    assert_eq!(s.apply_remote(vec![ev("x", 0, 9, m(9))]).unwrap(), 0);
}

#[test]
fn events_missing_respects_limit_and_order() {
    let s = store("me");
    for i in 1..=5 {
        s.apply_remote(vec![ev(
            "b",
            i,
            i as i64,
            EventBody::MeetingUpsert(fixtures::meeting(0)),
        )])
        .unwrap();
        s.apply_remote(vec![ev(
            "a",
            i,
            i as i64,
            EventBody::MeetingUpsert(fixtures::meeting(0)),
        )])
        .unwrap();
    }
    let mut have = VersionVector::default();
    have.observe(&dev("a"), 3);
    let got = s.events_missing(&have, 4).unwrap();
    let keys: Vec<_> = got.iter().map(|e| (e.device.0.as_str(), e.seq)).collect();
    assert_eq!(keys, vec![("a", 4), ("a", 5), ("b", 1), ("b", 2)]);
}

#[test]
fn meeting_lww_and_tombstones_are_order_independent() {
    let mut m1 = fixtures::meeting(0);
    m1.title = "from x".into();
    let mut m2 = m1.clone();
    m2.title = "from y".into();
    let e1 = ev("x", 1, 100, EventBody::MeetingUpsert(m1.clone()));
    let e2 = ev("y", 1, 100, EventBody::MeetingUpsert(m2.clone())); // same hlc: device breaks tie
    let del = ev("z", 1, 50, EventBody::MeetingDelete { id: m1.id.clone() }); // older delete loses

    let orders = [
        vec![e1.clone(), e2.clone(), del.clone()],
        vec![del.clone(), e2.clone(), e1.clone()],
        vec![e2.clone(), del.clone(), e1.clone()],
    ];
    for o in orders {
        let s = store("me");
        for e in o {
            s.apply_remote(vec![e]).unwrap();
        }
        assert_eq!(s.get_meeting(&m1.id).unwrap().unwrap().title, "from y");
    }
    // a newer delete wins regardless of order
    let del2 = ev("z", 2, 200, EventBody::MeetingDelete { id: m1.id.clone() });
    for o in [
        vec![e1.clone(), del2.clone()],
        vec![del2.clone(), e1.clone()],
    ] {
        let s = store("me");
        s.apply_remote(o).unwrap();
        assert!(s.get_meeting(&m1.id).unwrap().is_none());
        assert!(
            s.list_meetings(&MeetingQuery::default())
                .unwrap()
                .is_empty()
        );
    }
}

fn cal(id: &str, start: i64, title: &str) -> CalendarEvent {
    let mut e =
        almond_types::mock::MockCalendar::with_upcoming("google:a@b.c", start, 0).events[0].clone();
    e.event_id = id.into();
    e.title = title.into();
    e
}

#[test]
fn calendar_lww_with_deletes() {
    let up1 = ev(
        "x",
        1,
        10,
        EventBody::CalendarUpsert(cal("e1", 1_000, "v1")),
    );
    let up2 = ev(
        "x",
        2,
        20,
        EventBody::CalendarUpsert(cal("e1", 1_000, "v2")),
    );
    let del = ev(
        "y",
        1,
        15,
        EventBody::CalendarDelete {
            source: "google:a@b.c".into(),
            event_id: "e1".into(),
        },
    );
    for o in [
        vec![up1.clone(), del.clone(), up2.clone()],
        vec![up2.clone(), up1.clone(), del.clone()],
    ] {
        let s = store("me");
        for e in o {
            s.apply_remote(vec![e]).unwrap();
        }
        let v = s.calendar_events(0, 10_000_000).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].title, "v2");
    }
    let s = store("me");
    s.apply_remote(vec![up1, del]).unwrap();
    assert!(s.calendar_events(0, 10_000_000).unwrap().is_empty());
}

#[test]
fn job_state_machine() {
    let job = Job {
        id: Id::new(),
        meeting_id: Id::new(),
        processor: "summarize.ollama".into(),
        inputs: vec![],
        params: serde_json::json!({}),
        requires: vec![],
        requested_by: dev("phone"),
        requested_at: Hlc::default(),
        priority: 0,
    };
    let lease = |d: &str, wall: i64, expires: i64| JobLease {
        job_id: job.id.clone(),
        device: dev(d),
        claimed_at: Hlc {
            wall_ms: wall,
            logical: 0,
        },
        expires_at: expires,
    };
    let far = now_ms() + 3_600_000;
    let req = ev("phone", 1, 1, EventBody::JobRequested(job.clone()));
    let c_pc = ev("pc", 1, 5, EventBody::JobClaimed(lease("pc", 5, far)));
    let c_lap = ev("lap", 1, 6, EventBody::JobClaimed(lease("lap", 6, far)));
    let done = ev(
        "pc",
        2,
        9,
        EventBody::JobFinished(JobOutcome {
            job_id: job.id.clone(),
            device: dev("pc"),
            status: JobStatus::Done,
            outputs: vec![],
            error: None,
        }),
    );

    // claims arriving before the request still count once it shows up
    let s = store("me");
    s.apply_remote(vec![c_lap.clone(), c_pc.clone()]).unwrap();
    assert!(s.jobs(None).unwrap().is_empty());
    s.apply_remote(vec![req.clone()]).unwrap();
    assert_eq!(s.jobs(None).unwrap()[0].1, JobStatus::Claimed);
    assert_eq!(
        s.job_lease(&job.id).unwrap().unwrap().device,
        dev("pc"),
        "lowest claim wins"
    );
    s.apply_remote(vec![done]).unwrap();
    assert_eq!(s.jobs(Some(JobStatus::Done)).unwrap().len(), 1);
    assert!(s.jobs(Some(JobStatus::Pending)).unwrap().is_empty());

    // expired lease → back to pending
    let s = store("me2");
    let expired = ev("pc", 1, 5, EventBody::JobClaimed(lease("pc", 5, 1)));
    s.apply_remote(vec![req, expired]).unwrap();
    assert_eq!(s.jobs(None).unwrap()[0].1, JobStatus::Pending);
}

#[test]
fn search_folds_vietnamese_diacritics() {
    let s = store("a");
    let mut m = fixtures::meeting(1_000);
    m.title = "Họp kế hoạch Q4".into();
    s.append_local(EventBody::MeetingUpsert(m.clone())).unwrap();
    let t = fixtures::transcript(&m.id, &s.device_id());
    let tid = t.id.clone();
    s.append_local(EventBody::ArtifactAdd(t.clone())).unwrap();
    s.append_local(EventBody::ArtifactAdd(fixtures::summary(
        &t,
        &s.device_id(),
    )))
    .unwrap();

    // "hop" (no diacritics) finds "Họp"
    let hits = s.search("hop", 10).unwrap();
    assert!(
        hits.iter()
            .any(|h| h.kind == "meeting" && h.snippet.contains("«Họp»")),
        "{hits:?}"
    );
    // "dong" finds "đồng" (đ is not a combining mark; folded by us)
    let hits = s.search("dong", 10).unwrap();
    let seg = hits
        .iter()
        .find(|h| h.artifact_id.as_ref() == Some(&tid))
        .expect("transcript hit");
    assert_eq!(seg.segment, Some(1));
    assert!(seg.snippet.contains("«đồng»"), "{}", seg.snippet);
    assert!(hits.iter().any(|h| h.kind == "summary"));
    // multi-token, prefix, case-insensitive, English
    assert!(!s.search("ngan sach", 10).unwrap().is_empty());
    assert!(!s.search("BACKEND engin", 10).unwrap().is_empty());
    assert!(s.search("xyzzy", 10).unwrap().is_empty());
    assert!(
        s.search("  \"*( ", 10).unwrap().is_empty(),
        "syntax-only query is safe"
    );

    // list_meetings text filter uses the same index
    let q = MeetingQuery {
        text: Some("ngân sach".into()),
        ..Default::default()
    };
    assert_eq!(s.list_meetings(&q).unwrap().len(), 1);

    // deleted meetings disappear from search
    s.append_local(EventBody::MeetingDelete { id: m.id.clone() })
        .unwrap();
    assert!(s.search("dong", 10).unwrap().is_empty());
}

#[test]
fn list_meetings_filters() {
    let s = store("a");
    for (i, tag) in [(1, "x"), (2, "y"), (3, "x")] {
        let mut m = fixtures::meeting(i * 1000);
        m.tags = vec![tag.into()];
        s.append_local(EventBody::MeetingUpsert(m)).unwrap();
    }
    let q = |f: MeetingQuery| s.list_meetings(&f).unwrap();
    let all = q(MeetingQuery::default());
    assert_eq!(
        all.iter().map(|m| m.started_at).collect::<Vec<_>>(),
        vec![3000, 2000, 1000]
    );
    assert_eq!(
        q(MeetingQuery {
            tag: Some("x".into()),
            ..Default::default()
        })
        .len(),
        2
    );
    let page = q(MeetingQuery {
        from: Some(1500),
        limit: Some(1),
        offset: Some(1),
        ..Default::default()
    });
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].started_at, 2000);
}

#[test]
fn unknown_kinds_are_stored_and_relayed() {
    let a = store("a");
    let e = Event {
        device: dev("future"),
        seq: 1,
        hlc: Hlc::default(),
        kind: EventKind("verdicts.v9".into()),
        body: serde_json::json!({"x": [1, 2, {"y": "ỹ"}]}),
    };
    assert_eq!(a.apply_remote(vec![e.clone()]).unwrap(), 1);
    let b = store("b");
    b.apply_remote(a.events_missing(&VersionVector::default(), 10).unwrap())
        .unwrap();
    assert_eq!(
        b.events_missing(&VersionVector::default(), 10).unwrap(),
        vec![e]
    );
    // malformed body of a *known* kind: stored, not materialized, doesn't fail the batch
    let bad = Event {
        device: dev("old"),
        seq: 1,
        hlc: Hlc::default(),
        kind: EventKind(EventKind::MEETING_UPSERT.into()),
        body: serde_json::json!({"nope": true}),
    };
    assert_eq!(b.apply_remote(vec![bad]).unwrap(), 1);
    assert_eq!(b.version_vector().unwrap().get(&dev("old")), 1);
}

#[test]
fn loro_notes_merge_and_compact() {
    let a = store("a");
    let b = store("b");
    let doc_id = Id::new();

    let da = loro::LoroDoc::new();
    da.set_peer_id(1).unwrap();
    da.get_text("body").insert(0, "Xin chào").unwrap();
    da.commit();
    let u = notes::export_since(&da, &Default::default()).unwrap();
    a.append_local(notes_update_event(&doc_id, &u)).unwrap();
    b.apply_remote(a.events_missing(&b.version_vector().unwrap(), 100).unwrap())
        .unwrap();

    // concurrent edits on both devices
    let va = da.oplog_vv();
    da.get_text("body").insert(0, "A: ").unwrap();
    da.commit();
    a.append_local(notes_update_event(
        &doc_id,
        &notes::export_since(&da, &va).unwrap(),
    ))
    .unwrap();

    let db = b.notes_doc(&doc_id).unwrap();
    db.set_peer_id(2).unwrap();
    let vb = db.oplog_vv();
    db.get_text("body")
        .insert(db.get_text("body").len_unicode(), " mọi người")
        .unwrap();
    db.commit();
    b.append_local(notes_update_event(
        &doc_id,
        &notes::export_since(&db, &vb).unwrap(),
    ))
    .unwrap();

    a.apply_remote(b.events_missing(&a.version_vector().unwrap(), 100).unwrap())
        .unwrap();
    b.apply_remote(a.events_missing(&b.version_vector().unwrap(), 100).unwrap())
        .unwrap();
    let ta = a.notes_doc(&doc_id).unwrap().get_text("body").to_string();
    let tb = b.notes_doc(&doc_id).unwrap().get_text("body").to_string();
    assert_eq!(ta, tb);
    assert_eq!(ta, "A: Xin chào mọi người");

    assert_eq!(a.notes_updates(&doc_id).unwrap().len(), 3);
    assert_eq!(a.compact_notes(&doc_id).unwrap(), 3);
    assert_eq!(
        a.notes_updates(&doc_id).unwrap().len(),
        1,
        "just the snapshot"
    );
    assert_eq!(
        a.notes_doc(&doc_id).unwrap().get_text("body").to_string(),
        ta
    );
    // log still serves the raw updates to new peers
    let c = store("c");
    c.apply_remote(a.events_missing(&VersionVector::default(), 100).unwrap())
        .unwrap();
    assert_eq!(
        c.notes_doc(&doc_id).unwrap().get_text("body").to_string(),
        ta
    );
    assert_eq!(a.notes_docs().unwrap(), vec![doc_id]);
}

#[test]
fn persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("almond.db");
    let m = fixtures::meeting(5);
    {
        let s = SqliteStore::open(&path, dev("a")).unwrap();
        s.append_local(EventBody::MeetingUpsert(m.clone())).unwrap();
        s.kv_set("cursor", "1").unwrap();
        s.kv_set("cursor", "2").unwrap();
    }
    let s = SqliteStore::open(&path, dev("a")).unwrap();
    assert_eq!(s.get_meeting(&m.id).unwrap().unwrap(), m);
    assert_eq!(s.kv_get("cursor").unwrap().as_deref(), Some("2"));
    let e = s
        .append_local(EventBody::MeetingDelete { id: m.id.clone() })
        .unwrap();
    assert_eq!(e.seq, 2, "seq continues after reopen");
    assert!(e.hlc.wall_ms > 0);
    drop(s);
    assert!(
        SqliteStore::open(&path, dev("b")).is_err(),
        "db bound to device"
    );
}

#[test]
fn device_info_and_blobs_views() {
    let s = store("a");
    let mut info = fixtures::device("pc");
    s.append_local(EventBody::DeviceInfo(info.clone())).unwrap();
    info.name = "renamed".into();
    s.append_local(EventBody::DeviceInfo(info.clone())).unwrap();
    assert_eq!(s.devices().unwrap(), vec![info]);
    let h = almond_types::BlobHash("ab".repeat(32));
    s.append_local(EventBody::BlobAvailable {
        hash: h.clone(),
        size: 42,
        mime: "audio/ogg".into(),
    })
    .unwrap();
    let b = s.known_blobs().unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!((b[0].size, b[0].holders.clone()), (42, vec![dev("a")]));
    assert_eq!(s.event_count().unwrap(), 3);
}

#[test]
fn own_events_coming_back_bump_next_seq() {
    let a = store("a");
    a.append_local(EventBody::MeetingUpsert(fixtures::meeting(0)))
        .unwrap();
    a.append_local(EventBody::MeetingUpsert(fixtures::meeting(0)))
        .unwrap();
    let backup = a.events_missing(&VersionVector::default(), 10).unwrap();
    // "a" lost its DB; a fresh store for the same identity receives its own history back
    let a2 = store("a");
    a2.apply_remote(backup).unwrap();
    let e = a2
        .append_local(EventBody::MeetingUpsert(fixtures::meeting(0)))
        .unwrap();
    assert_eq!(e.seq, 3);
}

#[test]
fn records_are_lww_with_tombstones_in_any_arrival_order() {
    use serde_json::json;
    let up = |v: i64| EventBody::RecordUpsert {
        collection: "person".into(),
        id: "p1".into(),
        body: json!({ "name": format!("v{v}") }),
    };
    let del = EventBody::RecordDelete {
        collection: "person".into(),
        id: "p1".into(),
    };
    let evs = [
        ev("a", 1, 100, up(1)),
        ev("b", 1, 200, up(2)),
        ev("a", 2, 300, del),
        ev("b", 2, 400, up(4)),
    ];
    // newest write is the upsert at 400, whatever the order events arrive in
    for order in [[0, 1, 2, 3], [3, 2, 1, 0], [2, 0, 3, 1]] {
        let s = store("c");
        for i in order {
            s.apply_remote(vec![evs[i].clone()]).unwrap();
        }
        let r = s.get_record("person", "p1").unwrap().unwrap();
        assert_eq!(r.body["name"], "v4");
        assert_eq!(r.device, dev("b"));
        assert_eq!(s.list_records("person").unwrap().len(), 1);
        assert!(s.list_records("recipe").unwrap().is_empty());
    }
    // a newer delete wins over older upserts
    let s = store("d");
    s.apply_remote(vec![evs[0].clone(), evs[2].clone(), evs[1].clone()])
        .unwrap();
    assert!(s.get_record("person", "p1").unwrap().is_none());
    assert!(s.list_records("person").unwrap().is_empty());
}
