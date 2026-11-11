//! "Ask Almond" through the core with a scripted mock LLM: context building for each scope
//! (search hit, upcoming event, notes, recipe output, person via speaker labels and calendar
//! email), `chat.delta` streaming, citation validation, thread persistence, the Ollama-down
//! error, and sync of a thread to a second core.

mod common;

use almond_core::Api;
use almond_types::artifact::body::{LabelSource, RecipeOutput, SpeakerLabel, SpeakerLabels};
use almond_types::calendar::{Attendee, CalendarEvent};
use almond_types::chat::ChatThread;
use almond_types::event::EventBody;
use almond_types::ids::SpeakerId;
use almond_types::llm::{ChatRequest, ChatResponse, LlmModelInfo, LlmProvider};
use almond_types::mock::fixtures;
use almond_types::people::Person;
use almond_types::store::Store;
use almond_types::time::{Hlc, now_ms};
use almond_types::{Artifact, ArtifactDraft, Error, Id, Meeting, Provenance, Result};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use common::open_core;
use futures::stream::{self, BoxStream, StreamExt};
use loro::{ExportMode, LoroDoc};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DAY: i64 = 86_400_000;

type AnswerFn = dyn Fn(&str, &str) -> String + Send + Sync;

/// Answers chat requests (system prompt starting with "You are Almond") with `answer(system,
/// question)`, streamed word by word; refuses everything else (the pipeline's LLM jobs).
struct ScriptLlm {
    requests: Mutex<Vec<ChatRequest>>,
    answer: Box<AnswerFn>,
}

impl ScriptLlm {
    fn new(f: impl Fn(&str, &str) -> String + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(ScriptLlm {
            requests: Mutex::new(vec![]),
            answer: Box::new(f),
        })
    }
    fn last(&self) -> ChatRequest {
        self.requests
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a chat request")
    }
    fn reply(&self, req: &ChatRequest) -> Result<String> {
        let sys = &req.messages[0].content;
        if !sys.starts_with("You are Almond") {
            return Err(Error::Other("mock: not for pipeline jobs".into()));
        }
        self.requests.lock().unwrap().push(req.clone());
        let q = &req.messages.last().unwrap().content;
        Ok((self.answer)(sys, q))
    }
}

#[async_trait]
impl LlmProvider for ScriptLlm {
    fn name(&self) -> &str {
        "mock"
    }
    async fn health(&self) -> Result<()> {
        Ok(())
    }
    async fn list_models(&self) -> Result<Vec<LlmModelInfo>> {
        Ok(vec![LlmModelInfo {
            name: "mock:latest".into(),
            digest: None,
            size_bytes: None,
            context_length: Some(32_768),
        }])
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        Ok(ChatResponse {
            content: self.reply(&req)?,
            model: self.list_models().await?.remove(0),
            prompt_tokens: None,
            completion_tokens: None,
        })
    }
    async fn chat_stream(&self, req: ChatRequest) -> Result<BoxStream<'static, Result<String>>> {
        let full = self.reply(&req)?;
        let parts: Vec<Result<String>> = full
            .split_inclusive(' ')
            .map(|s| Ok(s.to_string()))
            .collect();
        Ok(stream::iter(parts).boxed())
    }
}

/// A device without Ollama.
struct NoLlm;

#[async_trait]
impl LlmProvider for NoLlm {
    fn name(&self) -> &str {
        "ollama"
    }
    async fn health(&self) -> Result<()> {
        Err(Error::Network("connection refused".into()))
    }
    async fn list_models(&self) -> Result<Vec<LlmModelInfo>> {
        Err(Error::Network("connection refused".into()))
    }
    async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
        Err(Error::Network("connection refused".into()))
    }
    async fn chat_stream(&self, _: ChatRequest) -> Result<BoxStream<'static, Result<String>>> {
        Err(Error::Network("connection refused".into()))
    }
}

/// Number of the source block in the system prompt that contains `needle`.
fn source_n(system: &str, needle: &str) -> u32 {
    let mut cur = None;
    let sources = system
        .split("\nSources:\n")
        .nth(1)
        .expect("sources section");
    for line in sources.lines() {
        if let Some(rest) = line.strip_prefix('[')
            && let Some((n, tail)) = rest.split_once("] (")
            && let Ok(n) = n.parse::<u32>()
        {
            cur = Some(n);
            if tail.contains(needle) {
                return n;
            }
            continue;
        }
        if line.contains(needle)
            && let Some(n) = cur
        {
            return n;
        }
    }
    panic!("no source contains {needle:?} in:\n{system}");
}

