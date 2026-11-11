//! "Ask Almond" against the real Ollama (`http://127.0.0.1:11434`, the `llm_summary` slot,
//! `gemma4:12b` on the PC). Ignored in CI:
//!
//! ```text
//! cargo test -p almond-core --test chat_ollama -- --ignored --nocapture
//! ```

mod common;

use almond_core::Api;
use almond_types::calendar::{Attendee, CalendarEvent};
use almond_types::event::EventBody;
use almond_types::mock::fixtures;
use almond_types::store::Store;
use almond_types::time::now_ms;
use common::open_core;
use serde_json::{Value, json};
use std::time::Instant;

const HOUR: i64 = 3_600_000;

fn event(id: &str, title: &str, start: i64, who: &[(&str, &str)]) -> CalendarEvent {
    CalendarEvent {
        source: "ics:test".into(),
        event_id: id.into(),
        calendar_id: "primary".into(),
        calendar_name: None,
        title: title.into(),
        description: None,
        location: None,
        start,
        end: start + HOUR / 2,
        all_day: false,
        attendees: who
            .iter()
            .map(|(n, e)| Attendee {
                email: Some(e.to_string()),
                name: Some(n.to_string()),
                response: None,
                is_self: false,
            })
            .collect(),
        conference_url: None,
        conference_app: Some("google_meet".into()),
        organizer: None,
        status: "confirmed".into(),
        updated: start,
    }
}

async fn ask(api: &Api, p: Value) -> Value {
    let t0 = Instant::now();
    let mut rx = api.subscribe();
    let r = api
        .call("chat.ask", p.clone())
        .await
        .unwrap_or_else(|e| panic!("chat.ask({p}): {e}"));
    let mut deltas = 0;
    while let Ok(n) = rx.try_recv() {
        if n.name == "chat.delta" {
            deltas += 1;
        }
    }
    let e = &r["entry"];
    println!(
        "\n=== {} ({:.1} s, {deltas} deltas, {})\n{}\n--- sources:",
        p["message"],
        t0.elapsed().as_secs_f64(),
        e["model"],
        e["text"].as_str().unwrap()
    );
    for s in e["sources"].as_array().unwrap() {
        println!("[{}] {} · {}", s["n"], s["kind"], s["label"]);
    }
    assert!(deltas > 1, "the answer streamed");
    // Every marker left in the text has its source.
    let text = e["text"].as_str().unwrap();
    for s in e["sources"].as_array().unwrap() {
        assert!(text.contains(&format!("[{}]", s["n"])));
    }
    r
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the real Ollama with a chat model"]
async fn real_ollama_upcoming_and_meeting_questions() {
    let (api, _dir) = open_core("chat-ollama").await;
    let caps = api.core.processing.refresh_capabilities(&api.core).await;
    assert!(
        !caps.llm_models.is_empty(),
        "Ollama is not running: {:?}",
        caps.llm_error
    );
    let now = now_ms();
    let store = api.core.store();
    let dev = store.device_id();
    let m = fixtures::meeting(now - 2 * 24 * HOUR);
    store
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    let t = fixtures::transcript(&m.id, &dev);
    store
        .append_local(EventBody::ArtifactAdd(t.clone()))
        .unwrap();
    store
        .append_local(EventBody::ArtifactAdd(fixtures::summary(&t, &dev)))
        .unwrap();
    store
        .append_local(EventBody::CalendarUpsert(event(
            "e1",
            "1:1 with Minh",
            now + 3 * HOUR,
            &[("Minh Lê", "minh@example.com")],
        )))
        .unwrap();
    store
        .append_local(EventBody::CalendarUpsert(event(
            "e2",
            "Q4 planning review",
            now + 26 * HOUR,
            &[
                ("Lan Nguyễn", "lan@example.com"),
                ("David Kim", "david@example.com"),
            ],
        )))
        .unwrap();

    // Upcoming events: the answer must cite calendar sources.
    let r = ask(
        &api,
        json!({ "message": "What's coming up in the next few days?" }),
    )
    .await;
    let srcs = r["entry"]["sources"].as_array().unwrap();
    assert!(
        srcs.iter().any(|s| s["kind"] == "calendar_event"),
        "cites the calendar: {srcs:?}"
    );

    // A question about the fixture meeting: cites the transcript/summary of that meeting.
    let r = ask(
        &api,
        json!({ "scope": { "type": "meeting", "meeting_id": m.id }, "message": "What did we decide about the marketing budget?" }),
    )
    .await;
    let srcs = r["entry"]["sources"].as_array().unwrap();
    assert!(!srcs.is_empty(), "cites something");
    assert!(srcs.iter().all(|s| s["meeting_id"] == m.id.as_str()));
    assert!(
        srcs.iter()
            .any(|s| s["kind"] == "transcript" && s["span"]["artifact"] == t.id.as_str()),
        "cites the transcript: {srcs:?}"
    );

    // Home scope, Vietnamese, follow-up in the same thread.
    let r = ask(
        &api,
        json!({ "message": "Mình đã quyết định gì về ngân sách marketing?" }),
    )
    .await;
    assert!(!r["entry"]["sources"].as_array().unwrap().is_empty());
    let tid = r["thread_id"].clone();
    ask(
        &api,
        json!({ "thread_id": tid, "message": "Còn tuyển dụng thì sao?" }),
    )
    .await;
    api.core.shutdown().await;
}
