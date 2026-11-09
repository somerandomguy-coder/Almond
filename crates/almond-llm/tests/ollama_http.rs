//! Ollama provider against a tiny in-process HTTP mock (no Ollama needed), plus `#[ignore]`d
//! tests against a real server.
//!
//! Run the real ones with Ollama on localhost:11434 and the models pulled:
//! `cargo test -p almond-llm --test ollama_http -- --ignored --nocapture`
//! (override the model with `ALMOND_TEST_MODEL=gemma4:12b`).

use almond_llm::{Hooks, LlmTasks, Ollama, OllamaConfig, PromptLibrary, TaskConfig, TaskInput};
use almond_llm::{TranscriptView, ollama::model_ref};
use almond_types::LangTag;
use almond_types::ids::DeviceId;
use almond_types::llm::{ChatMessage, ChatRequest, LlmProvider, Role};
use almond_types::mock::fixtures;
use futures::StreamExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Seen = Arc<Mutex<Vec<(String, Value)>>>;

/// Minimal HTTP/1.1 server: one request per connection, routes by path.
async fn mock_ollama(chat_status: u16, chat_body: &'static str) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::new(Mutex::new(vec![]));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let seen = seen2.clone();
            tokio::spawn(async move {
                let mut buf = vec![];
                let mut tmp = [0u8; 4096];
                let (head_end, content_len) = loop {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                        let cl = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .map(|v| v.trim().parse::<usize>().unwrap())
                            .unwrap_or(0);
                        break (p + 4, cl);
                    }
                };
                while buf.len() < head_end + content_len {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
                seen.lock().unwrap().push((path.clone(), body));
                let (status, resp) = match path.as_str() {
                    "/api/version" => (200, r#"{"version":"0.20.7"}"#),
                    "/api/tags" => (
                        200,
                        r#"{"models":[{"name":"qwen3.5:9b","model":"qwen3.5:9b","size":6594474711,"digest":"6488c96fa5fa"}]}"#,
                    ),
                    "/api/show" => (
                        200,
                        r#"{"model_info":{"general.architecture":"qwen35","qwen35.context_length":262144}}"#,
                    ),
                    "/api/chat" => (chat_status, chat_body),
                    _ => (404, r#"{"error":"not found"}"#),
                };
                let out = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                    resp.len()
                );
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), seen)
}

fn cfg(url: &str) -> OllamaConfig {
    OllamaConfig {
        base_url: url.into(),
        ..Default::default()
    }
}

fn req(schema: Option<Value>) -> ChatRequest {
    ChatRequest {
        model: "qwen3.5:9b".into(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: "hi".into(),
        }],
        json_schema: schema,
        temperature: Some(0.1),
        num_ctx: None,
        think: Some(false),
    }
}

