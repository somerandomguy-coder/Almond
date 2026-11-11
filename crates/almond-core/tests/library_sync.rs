//! Two (three) cores in one process pair over real iroh on localhost (no mDNS) and sync a
//! meeting with Loro notes both ways; then 6-word pairing via a manual address, unpairing, and an
//! offline peer reported as `offline`.

mod common;

use almond_core::Api;
use almond_types::Id;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use common::open_core;
use loro::{ExportMode, LoroDoc};
use serde_json::{Value, json};
use std::time::Duration;

async fn call(api: &Api, m: &str, p: Value) -> Value {
    match api.call(m, p.clone()).await {
        Ok(v) => v,
        Err(e) => panic!("{m}({p}) failed: {e}"),
    }
}

fn id_of(api: &Api) -> String {
    api.core.device_id().0.clone()
}

/// Apply local edits to a notes doc through the API (as the editor does).
async fn edit_notes(api: &Api, doc: &str, f: impl FnOnce(&LoroDoc)) {
    let snap = call(api, "notes.get", json!({ "doc": doc })).await;
    let d = LoroDoc::new();
    for u in snap["updates_b64"].as_array().unwrap() {
        d.import(&B64.decode(u.as_str().unwrap()).unwrap()).unwrap();
    }
    let before = d.oplog_vv();
    f(&d);
    d.commit();
    let upd = d.export(ExportMode::updates(&before)).unwrap();
    call(
        api,
        "notes.applyUpdate",
        json!({ "doc": doc, "update_b64": B64.encode(upd) }),
    )
    .await;
}

fn text(api: &Api, doc: &str) -> String {
    almond_core::library::notes_text(&api.core, &Id(doc.to_string())).unwrap()
}

