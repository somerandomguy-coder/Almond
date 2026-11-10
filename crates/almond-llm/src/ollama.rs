//! [`LlmProvider`] for a local Ollama server (`/api/chat`, `/api/tags`, `/api/show`).
//!
//! - Structured output: `ChatRequest::json_schema` is passed as Ollama's `format` (JSON Schema).
//! - `think: Some(false)` disables reasoning traces on models that support it (qwen3.x, gemma4).
//! - `num_ctx`: taken from the request, otherwise estimated from the prompt size, rounded up and
//!   capped by the model's trained context and [`OllamaConfig::max_num_ctx`] (VRAM guard).
//! - Provenance: the model digest from `/api/tags` is attached to every response; use
//!   [`model_ref`] to build `"ollama:<name>@<digest>"`.
//!
//! Ollama is only ever reached on localhost (see ARCHITECTURE §2); other devices offload work
//! through jobs, never by calling this server.

use crate::tokens::estimate_tokens;
use almond_types::llm::{ChatRequest, ChatResponse, LlmModelInfo, LlmProvider, Role};
use almond_types::{Error, Result};
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// Connection + behaviour settings for [`Ollama`].
#[derive(Debug, Clone)]
pub struct OllamaConfig {
    /// e.g. `http://127.0.0.1:11434`.
    pub base_url: String,
    pub connect_timeout: Duration,
    /// Whole-request timeout for non-streaming calls (model load + generation).
    pub request_timeout: Duration,
    /// Max silence between two streamed chunks (first chunk includes model load time).
    pub stream_idle_timeout: Duration,
    /// Ollama `keep_alive` (e.g. "10m"); None = server default.
    pub keep_alive: Option<String>,
    /// Upper bound for automatically chosen `num_ctx` (protects VRAM). Explicit
    /// `ChatRequest::num_ctx` values are only capped by the model's trained context.
    pub max_num_ctx: u32,
    /// Lower bound for automatically chosen `num_ctx`.
    pub min_num_ctx: u32,
    /// Tokens reserved for the answer when estimating `num_ctx`.
    pub reserve_output_tokens: u32,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        OllamaConfig {
            base_url: "http://127.0.0.1:11434".into(),
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(900),
            stream_idle_timeout: Duration::from_secs(300),
            keep_alive: Some("10m".into()),
            max_num_ctx: 32_768,
            min_num_ctx: 8_192,
            reserve_output_tokens: 3_072,
        }
    }
}

impl OllamaConfig {
    /// Default config, honouring `OLLAMA_HOST` (`host:port` or a full URL) if set.
    pub fn from_env() -> Self {
        let mut c = Self::default();
        if let Ok(h) = std::env::var("OLLAMA_HOST")
            && !h.trim().is_empty()
        {
            c.base_url = normalize_host(&h);
        }
        c
    }
}

/// `host:port` or a URL -> a URL to connect to (`0.0.0.0`, a bind address, becomes `127.0.0.1`).
pub fn normalize_host(h: &str) -> String {
    let h = h.trim().trim_end_matches('/');
    let with_scheme = if h.starts_with("http://") || h.starts_with("https://") {
        h.to_string()
    } else {
        format!("http://{h}")
    };
    // `0.0.0.0` is a bind address, not something to connect to.
    with_scheme.replace("://0.0.0.0", "://127.0.0.1")
}

/// Ollama HTTP client implementing [`LlmProvider`].
pub struct Ollama {
    cfg: OllamaConfig,
    http: reqwest::Client,
    /// name -> info (digest, size, context length), filled lazily.
    models: Mutex<HashMap<String, LlmModelInfo>>,
}

/// `"ollama:<name>@<digest>"` (digest omitted if unknown) for `Provenance::model`.
pub fn model_ref(provider: &str, info: &LlmModelInfo) -> String {
    match &info.digest {
        Some(d) => format!("{provider}:{}@{d}", info.name),
        None => format!("{provider}:{}", info.name),
    }
}

/// `qwen3.5` -> `qwen3.5:latest`; names with a tag are unchanged.
pub fn canonical_model_name(name: &str) -> String {
    let last = name.rsplit('/').next().unwrap_or(name);
    if last.contains(':') {
        name.to_string()
    } else {
        format!("{name}:latest")
    }
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Deserialize, Default)]
struct ChatChunk {
    #[serde(default)]
    message: Option<ChunkMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize, Default)]
struct ChunkMessage {
    #[serde(default)]
    content: String,
}

impl Ollama {
    pub fn new(cfg: OllamaConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(cfg.connect_timeout)
            // Never route localhost traffic through a system proxy.
            .no_proxy()
            .build()
            .map_err(Error::other)?;
        Ok(Ollama {
            cfg,
            http,
            models: Mutex::new(HashMap::new()),
        })
    }