#[tokio::test]
async fn chat_sends_format_think_ctx_and_reports_digest() {
    let (url, seen) = mock_ollama(
        200,
        r#"{"model":"qwen3.5:9b","message":{"role":"assistant","content":"{\"ok\":true}"},"done":true,"prompt_eval_count":12,"eval_count":5}"#,
    )
    .await;
    let o = Ollama::new(cfg(&url)).unwrap();
    o.health().await.unwrap();
    let schema = json!({"type":"object","properties":{"ok":{"type":"boolean"}}});
    let r = o.chat(req(Some(schema.clone()))).await.unwrap();
    assert_eq!(r.content, r#"{"ok":true}"#);
    assert_eq!(r.prompt_tokens, Some(12));
    assert_eq!(r.completion_tokens, Some(5));
    assert_eq!(r.model.digest.as_deref(), Some("6488c96fa5fa"));
    assert_eq!(r.model.context_length, Some(262_144));
    assert_eq!(
        model_ref("ollama", &r.model),
        "ollama:qwen3.5:9b@6488c96fa5fa"
    );

    let seen = seen.lock().unwrap();
    let (_, body) = seen.iter().find(|(p, _)| p == "/api/chat").unwrap();
    assert_eq!(body["format"], schema);
    assert_eq!(body["think"], false);
    assert_eq!(body["stream"], false);
    assert_eq!(
        body["options"]["num_ctx"], 8192,
        "min_num_ctx for a tiny prompt"
    );
    assert!((body["options"]["temperature"].as_f64().unwrap() - 0.1).abs() < 1e-6);
    assert_eq!(body["keep_alive"], "10m");
}

#[tokio::test]
async fn chat_stream_yields_deltas() {
    let (url, seen) = mock_ollama(
        200,
        "{\"message\":{\"content\":\"Xin \"},\"done\":false}\n{\"message\":{\"content\":\"chào\"},\"done\":false}\n{\"message\":{\"content\":\"\"},\"done\":true,\"eval_count\":2}\n",
    )
    .await;
    let o = Ollama::new(cfg(&url)).unwrap();
    let parts: Vec<String> = o
        .chat_stream(req(None))
        .await
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
        .await;
    assert_eq!(parts, vec!["Xin ", "chào"]);
    let seen = seen.lock().unwrap();
    let (_, body) = seen.iter().find(|(p, _)| p == "/api/chat").unwrap();
    assert_eq!(body["stream"], true);
    assert!(body.get("format").is_none());
}

#[tokio::test]
async fn stream_error_line_surfaces() {
    let (url, _) = mock_ollama(
        200,
        "{\"message\":{\"content\":\"a\"},\"done\":false}\n{\"error\":\"model crashed\"}\n",
    )
    .await;
    let o = Ollama::new(cfg(&url)).unwrap();
    let items: Vec<_> = o.chat_stream(req(None)).await.unwrap().collect().await;
    assert_eq!(items.len(), 2);
    assert!(
        items[1]
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("model crashed")
    );
}

#[tokio::test]
async fn errors_are_mapped() {
    let (url, _) = mock_ollama(400, r#"{"error":"invalid format"}"#).await;
    let o = Ollama::new(cfg(&url)).unwrap();
    let e = o.chat(req(None)).await.unwrap_err();
    assert!(matches!(e, almond_types::Error::Invalid(_)), "{e}");

    // Unknown model -> NotFound before any chat call.
    let mut r = req(None);
    r.model = "nope:1b".into();
    let e = o.chat(r).await.unwrap_err();
    assert!(matches!(e, almond_types::Error::NotFound(_)), "{e}");

    // Nothing listening -> Network with a helpful message.
    let dead = Ollama::new(OllamaConfig {
        base_url: "http://127.0.0.1:9".into(),
        connect_timeout: Duration::from_millis(500),
        ..Default::default()
    })
    .unwrap();
    let e = dead.health().await.unwrap_err();
    assert!(matches!(e, almond_types::Error::Network(_)), "{e}");
}

#[tokio::test]
async fn num_ctx_policy() {
    let o = Ollama::new(cfg("http://127.0.0.1:1")).unwrap();
    let mut r = req(None);
    assert_eq!(o.choose_num_ctx(&r, Some(262_144)), 8192);
    r.messages[0].content = "x".repeat(100_000); // ~32k tokens
    assert_eq!(
        o.choose_num_ctx(&r, Some(262_144)),
        32_768,
        "capped by max_num_ctx"
    );
    assert_eq!(
        o.choose_num_ctx(&r, Some(16_384)),
        16_384,
        "capped by model"
    );
    r.num_ctx = Some(65_536);
    assert_eq!(o.choose_num_ctx(&r, Some(262_144)), 65_536, "explicit wins");
}

// ---------------------------------------------------------------- real Ollama (ignored)

fn test_model() -> String {
    std::env::var("ALMOND_TEST_MODEL").unwrap_or_else(|_| almond_llm::tasks::default_model())
}

#[tokio::test]
#[ignore = "needs a running Ollama with the default model (or ALMOND_TEST_MODEL)"]
async fn real_ollama_models_and_stream() {
    let o = Ollama::from_env().unwrap();
    o.health().await.unwrap();
    let models = o.list_models().await.unwrap();
    println!(
        "models: {:?}",
        models.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
    let mut r = req(None);
    r.model = test_model();
    r.messages[0].content = "Say hello in Vietnamese, one short sentence.".into();
    let s: String = o
        .chat_stream(r)
        .await
        .unwrap()
        .map(|x| x.unwrap())
        .collect::<Vec<_>>()
        .await
        .concat();
    println!("stream: {s}");
    assert!(!s.trim().is_empty());
}

#[tokio::test]
#[ignore = "needs a running Ollama with the default model (or ALMOND_TEST_MODEL)"]
async fn real_ollama_fixture_summary_vi_and_en() {
    let o: Arc<dyn LlmProvider> = Arc::new(Ollama::from_env().unwrap());
    let tasks = LlmTasks::new(
        o,
        PromptLibrary::builtin(),
        TaskConfig {
            model: test_model(),
            ..Default::default()
        },
    );
    let m = fixtures::meeting(0);
    let t = fixtures::transcript(&m.id, &DeviceId("d".into()));
    let view = TranscriptView::from_artifact(&t).unwrap();
    for lang in [LangTag::vi(), LangTag::en()] {
        let input = TaskInput {
            transcript: &view,
            meeting_title: m.title.clone(),
            meeting_date: Some("2026-11-09".into()),
            output_lang: lang.clone(),
        };
        let s = tasks.summarize(&input, &Hooks::default()).await.unwrap();
        println!("{}", serde_json::to_string_pretty(&s.body).unwrap());
        println!("{:?}", s.report);
        assert!(!s.body.sections.is_empty());
        assert_eq!(s.report.cites.cites_invalid, 0);
        let a = tasks.action_items(&input, &Hooks::default()).await.unwrap();
        println!("{}", serde_json::to_string_pretty(&a.body).unwrap());
        let n = tasks
            .enhance_notes(&input, "- ngân sách mkt?\n- tuyển dụng", &Hooks::default())
            .await
            .unwrap();
        println!("{}", serde_json::to_string_pretty(&n.body).unwrap());
        let ti = tasks.title(&input, &Hooks::default()).await.unwrap();
        println!("title[{}]: {}", lang.0, ti.body.title);
    }
}
