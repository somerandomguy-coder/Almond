//! Library module through the router: meetings CRUD, notes (Loro in, text out), search,
//! storage usage, ICS calendars → `calendar.upcoming`, reminders.

mod common;

use almond_core::Api;
use almond_types::event::EventBody;
use almond_types::mock::fixtures;
use almond_types::store::{BlobStore, Store};
use almond_types::time::{Hlc, now_ms};
use almond_types::{DeviceId, Id};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use common::open_core;
use loro::{ExportMode, LoroDoc, LoroList, LoroMap, LoroText};
use serde_json::{Value, json};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn call(api: &Api, m: &str, p: Value) -> Value {
    match api.call(m, p.clone()).await {
        Ok(v) => v,
        Err(e) => panic!("{m}({p}) failed: {e}"),
    }
}

async fn call_err(api: &Api, m: &str, p: Value) -> almond_core::ApiError {
    api.call(m, p.clone())
        .await
        .expect_err(&format!("{m}({p}) should fail"))
}

/// A paragraph block in the loro-prosemirror layout the desktop editor writes.
fn paragraph(list: &LoroList, idx: usize, text: &str) {
    let m = list.insert_container(idx, LoroMap::new()).unwrap();
    m.insert("nodeName", "paragraph").unwrap();
    m.insert_container("attributes", LoroMap::new()).unwrap();
    let kids = m.insert_container("children", LoroList::new()).unwrap();
    let t = kids.insert_container(0, LoroText::new()).unwrap();
    t.insert(0, text).unwrap();
}

