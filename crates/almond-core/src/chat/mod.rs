//! Feature "chat" ("Ask Almond"): chat with the local LLM about your meetings, notes, calendar
//! and people (docs/FEATURES-2.md, "Feature `chat`").
//!
//! Methods (docs/API.md, "Ask Almond"):
//! - `chat.ask { thread_id?, scope?, message, lang? }` → `{ thread_id, entry }`: builds the
//!   context for the thread's [`ChatScope`] ([`context`]), streams the answer through `chat.delta`
//!   notifications, validates its `[n]` citations against the numbered sources, then saves the
//!   thread (a synced `chat` record). A `thread_id` that doesn't exist yet creates the thread
//!   with that id (so the UI can match `chat.delta` before the call returns).
//! - `chat.threads { scope?, limit? }`, `chat.get { thread_id }`, `chat.delete { thread_id }`,
//!   `chat.cancel { thread_id }` (stops a streaming answer; what was written so far is kept).
//!
//! LLM: the processing module's provider (`processing.llm_provider`: Ollama at the remote-aware
//! URL, or a test override). Model: the `llm_chat` slot when the user chose one
//! (`models.select { task: "llm_chat" }`), else the `llm_summary` slot (default `gemma4:12b` on
//! the PC).

mod context;
mod text;

use crate::error::ApiResult;
use crate::processing::caps::{self, SLOT_LLM_SUMMARY};
use crate::router::Router;
use crate::settings::keys;
use crate::{Core, Shared};
use almond_types::chat::{COLLECTION, ChatEntry, ChatRole, ChatScope, ChatSource, ChatThread};
use almond_types::llm::{ChatMessage, ChatRequest, LlmProvider, Role};
use almond_types::time::{UnixMs, now_ms};
use almond_types::{Error, Id, Result};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Notification while an answer streams: [`ChatDelta`].
pub const CHAT_DELTA: &str = "chat.delta";
/// Model slot for chat (falls back to `llm_summary`).
pub const SLOT_LLM_CHAT: &str = "llm_chat";

/// Earlier turns sent back to the model, at most (newest kept).
const HISTORY_TOKENS: usize = 1_800;
const HISTORY_ENTRIES: usize = 8;
const MAX_QUESTION_CHARS: usize = 4_000;