    /// `Ollama::new(OllamaConfig::from_env())`.
    pub fn from_env() -> Result<Self> {
        Self::new(OllamaConfig::from_env())
    }

    pub fn config(&self) -> &OllamaConfig {
        &self.cfg
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.cfg.base_url.trim_end_matches('/'), path)
    }

    fn map_reqwest(&self, e: reqwest::Error) -> Error {
        if e.is_connect() {
            Error::Network(format!(
                "Ollama not reachable at {} (is `ollama serve` running?): {e}",
                self.cfg.base_url
            ))
        } else if e.is_timeout() {
            Error::Network(format!("Ollama request timed out: {e}"))
        } else {
            Error::Network(format!("Ollama: {e}"))
        }
    }

    async fn check_status(
        &self,
        resp: reqwest::Response,
        model: &str,
    ) -> Result<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let text = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(text);
        Err(match status.as_u16() {
            404 => Error::NotFound(format!(
                "Ollama model {model:?}: {msg} (try `ollama pull {model}`)"
            )),
            400 => Error::Invalid(format!("Ollama rejected request: {msg}")),
            _ => Error::Network(format!("Ollama HTTP {status}: {msg}")),
        })
    }

    /// Model info incl. digest (from `/api/tags`) and context length (from `/api/show`), cached.
    pub async fn model_info(&self, name: &str) -> Result<LlmModelInfo> {
        let key = canonical_model_name(name);
        if let Some(i) = self.models.lock().unwrap().get(&key).cloned()
            && i.context_length.is_some()
        {
            return Ok(i);
        }
        let mut info = self
            .list_models()
            .await?
            .into_iter()
            .find(|m| canonical_model_name(&m.name) == key)
            .ok_or_else(|| Error::NotFound(format!("Ollama model {name:?} is not pulled")))?;
        if info.context_length.is_none() {
            info.context_length = self.show_context_length(&info.name).await.ok().flatten();
        }
        self.models.lock().unwrap().insert(key, info.clone());
        Ok(info)
    }

    async fn show_context_length(&self, name: &str) -> Result<Option<u32>> {
        let resp = self
            .http
            .post(self.url("/api/show"))
            .timeout(Duration::from_secs(30))
            .json(&json!({ "model": name }))
            .send()
            .await
            .map_err(|e| self.map_reqwest(e))?;
        let v: Value = self
            .check_status(resp, name)
            .await?
            .json()
            .await
            .map_err(|e| self.map_reqwest(e))?;
        Ok(v.get("model_info")
            .and_then(|mi| mi.as_object())
            .and_then(|mi| {
                mi.iter()
                    .find(|(k, _)| k.ends_with(".context_length"))
                    .and_then(|(_, v)| v.as_u64())
                    .map(|n| n.min(u32::MAX as u64) as u32)
            }))
    }

    /// `num_ctx` to send: explicit value, or an estimate from the prompt size.
    pub fn choose_num_ctx(&self, req: &ChatRequest, model_ctx: Option<u32>) -> u32 {
        let cap_model = model_ctx.unwrap_or(u32::MAX);
        if let Some(n) = req.num_ctx {
            return n.min(cap_model);
        }
        let prompt: usize = req
            .messages
            .iter()
            .map(|m| estimate_tokens(&m.content) + 8)
            .sum();
        let schema = req
            .json_schema
            .as_ref()
            .map(|s| estimate_tokens(&s.to_string()))
            .unwrap_or(0);
        let need = prompt as u32 + schema as u32 + self.cfg.reserve_output_tokens;
        let rounded = need.div_ceil(2048) * 2048;
        rounded
            .max(self.cfg.min_num_ctx)
            .min(self.cfg.max_num_ctx)
            .min(cap_model)
    }

    fn body(&self, req: &ChatRequest, stream: bool, num_ctx: u32) -> Value {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                json!({
                    "role": match m.role { Role::System => "system", Role::User => "user", Role::Assistant => "assistant" },
                    "content": m.content,
                })
            })
            .collect();
        let mut options = serde_json::Map::new();
        options.insert("num_ctx".into(), json!(num_ctx));
        if let Some(t) = req.temperature {
            options.insert("temperature".into(), json!(t));
        }
        let mut b = json!({
            "model": req.model,
            "messages": messages,
            "stream": stream,
            "options": options,
        });
        if let Some(s) = &req.json_schema {
            b["format"] = s.clone();
        }
        if let Some(t) = req.think {
            b["think"] = json!(t);
        }
        if let Some(k) = &self.cfg.keep_alive {
            b["keep_alive"] = json!(k);
        }
        b
    }

    async fn prepare(&self, req: &ChatRequest) -> Result<(LlmModelInfo, u32)> {
        let info = self.model_info(&req.model).await?;
        let n = self.choose_num_ctx(req, info.context_length);
        Ok((info, n))
    }
}