fn doc_children(d: &LoroDoc) -> LoroList {
    let root = d.get_map("doc");
    root.insert("nodeName", "doc").unwrap();
    root.insert_container("children", LoroList::new()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn meetings_crud_notes_search_storage() {
    let (api, dir) = open_core("lib-meetings").await;
    let core = api.core.clone();

    // ---- create / list / get / update
    let m = call(
        &api,
        "meetings.create",
        json!({ "title": "Họp ngân sách Q4", "kind": { "type": "online", "app": "google_meet" }, "calendar_event": null }),
    )
    .await;
    let id = m["id"].as_str().unwrap().to_string();
    let doc = m["notes_doc"].as_str().unwrap().to_string();
    assert_eq!(m["title"], "Họp ngân sách Q4");
    assert!(m["tz"].is_string(), "local IANA zone recorded: {m}");
    let list = call(&api, "meetings.list", json!({})).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    let upd = call(
        &api,
        "meetings.update",
        json!({ "id": id, "title": "Họp ngân sách Q4 (chốt)", "tags": ["finance", "q4"] }),
    )
    .await;
    assert_eq!(upd["title"], "Họp ngân sách Q4 (chốt)");
    let tagged = call(&api, "meetings.list", json!({ "tag": "finance" })).await;
    assert_eq!(tagged.as_array().unwrap().len(), 1);
    let none = call(&api, "meetings.list", json!({ "tag": "nope" })).await;
    assert!(none.as_array().unwrap().is_empty());

    // ---- notes: Loro updates in, the same doc and plain text out
    let d = LoroDoc::new();
    let kids = doc_children(&d);
    paragraph(&kids, 0, "Chốt ngân sách marketing");
    d.commit();
    let v1 = d.oplog_vv();
    let u1 = d.export(ExportMode::all_updates()).unwrap();
    paragraph(&kids, 1, "Lan gửi kế hoạch trước thứ Sáu");
    d.commit();
    let u2 = d.export(ExportMode::updates(&v1)).unwrap();
    for u in [&u1, &u2] {
        call(
            &api,
            "notes.applyUpdate",
            json!({ "doc": doc, "update_b64": B64.encode(u) }),
        )
        .await;
    }
    let snap = call(&api, "notes.get", json!({ "doc": doc })).await;
    let back = LoroDoc::new();
    for u in snap["updates_b64"].as_array().unwrap() {
        back.import(&B64.decode(u.as_str().unwrap()).unwrap())
            .unwrap();
    }
    assert_eq!(back.get_deep_value(), d.get_deep_value());
    let text = almond_core::library::notes_text(&core, &Id(doc.clone())).unwrap();
    assert_eq!(
        text,
        "Chốt ngân sách marketing\nLan gửi kế hoạch trước thứ Sáu"
    );
    // garbage is rejected before it enters the synced log
    let e = call_err(
        &api,
        "notes.applyUpdate",
        json!({ "doc": doc, "update_b64": B64.encode(b"not loro") }),
    )
    .await;
    assert_eq!(e.code, -32602, "{e}");
    let e = call_err(
        &api,
        "notes.applyUpdate",
        json!({ "doc": doc, "update_b64": "%%%" }),
    )
    .await;
    assert_eq!(e.code, -32602, "{e}");

    // ---- artifacts in meetings.get: latest non-superseded per kind first, then older ones
    let me: DeviceId = core.device_id().clone();
    let mid = Id(id.clone());
    let mut t = fixtures::transcript(&mid, &me);
    t.created_at = Hlc {
        wall_ms: 1_000,
        logical: 0,
    };
    let mut s1 = fixtures::summary(&t, &me);
    s1.created_at = Hlc {
        wall_ms: 2_000,
        logical: 0,
    };
    let mut s2 = fixtures::summary(&t, &me);
    s2.created_at = Hlc {
        wall_ms: 3_000,
        logical: 0,
    };
    s2.supersedes = Some(s1.id.clone());
    for a in [&t, &s1, &s2] {
        core.store()
            .append_local(EventBody::ArtifactAdd(a.clone()))
            .unwrap();
    }
    let detail = call(&api, "meetings.get", json!({ "id": id })).await;
    let ids: Vec<&str> = detail["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![s2.id.as_str(), t.id.as_str(), s1.id.as_str()]);
    assert_eq!(detail["meeting"]["tags"], json!(["finance", "q4"]));
    assert!(detail["calendar_event"].is_null());

    // ---- search: diacritic-insensitive over titles and transcripts
    let hits = call(
        &api,
        "search.query",
        json!({ "text": "ngan sach", "limit": 10 }),
    )
    .await;
    let hits = hits.as_array().unwrap();
    assert!(hits.iter().any(|h| h["kind"] == "meeting"), "{hits:?}");
    assert!(
        hits.iter()
            .any(|h| h["kind"] == "transcript" && h["snippet"].as_str().unwrap().contains("«")),
        "{hits:?}"
    );
    assert!(hits.iter().all(|h| h["meeting_id"] == id.as_str()));
    let hits = call(&api, "search.query", json!({ "text": "backend engineers" })).await;
    assert!(!hits.as_array().unwrap().is_empty());
    let found = call(&api, "meetings.list", json!({ "text": "ngân sách" })).await;
    assert_eq!(found.as_array().unwrap().len(), 1);

    // ---- storage usage
    let blob = core.blobs().put(&vec![7u8; 4096]).expect("put blob");
    core.store()
        .append_local(EventBody::BlobAvailable {
            hash: blob,
            size: 4096,
            mime: "audio/ogg".into(),
        })
        .unwrap();
    let u = call(&api, "storage.usage", json!({})).await;
    assert!(u["db_bytes"].as_u64().unwrap() > 0, "{u}");
    assert!(u["blobs_bytes"].as_u64().unwrap() >= 4096, "{u}");
    assert_eq!(u["audio_bytes"].as_u64().unwrap(), 4096, "{u}");
    assert!(u["models_bytes"].is_u64());

    // ---- delete
    call(&api, "meetings.delete", json!({ "id": id })).await;
    let e = call_err(&api, "meetings.get", json!({ "id": id })).await;
    assert_eq!(e.code, -32004);
    let e = call_err(&api, "meetings.delete", json!({ "id": id })).await;
    assert_eq!(e.code, -32004);
    assert!(
        call(&api, "meetings.list", json!({}))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        call(&api, "search.query", json!({ "text": "ngan sach" }))
            .await
            .as_array()
            .unwrap()
            .is_empty(),
        "deleted meetings drop out of search"
    );

    api.core.shutdown().await;
    drop((api, core));
    let _ = std::fs::remove_dir_all(dir);
}

/// `20261110T090000Z` for a Unix-ms instant.
fn ics_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap()
        .format("%Y%m%dT%H%M%SZ")
        .to_string()
}

fn vevent(uid: &str, title: &str, start: i64, extra: &str) -> String {
    format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:{}\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:{title}\r\n{extra}END:VEVENT\r\n",
        ics_time(start),
        ics_time(start),
        ics_time(start + 30 * 60_000)
    )
}

fn calendar(events: &[String]) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//almond test//EN\r\nX-WR-CALNAME:Team\r\n{}END:VCALENDAR\r\n",
        events.concat()
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn ics_calendar_upcoming_reminder() {
    let (api, dir) = open_core("lib-calendar").await;
    let mut rx = api.subscribe();
    let now = now_ms();
    let soon = now + 3 * 60_000; // inside the default 5-minute lead → reminder fires now
    let later = now + 5 * 3_600_000;
    let ics_path = dir.join("team.ics");
    let standup = vevent(
        "standup",
        "Standup",
        soon,
        "DESCRIPTION:Join https://meet.google.com/abc-defg-hij\r\nATTENDEE;CN=Bob:mailto:bob@example.com\r\n",
    );
    std::fs::write(
        &ics_path,
        calendar(&[
            standup.clone(),
            vevent("review", "Design review", later, ""),
        ]),
    )
    .unwrap();

    // ---- local file feed
    let info = call(
        &api,
        "calendar.addIcs",
        json!({ "url": ics_path.to_str().unwrap(), "label": "Team" }),
    )
    .await;
    let src = info["id"].as_str().unwrap().to_string();
    assert!(src.starts_with("ics:"));
    assert_eq!(info["provider"], "ics");
    let sources = call(&api, "calendar.sources", json!({})).await;
    assert_eq!(sources["sources"].as_array().unwrap().len(), 1);

    let up = call(&api, "calendar.upcoming", json!({ "hours": 24 })).await;
    let titles: Vec<&str> = up
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["event"]["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["Standup", "Design review"]);
    assert_eq!(up[0]["event"]["conference_app"], "google_meet");
    let short = call(&api, "calendar.upcoming", json!({ "hours": 1 })).await;
    assert_eq!(short.as_array().unwrap().len(), 1);

    // ---- a reminder fires for the meeting with a Meet link (lead 5 min, it starts in 3)
    let reminder = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let n = rx.recv().await.unwrap();
            if n.name == "calendar.reminder" {
                return n.payload;
            }
        }
    })
    .await
    .expect("calendar.reminder within 15 s");
    assert_eq!(reminder["event"]["title"], "Standup");
    assert_eq!(reminder["minutes_before"], 5);

    // ---- starting notes from the event links the meeting
    let m = call(
        &api,
        "meetings.create",
        json!({ "title": null, "kind": { "type": "online", "app": null },
                "calendar_event": { "source": src, "event_id": "standup" } }),
    )
    .await;
    assert_eq!(m["title"], "Standup", "title defaults to the event's");
    let up = call(&api, "calendar.upcoming", json!({})).await;
    assert_eq!(up[0]["meeting_id"], m["id"]);
    let detail = call(&api, "meetings.get", json!({ "id": m["id"] })).await;
    assert_eq!(detail["calendar_event"]["event_id"], "standup");

    // ---- the feed changes: review removed, standup renamed → refresh applies the diff
    std::fs::write(
        &ics_path,
        calendar(&[standup.replace("SUMMARY:Standup", "SUMMARY:Daily standup")]),
    )
    .unwrap();
    call(&api, "calendar.refresh", json!({})).await;
    let up = call(&api, "calendar.upcoming", json!({ "hours": 24 })).await;
    let titles: Vec<&str> = up
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["event"]["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["Daily standup"]);

    // ---- an HTTP feed (webcal-style subscription)
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cal.ics"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(calendar(&[vevent(
                "offsite",
                "Offsite",
                now + 2 * 3_600_000,
                "",
            )])),
        )
        .mount(&server)
        .await;
    let http = call(
        &api,
        "calendar.addIcs",
        json!({ "url": format!("{}/cal.ics", server.uri()) }),
    )
    .await;
    let up = call(&api, "calendar.upcoming", json!({ "hours": 24 })).await;
    assert_eq!(up.as_array().unwrap().len(), 2);
    // a wrong URL is reported, not saved
    let e = call_err(
        &api,
        "calendar.addIcs",
        json!({ "url": format!("{}/missing.ics", server.uri()) }),
    )
    .await;
    assert_eq!(e.code, -32004, "{e}");
    assert_eq!(
        call(&api, "calendar.sources", json!({})).await["sources"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // ---- remove: the source and its events go away
    call(&api, "calendar.remove", json!({ "source": http["id"] })).await;
    call(&api, "calendar.remove", json!({ "source": src })).await;
    assert!(
        call(&api, "calendar.upcoming", json!({ "hours": 24 }))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    let e = call_err(&api, "calendar.remove", json!({ "source": src })).await;
    assert_eq!(e.code, -32004);

    // ---- platform / setup errors are actionable
    let e = call_err(&api, "calendar.importPhone", json!({})).await;
    assert_eq!(e.code, -32005, "{e}");
    let e = call_err(&api, "calendar.addGoogle", json!({})).await;
    assert!(
        e.message.contains("Settings → Calendar & mail") && e.message.contains("google-setup.md"),
        "{e}"
    );

    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn reminder_lead_setting_and_other_active_device() {
    let (api, dir) = open_core("lib-reminder").await;
    let core = api.core.clone();
    let mut rx = api.subscribe();
    // another device was used a minute ago (its device.info heartbeat arrived by sync)
    let mut other = fixtures::device("laptop");
    other.last_active = Some(now_ms() - 60_000);
    let ev = |id: &str, start_in_min: i64| almond_types::calendar::CalendarEvent {
        source: "phone:x".into(),
        event_id: id.into(),
        calendar_id: "c".into(),
        calendar_name: None,
        title: id.into(),
        description: None,
        location: None,
        start: now_ms() + start_in_min * 60_000,
        end: now_ms() + (start_in_min + 30) * 60_000,
        all_day: false,
        attendees: vec![],
        conference_url: Some("https://zoom.us/j/1".into()),
        conference_app: Some("zoom".into()),
        organizer: None,
        status: "confirmed".into(),
        updated: 0,
    };
    // lead 15 min: an event in 10 min is due
    call(
        &api,
        "settings.set",
        json!({ "key": "calendar.reminder_lead_min", "value": 15 }),
    )
    .await;
    core.store()
        .apply_remote(vec![almond_types::event::Event {
            device: other.id.clone(),
            seq: 1,
            hlc: Hlc {
                wall_ms: now_ms(),
                logical: 0,
            },
            kind: almond_types::event::EventKind("device.info".into()),
            body: serde_json::to_value(&other).unwrap(),
        }])
        .unwrap();
    // this device: in use right now (any library call counts)
    call(&api, "meetings.list", json!({})).await;
    core.store()
        .append_local(EventBody::CalendarUpsert(ev("sync-call", 10)))
        .unwrap();
    // calendar changes arrive by sync too; a new upcoming call wakes the scheduler via polling
    // within 20 s at worst, so nudge it through the public refresh path
    call(&api, "calendar.refresh", json!({})).await;
    let r = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let n = rx.recv().await.unwrap();
            if n.name == "calendar.reminder" {
                return n.payload;
            }
        }
    })
    .await
    .expect("reminder on the most recently used device");
    assert_eq!(r["event"]["event_id"], "sync-call");
    assert_eq!(r["minutes_before"], 15);

    api.core.shutdown().await;
    drop((api, core));
    let _ = std::fs::remove_dir_all(dir);
}