/// `chat.delta` payload: `text` is the new piece of the answer (append it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatDelta {
    pub thread_id: Id,
    pub entry_id: Id,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct AskParams {
    #[serde(default)]
    pub thread_id: Option<Id>,
    /// Scope of a new thread (default `all`). Must match an existing thread's scope if given.
    #[serde(default)]
    pub scope: Option<ChatScope>,
    pub message: String,
    /// Answer language ("vi" | "en"); default: the question's language, else `ui.language`.
    #[serde(default)]
    pub lang: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AskResult {
    pub thread_id: Id,
    pub entry: ChatEntry,
}

#[derive(Debug, Deserialize)]
struct ThreadsParams {
    #[serde(default)]
    scope: Option<ChatScope>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ThreadParams {
    thread_id: Id,
}

pub struct State {
    /// Threads with an answer streaming → its cancel flag.
    inflight: Mutex<HashMap<Id, Arc<AtomicBool>>>,
}

impl State {
    pub(crate) async fn init(_shared: &Shared) -> Result<State> {
        Ok(State {
            inflight: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) async fn shutdown(&self) {
        for f in self.inflight.lock().unwrap().values() {
            f.store(true, Ordering::Relaxed);
        }
    }
}

/// Marks a thread as answering; cleared on drop.
struct Inflight<'a> {
    st: &'a State,
    id: Id,
    cancel: Arc<AtomicBool>,
}

impl<'a> Inflight<'a> {
    fn start(st: &'a State, id: &Id) -> Result<Self> {
        let mut m = st.inflight.lock().unwrap();
        if m.contains_key(id) {
            return Err(Error::Invalid(
                "Almond is still answering in this chat; wait for it or stop it first".into(),
            ));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        m.insert(id.clone(), cancel.clone());
        Ok(Inflight {
            st,
            id: id.clone(),
            cancel,
        })
    }
}

impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        self.st.inflight.lock().unwrap().remove(&self.id);
    }
}

pub(crate) fn register(r: &mut Router) {
    r.add("chat.ask", |core, p: AskParams| async move {
        ApiResult::Ok(ask(&core, p).await?)
    });
    r.add("chat.threads", |core, p: ThreadsParams| async move {
        ApiResult::Ok(threads(&core, p.scope.as_ref(), p.limit)?)
    });
    r.add("chat.get", |core, p: ThreadParams| async move {
        ApiResult::Ok(
            core.record::<ChatThread>(COLLECTION, p.thread_id.as_str())?
                .ok_or_else(|| Error::NotFound(format!("chat {}", p.thread_id)))?,
        )
    });
    r.add("chat.delete", |core, p: ThreadParams| async move {
        if core
            .record::<Value>(COLLECTION, p.thread_id.as_str())?
            .is_none()
        {
            return Err(Error::NotFound(format!("chat {}", p.thread_id)).into());
        }
        if let Some(f) = core.chat.inflight.lock().unwrap().get(&p.thread_id) {
            f.store(true, Ordering::Relaxed);
        }
        core.delete_record(COLLECTION, p.thread_id.as_str())?;
        ApiResult::Ok(Value::Null)
    });
    r.add("chat.cancel", |core, p: ThreadParams| async move {
        let f = core
            .chat
            .inflight
            .lock()
            .unwrap()
            .get(&p.thread_id)
            .cloned();
        if let Some(f) = &f {
            f.store(true, Ordering::Relaxed);
        }
        ApiResult::Ok(f.is_some())
    });
}

pub(crate) fn start(_core: Arc<Core>) {}

/// Threads, most recently updated first.
pub fn threads(
    core: &Core,
    scope: Option<&ChatScope>,
    limit: Option<usize>,
) -> Result<Vec<ChatThread>> {
    let mut v: Vec<ChatThread> = core
        .records::<ChatThread>(COLLECTION)?
        .into_iter()
        .filter(|t| scope.is_none_or(|s| *s == t.scope))
        .collect();
    v.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
    if let Some(n) = limit {
        v.truncate(n);
    }
    Ok(v)
}

fn valid_thread_id(id: &Id) -> Result<()> {
    let s = id.as_str();
    if s.is_empty()
        || s.len() > 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::Invalid(format!("bad thread id {s:?}")));
    }
    Ok(())
}

/// The thread title: the first question, shortened.
fn title_from(question: &str) -> String {
    let one_line = question.split_whitespace().collect::<Vec<_>>().join(" ");
    text::clip(&one_line, 60)
}

/// The provider and the Ollama model name chat uses on this device.
async fn chat_model(core: &Core, lang: &str) -> Result<(Arc<dyn LlmProvider>, String)> {
    let st = &core.processing;
    let mut info = st.capabilities();
    if info.llm_models.is_empty() {
        // Not detected yet (startup) or Ollama was down at the last check: look again.
        info = st.refresh_capabilities(core).await;
    }
    let url = crate::processing::ollama_url(core);
    if info.llm_models.is_empty()
        && let Some(e) = &info.llm_error
    {
        return Err(Error::Network(format!(
            "Ollama isn't running at {url}, so Ask Almond can't answer. Start Ollama (or set a \
             remote Ollama in Settings) and try again. ({e})"
        )));
    }
    let stored = caps::stored_slots(core);
    let chosen = stored
        .iter()
        .find(|s| s.task == SLOT_LLM_CHAT && s.lang.as_deref() == Some(lang))
        .or_else(|| {
            stored
                .iter()
                .find(|s| s.task == SLOT_LLM_CHAT && s.lang.is_none())
        })
        .map(|s| caps::ollama_name(&s.model).to_string())
        .filter(|m| info.llm_models.is_empty() || info.has_model(m));
    let model =
        chosen.unwrap_or_else(|| caps::slot_model(core, &info, SLOT_LLM_SUMMARY, Some(lang)));
    if !info.llm_models.is_empty() && !info.has_model(&model) {
        return Err(Error::NotFound(format!(
            "model {model} is not installed in Ollama at {url}; run `ollama pull {model}` or \
             choose another model in Settings"
        )));
    }
    Ok((st.llm_provider(core)?, model))
}

/// Make LLM errors say what to do.
fn friendly(core: &Core, e: Error) -> Error {
    match e {
        Error::Network(m) => Error::Network(format!(
            "Ask Almond can't reach Ollama at {}. Start Ollama (or set a remote Ollama in \
             Settings) and try again. ({m})",
            crate::processing::ollama_url(core)
        )),
        e => e,
    }
}