async fn sync_ok(api: &Api, peer: &str) -> Value {
    let reports = call(api, "sync.now", json!({ "peers": [peer] })).await;
    assert_eq!(reports.as_array().unwrap().len(), 1, "{reports}");
    assert_eq!(reports[0]["status"], "ok", "{reports}");
    reports[0].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pair_and_sync_meeting_with_notes() {
    let (a, dir_a) = open_core("lib-sync-a").await;
    let (b, dir_b) = open_core("lib-sync-b").await;
    let (ida, idb) = (id_of(&a), id_of(&b));
    assert_ne!(ida, idb, "each data dir has its own device identity");
    let mut b_events = b.subscribe();

    // ---- pair: A shows the QR, B scans it
    let begin = call(&a, "sync.pairBegin", json!({})).await;
    assert_eq!(begin["words"].as_array().unwrap().len(), 6);
    let peer = call(
        &b,
        "sync.pairAccept",
        json!({ "qr_payload": begin["qr_payload"] }),
    )
    .await;
    assert_eq!(peer["device"]["id"], ida.as_str());
    assert_eq!(peer["paired"], true);
    // pairAccept starts a first sync in the background; let it finish so the meeting below is
    // new to B (otherwise, on a fast machine, that sync may already carry it and B's own
    // `sync.now` receives nothing new).
    let t0 = std::time::Instant::now();
    loop {
        let st = call(&b, "sync.status", json!({})).await;
        let done = st["last_reports"]
            .as_array()
            .is_some_and(|v| v.iter().any(|r| r["peer"] == ida.as_str()));
        if done && st["syncing"] == false {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "first sync: {st}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // ---- A writes a meeting with notes; B syncs and sees both
    let m = call(
        &a,
        "meetings.create",
        json!({ "title": "Sprint planning", "kind": { "type": "in_person" }, "calendar_event": null }),
    )
    .await;
    let doc = m["notes_doc"].as_str().unwrap().to_string();
    edit_notes(&a, &doc, |d| {
        d.get_text("notes").insert(0, "Agenda: scope").unwrap();
    })
    .await;
    let rep = sync_ok(&b, &ida).await;
    assert!(rep["events_received"].as_u64().unwrap() >= 1, "{rep}");
    let on_b = call(&b, "meetings.get", json!({ "id": m["id"] })).await;
    assert_eq!(on_b["meeting"]["title"], "Sprint planning");
    assert_eq!(text(&b, &doc), "Agenda: scope");

    // the host learned the joiner during pairing
    let st = call(&a, "sync.status", json!({})).await;
    let peers = st["peers"].as_array().unwrap();
    assert!(
        peers
            .iter()
            .any(|p| p["device"]["id"] == idb.as_str() && p["paired"] == true),
        "{st}"
    );
    assert!(st["this_device"]["last_active"].is_i64(), "{st}");

    // ---- concurrent edits on both sides merge (Loro), whoever presses Sync
    edit_notes(&a, &doc, |d| {
        let t = d.get_text("notes");
        t.insert(t.len_unicode(), ", budget").unwrap();
    })
    .await;
    edit_notes(&b, &doc, |d| {
        d.get_text("notes").insert(0, "[B] ").unwrap();
    })
    .await;
    call(
        &b,
        "meetings.update",
        json!({ "id": m["id"], "title": "Sprint planning #42", "tags": null }),
    )
    .await;
    sync_ok(&a, &idb).await;
    let (ta, tb) = (text(&a, &doc), text(&b, &doc));
    assert_eq!(ta, tb);
    assert_eq!(ta, "[B] Agenda: scope, budget");
    let on_a = call(&a, "meetings.get", json!({ "id": m["id"] })).await;
    assert_eq!(on_a["meeting"]["title"], "Sprint planning #42");

    // B saw progress notifications for its sync
    let mut phases = vec![];
    while let Ok(n) = b_events.try_recv() {
        if n.name == "sync.progress" {
            phases.push(n.payload["phase"].as_str().unwrap().to_string());
        }
    }
    assert!(phases.contains(&"connecting".to_string()), "{phases:?}");
    assert!(phases.contains(&"done".to_string()), "{phases:?}");
    let st_b = call(&b, "sync.status", json!({})).await;
    assert!(!st_b["last_reports"].as_array().unwrap().is_empty());
    assert!(
        st_b["peers"][0]["last_synced"].is_i64(),
        "last_synced recorded: {st_b}"
    );

    // ---- 6 words + manual address: C pairs with A without discovery
    let (c, dir_c) = open_core("lib-sync-c").await;
    let begin = call(&a, "sync.pairBegin", json!({})).await;
    let ticket = almond_sync::ticket_from_qr(begin["qr_payload"].as_str().unwrap()).unwrap();
    let addr = ticket.addrs[0].clone();
    // the device id is required: iroh dials by key
    let e = c
        .call("sync.addAddr", json!({ "addr": addr }))
        .await
        .unwrap_err();
    assert_eq!(e.code, -32602);
    let e = c
        .call(
            "sync.addAddr",
            json!({ "device": ida, "addr": "not-an-addr" }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, -32602);
    call(&c, "sync.addAddr", json!({ "device": ida, "addr": addr })).await;
    let peer = call(
        &c,
        "sync.pairAccept",
        json!({ "words": begin["words"], "device": ida }),
    )
    .await;
    assert_eq!(peer["device"]["id"], ida.as_str());
    sync_ok(&c, &ida).await;
    // C got A's meeting, including what A had received from B
    let on_c = call(&c, "meetings.get", json!({ "id": m["id"] })).await;
    assert_eq!(on_c["meeting"]["title"], "Sprint planning #42");
    assert_eq!(text(&c, &doc), "[B] Agenda: scope, budget");

    // ---- unpair: no more syncing with that peer
    call(&c, "sync.unpair", json!({ "device": ida })).await;
    let e = c
        .call("sync.unpair", json!({ "device": ida }))
        .await
        .unwrap_err();
    assert_eq!(e.code, -32004);
    let r = call(&c, "sync.now", json!({ "peers": [ida] })).await;
    assert_ne!(r[0]["status"], "ok", "{r}");

    // ---- a paired peer whose app is closed shows up as offline, not as a failure
    a.core.shutdown().await;
    let r = tokio::time::timeout(
        Duration::from_secs(60),
        call(&b, "sync.now", json!({ "peers": [ida] })),
    )
    .await
    .expect("sync.now returns even when the peer is gone");
    assert_eq!(r[0]["status"], "offline", "{r}");

    b.core.shutdown().await;
    c.core.shutdown().await;
    drop((a, b, c));
    for d in [dir_a, dir_b, dir_c] {
        let _ = std::fs::remove_dir_all(d);
    }
}