async fn call(api: &Api, m: &str, p: Value) -> Value {
    match api.call(m, p.clone()).await {
        Ok(v) => v,
        Err(e) => panic!("{m}({p}) failed: {e}"),
    }
}

/// A meeting two days ago with the fixture transcript (budget 200 triệu, hiring, Lan's plan)
/// and summary.
fn seed_meeting(api: &Api, started_at: i64) -> (Meeting, Artifact) {
    let store = api.core.store();
    let dev = store.device_id();
    let mut m = fixtures::meeting(started_at);
    m.calendar_event = Some(almond_types::meeting::EventRef {
        source: "ics:test".into(),
        event_id: "past1".into(),
    });
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
    // The calendar event the meeting came from (attendee email for people matching).
    store
        .append_local(EventBody::CalendarUpsert(event(
            "past1",
            "Họp kế hoạch Q4",
            started_at,
            &["lan@example.com"],
        )))
        .unwrap();
    (m, t)
}

fn event(id: &str, title: &str, start: i64, emails: &[&str]) -> CalendarEvent {
    CalendarEvent {
        source: "ics:test".into(),
        event_id: id.into(),
        calendar_id: "primary".into(),
        calendar_name: None,
        title: title.into(),
        description: Some("Agenda: budget".into()),
        location: None,
        start,
        end: start + 30 * 60_000,
        all_day: false,
        attendees: emails
            .iter()
            .map(|e| Attendee {
                email: Some(e.to_string()),
                name: None,
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

async fn with_llm(api: &Api, llm: Arc<dyn LlmProvider>) {
    api.core.processing.set_llm_provider(&api.core, llm).await;
}

async fn edit_notes(api: &Api, doc: &Id, text: &str) {
    let snap = call(api, "notes.get", json!({ "doc": doc })).await;
    let d = LoroDoc::new();
    for u in snap["updates_b64"].as_array().unwrap() {
        d.import(&B64.decode(u.as_str().unwrap()).unwrap()).unwrap();
    }
    let before = d.oplog_vv();
    d.get_text("text").insert(0, text).unwrap();
    d.commit();
    let upd = d.export(ExportMode::updates(&before)).unwrap();
    call(
        api,
        "notes.applyUpdate",
        json!({ "doc": doc, "update_b64": B64.encode(upd) }),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn all_scope_search_hit_upcoming_event_streaming_and_threads() {
    let (api, _dir) = open_core("chat-all").await;
    let now = now_ms();
    let (m, t) = seed_meeting(&api, now - 2 * DAY);
    api.core
        .store()
        .append_local(EventBody::CalendarUpsert(event(
            "up1",
            "Q4 planning review",
            now + DAY,
            &["lan@example.com"],
        )))
        .unwrap();
    let llm = ScriptLlm::new(|sys, q| {
        if q.contains("tuyển") {
            // The summary hit (the transcript says it in English).
            let h = source_n(sys, "Tuyển 2 kỹ sư backend");
            return format!("Mình sẽ tuyển 2 kỹ sư backend trước tháng 12 [{h}].");
        }
        let b = source_n(sys, "200 triệu");
        let e = source_n(sys, "Q4 planning review");
        format!(
            "The marketing budget is 200 triệu đồng [{b}]. Next up: the Q4 planning review \
             tomorrow [{e}] [42]."
        )
    });
    with_llm(&api, llm.clone()).await;
    let mut rx = api.subscribe();

    let r = call(
        &api,
        "chat.ask",
        json!({ "scope": { "type": "all" }, "message": "What did we decide about the marketing budget?" }),
    )
    .await;
    let req = llm.last();
    let sys = &req.messages[0].content;
    assert_eq!(req.model, "mock:latest");
    assert_eq!(req.think, Some(false));
    assert!(sys.contains("Write your answer in English"), "{sys}");
    // Search hit (transcript line with its neighbours) and the upcoming event are numbered sources.
    let b = source_n(sys, "Ngân sách marketing là 200 triệu đồng.");
    let e = source_n(sys, "Q4 planning review");
    assert!(sys.contains("Attendees: lan@example.com"), "{sys}");
    assert!(
        sys.contains("(in 1 days)") || sys.contains("in 23 h") || sys.contains("in 24 h"),
        "{sys}"
    );
    // Recent meetings are summarised too.
    assert!(
        sys.contains("Tuyển 2 kỹ sư backend trước tháng 12."),
        "{sys}"
    );

    let thread_id = r["thread_id"].as_str().unwrap().to_string();
    let entry = &r["entry"];
    assert_eq!(entry["role"], "assistant");
    assert_eq!(entry["model"], "mock:mock:latest");
    // [42] doesn't exist: dropped. The cited sources come back, linked.
    assert_eq!(
        entry["text"],
        format!(
            "The marketing budget is 200 triệu đồng [{b}]. Next up: the Q4 planning review \
             tomorrow [{e}]."
        )
    );
    let srcs = entry["sources"].as_array().unwrap();
    assert_eq!(srcs.len(), 2, "{srcs:?}");
    let hit = srcs.iter().find(|s| s["n"] == b).unwrap();
    assert_eq!(hit["kind"], "transcript");
    assert_eq!(hit["meeting_id"], m.id.as_str());
    assert_eq!(hit["span"]["artifact"], t.id.as_str());
    assert_eq!(hit["span"]["segment"], 1);
    assert_eq!(hit["span"]["time_ms"], json!([5000, 9500]));
    assert!(
        hit["label"]
            .as_str()
            .unwrap()
            .starts_with("Họp kế hoạch Q4 · ")
    );
    let ev = srcs.iter().find(|s| s["n"] == e).unwrap();
    assert_eq!(ev["kind"], "calendar_event");
    assert_eq!(ev["event"]["event_id"], "up1");

    // Streaming: the deltas add up to the raw answer.
    let mut streamed = String::new();
    while let Ok(n) = rx.try_recv() {
        if n.name == "chat.delta" {
            assert_eq!(n.payload["thread_id"], thread_id.as_str());
            assert_eq!(n.payload["entry_id"], entry["id"]);
            streamed.push_str(n.payload["text"].as_str().unwrap());
        }
    }
    assert!(streamed.split_whitespace().count() > 5, "{streamed}");
    assert!(streamed.ends_with("[42]."), "{streamed}");

    // Persisted: the first question titles the thread.
    let th = call(&api, "chat.get", json!({ "thread_id": thread_id })).await;
    assert_eq!(
        th["title"],
        "What did we decide about the marketing budget?"
    );
    assert_eq!(th["scope"], json!({ "type": "all" }));
    assert_eq!(th["messages"].as_array().unwrap().len(), 2);
    assert_eq!(th["messages"][0]["role"], "user");

    // A follow-up in Vietnamese: history goes back without the old markers; answer in Vietnamese.
    let r2 = call(
        &api,
        "chat.ask",
        json!({ "thread_id": thread_id, "message": "Còn tuyển dụng thì sao?" }),
    )
    .await;
    assert_eq!(r2["thread_id"], thread_id.as_str());
    let req = llm.last();
    assert!(
        req.messages[0]
            .content
            .contains("Write your answer in Vietnamese")
    );
    let roles: Vec<_> = req.messages.iter().map(|m| m.role).collect();
    use almond_types::llm::Role;
    assert_eq!(
        roles,
        vec![Role::System, Role::User, Role::Assistant, Role::User]
    );
    assert_eq!(
        req.messages[2].content,
        "The marketing budget is 200 triệu đồng. Next up: the Q4 planning review tomorrow."
    );
    assert!(
        r2["entry"]["text"]
            .as_str()
            .unwrap()
            .contains("kỹ sư backend")
    );
    assert_eq!(r2["entry"]["sources"][0]["kind"], "summary");

    // Listing, scope filter, delete.
    let all = call(&api, "chat.threads", json!({})).await;
    assert_eq!(all.as_array().unwrap().len(), 1);
    assert_eq!(all[0]["messages"].as_array().unwrap().len(), 4);
    let other = call(
        &api,
        "chat.threads",
        json!({ "scope": { "type": "meeting", "meeting_id": m.id } }),
    )
    .await;
    assert!(other.as_array().unwrap().is_empty());
    call(&api, "chat.delete", json!({ "thread_id": thread_id })).await;
    let err = api
        .call("chat.get", json!({ "thread_id": thread_id }))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32004);

    // Client-chosen id for a new thread (so the UI can match deltas before the call returns).
    let r3 = call(
        &api,
        "chat.ask",
        json!({ "thread_id": "ui-thread-1", "message": "What's the marketing budget?" }),
    )
    .await;
    assert_eq!(r3["thread_id"], "ui-thread-1");
    let bad = api
        .call(
            "chat.ask",
            json!({ "thread_id": "bad id!", "message": "x" }),
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code, -32602);
    let empty = api
        .call("chat.ask", json!({ "message": "   " }))
        .await
        .unwrap_err();
    assert_eq!(empty.code, -32602);
    api.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn meeting_scope_uses_notes_summary_recipe_output_and_transcript() {
    let (api, _dir) = open_core("chat-meeting").await;
    let now = now_ms();
    let (m, t) = seed_meeting(&api, now - DAY);
    // Another meeting must not leak into this scope.
    let (other, _) = seed_meeting(&api, now - 3 * DAY);
    edit_notes(&api, &m.notes_doc, "ngân sách mkt: hỏi lại chị Hoa").await;
    let dev = api.core.store().device_id();
    let recipe = ArtifactDraft::new(
        m.id.clone(),
        &RecipeOutput {
            recipe_id: "builtin:questions".into(),
            recipe_name: "Questions I forgot to ask".into(),
            markdown: "- Who approves the extra 15% budget? [1]".into(),
            citations: vec![],
        },
        Provenance::default(),
    )
    .unwrap()
    .into_artifact(Id::new(), Hlc::default(), dev);
    api.core
        .store()
        .append_local(EventBody::ArtifactAdd(recipe))
        .unwrap();

    let llm = ScriptLlm::new(|sys, _| {
        let tr = source_n(sys, "Lan sẽ gửi bản kế hoạch");
        let notes = source_n(sys, "hỏi lại chị Hoa");
        let rec = source_n(sys, "Who approves the extra 15% budget?");
        format!("Lan sends the plan by Friday [{tr}]; ask Hoa about the budget [{notes}][{rec}].")
    });
    with_llm(&api, llm.clone()).await;
    let r = call(
        &api,
        "chat.ask",
        json!({ "scope": { "type": "meeting", "meeting_id": m.id }, "message": "What are the next steps?", "lang": "en" }),
    )
    .await;
    let sys = llm.last().messages[0].content.clone();
    assert!(
        sys.contains("This chat is about one meeting: Meeting \"Họp kế hoạch Q4\""),
        "{sys}"
    );
    assert!(sys.contains("Calendar attendees: lan@example.com"), "{sys}");
    assert!(sys.contains("AI summary of the meeting"), "{sys}");
    assert!(
        sys.contains("Ngân sách marketing Q4 là 300 triệu đồng."),
        "{sys}"
    );
    assert!(!sys.contains(other.id.as_str()));
    // The whole (short) transcript is one chunk, with speakers and times.
    assert!(
        sys.contains("0:05 S1: Ngân sách marketing là 200 triệu đồng."),
        "{sys}"
    );
    assert!(sys.contains("0:00 Me: Chào mọi người"), "{sys}");
    // The recipe output's own markers are not mistaken for ours.
    assert!(
        sys.contains("- Who approves the extra 15% budget?\n"),
        "{sys}"
    );

    let srcs = r["entry"]["sources"].as_array().unwrap().clone();
    let kinds: Vec<&str> = srcs.iter().map(|s| s["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        vec!["notes", "recipe_output", "transcript"],
        "{srcs:?}"
    );
    let tr = srcs.iter().find(|s| s["kind"] == "transcript").unwrap();
    assert_eq!(tr["span"]["artifact"], t.id.as_str());
    assert_eq!(tr["span"]["segment"], 0);
    assert_eq!(tr["span"]["time_ms"], json!([0, 19500]));
    assert!(srcs.iter().all(|s| s["meeting_id"] == m.id.as_str()));
    let th = call(
        &api,
        "chat.threads",
        json!({ "scope": { "type": "meeting", "meeting_id": m.id } }),
    )
    .await;
    assert_eq!(th.as_array().unwrap().len(), 1);
    api.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn person_scope_via_speaker_labels_and_calendar_email() {
    let (api, _dir) = open_core("chat-person").await;
    let now = now_ms();
    let (m, t) = seed_meeting(&api, now - DAY);
    let lan = Person {
        id: Id::new(),
        name: "Lan Nguyễn".into(),
        emails: vec!["lan@example.com".into()],
        aliases: vec![],
        is_self: false,
        notes: "Marketing lead".into(),
        voiceprints: vec![],
        created_at: now,
    };
    api.core
        .put_record("person", lan.id.as_str(), &lan)
        .unwrap();
    // S1 in the transcript is Lan.
    let labels = ArtifactDraft::new(
        m.id.clone(),
        &SpeakerLabels {
            transcript: t.id.clone(),
            labels: vec![SpeakerLabel {
                speaker: SpeakerId("S1".into()),
                person: lan.id.clone(),
                source: LabelSource::User,
                confidence: None,
            }],
        },
        Provenance::default(),
    )
    .unwrap()
    .into_artifact(Id::new(), Hlc::default(), api.core.store().device_id());
    api.core
        .store()
        .append_local(EventBody::ArtifactAdd(labels))
        .unwrap();
    api.core
        .store()
        .append_local(EventBody::CalendarUpsert(event(
            "up2",
            "1:1 with Lan",
            now + 2 * 3_600_000,
            &["LAN@example.com"],
        )))
        .unwrap();

    let llm = ScriptLlm::new(|sys, _| {
        let p = source_n(sys, "Person: Lan Nguyễn");
        let said = source_n(sys, "What Lan Nguyễn said");
        let ev = source_n(sys, "1:1 with Lan");
        format!(
            "Lan leads marketing [{p}], said the budget is 200 triệu [{said}]; 1:1 today [{ev}]."
        )
    });
    with_llm(&api, llm.clone()).await;
    let r = call(
        &api,
        "chat.ask",
        json!({ "scope": { "type": "person", "person_id": lan.id }, "message": "Prep me for my 1:1" }),
    )
    .await;
    let sys = llm.last().messages[0].content.clone();
    assert!(sys.contains("Meetings with them (1):"), "{sys}");
    assert!(
        sys.contains("Notes about") || sys.contains("notes about them: Marketing lead"),
        "{sys}"
    );
    // Names replace speaker ids in the transcript lines.
    assert!(
        sys.contains("0:05 Lan Nguyễn: Ngân sách marketing là 200 triệu đồng."),
        "{sys}"
    );
    let kinds: Vec<&str> = r["entry"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["person", "calendar_event", "transcript"]);

    // Home scope: a question naming Lan brings in her card and the people list.
    let llm2 =
        ScriptLlm::new(|sys, _| format!("Yesterday [{}].", source_n(sys, "Person: Lan Nguyễn")));
    with_llm(&api, llm2.clone()).await;
    call(
        &api,
        "chat.ask",
        json!({ "message": "When did I last meet Lan?" }),
    )
    .await;
    let sys = llm2.last().messages[0].content.clone();
    assert!(
        sys.contains("People the user knows: Lan Nguyễn (last met"),
        "{sys}"
    );
    let missing = api
        .call(
            "chat.ask",
            json!({ "scope": { "type": "person", "person_id": "nobody" }, "message": "hi" }),
        )
        .await
        .unwrap_err();
    assert_eq!(missing.code, -32004);
    api.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ollama_down_is_a_clear_error_and_saves_nothing() {
    let (api, _dir) = open_core("chat-nollm").await;
    with_llm(&api, Arc::new(NoLlm)).await;
    let err = api
        .call("chat.ask", json!({ "message": "What do I have today?" }))
        .await
        .unwrap_err();
    assert!(err.message.contains("Ollama isn't running at"), "{err}");
    assert!(err.message.contains("Start Ollama"), "{err}");
    let threads = call(&api, "chat.threads", json!({})).await;
    assert!(threads.as_array().unwrap().is_empty());
    api.core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn threads_sync_to_a_second_core() {
    let (a, _da) = open_core("chat-sync-a").await;
    let (b, _db) = open_core("chat-sync-b").await;
    let ida = a.core.device_id().0.clone();
    let begin = call(&a, "sync.pairBegin", json!({})).await;
    call(
        &b,
        "sync.pairAccept",
        json!({ "qr_payload": begin["qr_payload"] }),
    )
    .await;

    with_llm(&a, ScriptLlm::new(|_, _| "Nothing yet.".into())).await;
    let r = call(
        &a,
        "chat.ask",
        json!({ "message": "What do I have today?" }),
    )
    .await;
    let tid = r["thread_id"].as_str().unwrap().to_string();

    let t0 = std::time::Instant::now();
    loop {
        let reports = b.call("sync.now", json!({ "peers": [ida] })).await;
        if reports.is_ok()
            && let Ok(Some(th)) = b.core.record::<ChatThread>("chat", &tid)
        {
            assert_eq!(th.messages.len(), 2);
            assert_eq!(th.messages[1].text, "Nothing yet.");
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "thread did not sync"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let listed = call(&b, "chat.threads", json!({})).await;
    assert_eq!(listed[0]["title"], "What do I have today?");
    // Deleting on B syncs back to A.
    call(&b, "chat.delete", json!({ "thread_id": tid })).await;
    let idb = b.core.device_id().0.clone();
    let t0 = std::time::Instant::now();
    loop {
        let _ = a.call("sync.now", json!({ "peers": [idb] })).await;
        if a.core.record::<ChatThread>("chat", &tid).unwrap().is_none() {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "delete did not sync"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    a.core.shutdown().await;
    b.core.shutdown().await;
}