fn system_prompt(ctx: &context::Context, scope: &ChatScope, lang: &str, now: UnixMs) -> String {
    let language = if lang == "vi" {
        "Vietnamese"
    } else {
        "English"
    };
    let scope_line = match scope {
        ChatScope::All => {
            "The user is asking from the home screen: anything about their meetings, notes, calendar and people."
        }
        ChatScope::Meeting { .. } => {
            "The user is asking about one specific meeting (described below)."
        }
        ChatScope::Person { .. } => {
            "The user is asking about one specific person (described below)."
        }
    };
    let mut p = format!(
        "You are Almond, the user's private meeting assistant. You answer questions about the \
user's meetings, notes, calendar and the people they meet, using only the numbered sources below \
(all from the user's own data on this device) and the conversation so far.

Rules:
- Cite the sources you use with their number in square brackets right after the statement, e.g. \
\"The marketing budget is 200 million VND [2].\" Cite several as [2][5]. Use only numbers listed \
below.
- If the sources don't contain the answer, say so in one sentence. Never invent meetings, dates, \
people, decisions or numbers.
- Transcripts are what was actually said; AI summaries and notes can be wrong. When they disagree, \
answer with what the transcript says (citing the transcript), then mention that the summary differs.
- Be concise: answer first, then short bullet points if useful. Markdown is fine; no headings for \
short answers.
- \"Me\" in transcripts is the user. Times are the user's local time. Now: {now}.
- Write your answer in {language}, whatever the language of the sources. Keep names, numbers and \
quotes as they are.
{scope_line}
",
        now = context::fmt_now(now),
    );
    if !ctx.background.is_empty() {
        p.push_str("\nBackground:\n");
        for l in &ctx.background {
            p.push_str(&format!("- {l}\n"));
        }
    }
    p.push_str("\nSources:\n");
    if ctx.sources.is_empty() {
        p.push_str("(none: nothing in the user's data matches)\n");
    }
    for s in &ctx.sources {
        let kind = serde_json::to_value(s.src.kind)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.replace('_', " ")))
            .unwrap_or_default();
        p.push_str(&format!("\n[{}] ({kind}) {}\n", s.src.n, s.text.trim()));
    }
    p
}

/// Earlier turns (newest last), markers removed, within [`HISTORY_TOKENS`].
fn history(thread: &ChatThread) -> Vec<ChatMessage> {
    let mut out = vec![];
    let mut used = 0;
    for e in thread.messages.iter().rev().take(HISTORY_ENTRIES) {
        let content = match e.role {
            ChatRole::User => e.text.clone(),
            ChatRole::Assistant => text::strip_markers(&e.text),
        };
        used += almond_llm::tokens::estimate_tokens(&content) + 8;
        if used > HISTORY_TOKENS && !out.is_empty() {
            break;
        }
        out.push(ChatMessage {
            role: match e.role {
                ChatRole::User => Role::User,
                ChatRole::Assistant => Role::Assistant,
            },
            content,
        });
    }
    // Start with a user turn.
    while out.last().is_some_and(|m| m.role == Role::Assistant) {
        out.pop();
    }
    out.reverse();
    out
}