#[async_trait]
impl LlmProvider for Ollama {
    fn name(&self) -> &str {
        "ollama"
    }

    async fn health(&self) -> Result<()> {
        let resp = self
            .http
            .get(self.url("/api/version"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| self.map_reqwest(e))?;
        self.check_status(resp, "").await?;
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<LlmModelInfo>> {
        let resp = self
            .http
            .get(self.url("/api/tags"))
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| self.map_reqwest(e))?;
        let tags: TagsResponse = self
            .check_status(resp, "")
            .await?
            .json()
            .await
            .map_err(|e| self.map_reqwest(e))?;
        let cache = self.models.lock().unwrap();
        Ok(tags
            .models
            .into_iter()
            .map(|m| {
                let ctx = cache
                    .get(&canonical_model_name(&m.name))
                    .and_then(|i| i.context_length);
                LlmModelInfo {
                    name: m.name,
                    digest: m.digest,
                    size_bytes: m.size,
                    context_length: ctx,
                }
            })
            .collect())
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let (info, num_ctx) = self.prepare(&req).await?;
        let resp = self
            .http
            .post(self.url("/api/chat"))
            .timeout(self.cfg.request_timeout)
            .json(&self.body(&req, false, num_ctx))
            .send()
            .await
            .map_err(|e| self.map_reqwest(e))?;
        let chunk: ChatChunk = self
            .check_status(resp, &req.model)
            .await?
            .json()
            .await
            .map_err(|e| self.map_reqwest(e))?;
        if let Some(e) = chunk.error {
            return Err(Error::Other(format!("Ollama: {e}")));
        }
        Ok(ChatResponse {
            content: chunk.message.map(|m| m.content).unwrap_or_default(),
            model: info,
            prompt_tokens: chunk.prompt_eval_count,
            completion_tokens: chunk.eval_count,
        })
    }

    async fn chat_stream(&self, req: ChatRequest) -> Result<BoxStream<'static, Result<String>>> {
        let (_info, num_ctx) = self.prepare(&req).await?;
        let resp = self
            .http
            .post(self.url("/api/chat"))
            .json(&self.body(&req, true, num_ctx))
            .send()
            .await
            .map_err(|e| self.map_reqwest(e))?;
        let resp = self.check_status(resp, &req.model).await?;
        let idle = self.cfg.stream_idle_timeout;
        Ok(ndjson_content_stream(resp.bytes_stream().boxed(), idle))
    }
}

/// Turn an NDJSON `/api/chat` byte stream into content deltas.
pub(crate) fn ndjson_content_stream<S, B, E>(
    bytes: S,
    idle: Duration,
) -> BoxStream<'static, Result<String>>
where
    S: futures::Stream<Item = std::result::Result<B, E>> + Send + Unpin + 'static,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    struct St<S> {
        bytes: S,
        buf: Vec<u8>,
        done: bool,
    }
    let st = St {
        bytes,
        buf: Vec::new(),
        done: false,
    };
    futures::stream::unfold(st, move |mut st| async move {
        loop {
            if st.done {
                return None;
            }
            // Emit complete lines first.
            if let Some(pos) = st.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = st.buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<ChatChunk>(line) {
                    Ok(c) => {
                        if let Some(e) = c.error {
                            st.done = true;
                            return Some((Err(Error::Other(format!("Ollama: {e}"))), st));
                        }
                        if c.done {
                            st.done = true;
                        }
                        let content = c.message.map(|m| m.content).unwrap_or_default();
                        if content.is_empty() {
                            continue;
                        }
                        return Some((Ok(content), st));
                    }
                    Err(e) => {
                        st.done = true;
                        return Some((Err(Error::Serde(e)), st));
                    }
                }
            }
            match tokio::time::timeout(idle, st.bytes.next()).await {
                Err(_) => {
                    st.done = true;
                    return Some((Err(Error::Network("Ollama stream idle timeout".into())), st));
                }
                Ok(None) => {
                    // Flush a trailing line without newline.
                    if st.buf.iter().any(|b| !b.is_ascii_whitespace()) {
                        st.buf.push(b'\n');
                        continue;
                    }
                    return None;
                }
                Ok(Some(Err(e))) => {
                    st.done = true;
                    return Some((Err(Error::Network(format!("Ollama stream: {e}"))), st));
                }
                Ok(Some(Ok(chunk))) => st.buf.extend_from_slice(chunk.as_ref()),
            }
        }
    })
    .boxed()
}