/// `chat.ask`.
pub async fn ask(core: &Arc<Core>, p: AskParams) -> Result<AskResult> {
    let question = p.message.trim().to_string();
    if question.is_empty() {
        return Err(Error::Invalid("message is empty".into()));
    }
    if question.chars().count() > MAX_QUESTION_CHARS {
        return Err(Error::Invalid(format!(
            "message is too long (max {MAX_QUESTION_CHARS} characters)"
        )));
    }
    let now = now_ms();
    let existing = match &p.thread_id {
        Some(id) => core.record::<ChatThread>(COLLECTION, id.as_str())?,
        None => None,
    };
    let mut thread = match existing {
        Some(t) => {
            if let Some(s) = &p.scope
                && *s != t.scope
            {
                return Err(Error::Invalid(
                    "scope doesn't match the existing thread's scope".into(),
                ));
            }
            t
        }
        None => {
            let id = match &p.thread_id {
                Some(id) => {
                    valid_thread_id(id)?;
                    id.clone()
                }
                None => Id::new(),
            };
            ChatThread {
                id,
                title: String::new(),
                scope: p.scope.clone().unwrap_or(ChatScope::All),
                messages: vec![],
                created_at: now,
                updated_at: now,
            }
        }
    };
    let guard = Inflight::start(&core.chat, &thread.id)?;

    let ui_lang = core.setting_as::<String>(keys::UI_LANGUAGE).ok().flatten();
    let lang = match p.lang.as_deref() {
        Some(l) if l.starts_with("vi") => "vi",
        Some(l) if l.starts_with("en") => "en",
        _ => text::answer_lang(&question, ui_lang.as_deref()),
    };
    // Fail fast (before reading the store) when there is no model to ask.
    let (provider, model) = chat_model(core, lang).await?;

    let ctx = {
        let core = core.clone();
        let scope = thread.scope.clone();
        let q = question.clone();
        tokio::task::spawn_blocking(move || {
            context::build(&core, &scope, &q, now, context::SOURCES_BUDGET)
        })
        .await
        .map_err(Error::other)??
    };

    let mut messages = vec![ChatMessage {
        role: Role::System,
        content: system_prompt(&ctx, &thread.scope, lang, now),
    }];
    messages.extend(history(&thread));
    messages.push(ChatMessage {
        role: Role::User,
        content: question.clone(),
    });
    let req = ChatRequest {
        model: model.clone(),
        messages,
        json_schema: None,
        temperature: Some(0.3),
        num_ctx: None,
        think: Some(false),
    };

    let entry_id = Id::new();
    let emit = |text: &str| {
        core.notifier.emit(
            CHAT_DELTA,
            ChatDelta {
                thread_id: thread.id.clone(),
                entry_id: entry_id.clone(),
                text: text.to_string(),
            },
        )
    };
    let mut answer = String::new();
    match provider.chat_stream(req.clone()).await {
        Ok(mut stream) => {
            while let Some(chunk) = stream.next().await {
                if guard.cancel.load(Ordering::Relaxed) {
                    break;
                }
                match chunk {
                    Ok(c) => {
                        if !c.is_empty() {
                            emit(&c);
                            answer.push_str(&c);
                        }
                    }
                    Err(e) => {
                        if answer.trim().is_empty() {
                            return Err(friendly(core, e));
                        }
                        // Keep what was written; say it was cut.
                        tracing::warn!("chat: stream ended early: {e}");
                        answer.push_str(" …");
                        break;
                    }
                }
            }
        }
        // Providers without streaming: one delta with the whole answer.
        Err(Error::Unsupported(_)) => {
            let r = provider.chat(req).await.map_err(|e| friendly(core, e))?;
            emit(&r.content);
            answer = r.content;
        }
        Err(e) => return Err(friendly(core, e)),
    }
    let cancelled = guard.cancel.load(Ordering::Relaxed);
    if answer.trim().is_empty() {
        return Err(if cancelled {
            Error::Cancelled
        } else {
            Error::Other(format!("the model ({model}) returned an empty answer"))
        });
    }

    let (text, cited) = text::resolve_markers(answer.trim(), ctx.sources.len());
    let sources: Vec<ChatSource> = ctx
        .sources
        .into_iter()
        .filter(|s| cited.contains(&s.src.n))
        .map(|s| s.src)
        .collect();
    let entry = ChatEntry {
        id: entry_id,
        role: ChatRole::Assistant,
        text,
        sources,
        model: Some(format!("{}:{model}", provider.name())),
        at: now_ms(),
    };
    // A thread deleted while answering stays deleted.
    let deleted = p.thread_id.is_some()
        && !thread.messages.is_empty()
        && core
            .record::<Value>(COLLECTION, thread.id.as_str())?
            .is_none();
    if !deleted {
        if thread.title.trim().is_empty() {
            thread.title = title_from(&question);
        }
        thread.messages.push(ChatEntry {
            id: Id::new(),
            role: ChatRole::User,
            text: question,
            sources: vec![],
            model: None,
            at: now,
        });
        thread.messages.push(entry.clone());
        thread.updated_at = entry.at;
        core.put_record(COLLECTION, thread.id.as_str(), &thread)?;
    }
    drop(guard);
    Ok(AskResult {
        thread_id: thread.id,
        entry,
    })
}
