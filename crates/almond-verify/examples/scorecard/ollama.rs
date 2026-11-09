//! Tiny private Ollama client implementing `LlmProvider` for dev/eval runs only. The real
//! provider lives in `almond-llm` (WS-C); this crate must not depend on it.

use almond_types::llm::{ChatRequest, ChatResponse, LlmModelInfo, LlmProvider, Role};
use almond_types::{Error, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::{Value, json};

pub struct Ollama {
    pub base: String,
    http: reqwest::Client,
}

impl Ollama {
    pub fn new(base: &str) -> Self {
        Ollama {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .expect("http client"),
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?
            .json()
            .await
            .map_err(|e| Error::Network(e.to_string()))
    }
}

#[async_trait]
impl LlmProvider for Ollama {
    fn name(&self) -> &str {
        "ollama"
    }

    async fn health(&self) -> Result<()> {
        self.get("/api/version").await.map(|_| ())
    }

    async fn list_models(&self) -> Result<Vec<LlmModelInfo>> {
        let v = self.get("/api/tags").await?;
        Ok(v["models"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|m| LlmModelInfo {
                name: m["name"].as_str().unwrap_or_default().into(),
                digest: m["digest"].as_str().map(|d| d.chars().take(12).collect()),
                size_bytes: m["size"].as_u64(),
                context_length: None,
            })
            .collect())
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                json!({"role": match m.role { Role::System => "system", Role::User => "user", Role::Assistant => "assistant" },
                       "content": m.content})
            })
            .collect();
        let mut options = json!({});
        if let Some(t) = req.temperature {
            options["temperature"] = json!(t);
        }
        if let Some(n) = req.num_ctx {
            options["num_ctx"] = json!(n);
        }
        let mut body = json!({"model": req.model, "messages": messages, "stream": false, "options": options});
        if let Some(s) = &req.json_schema {
            body["format"] = s.clone();
        }
        if let Some(t) = req.think {
            body["think"] = json!(t);
        }
        let resp = self
            .http
            .post(format!("{}/api/chat", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(Error::Other(format!("ollama {status}: {v}")));
        }
        let digest = self
            .list_models()
            .await
            .ok()
            .and_then(|ms| ms.into_iter().find(|m| m.name == req.model))
            .and_then(|m| m.digest);
        Ok(ChatResponse {
            content: v["message"]["content"].as_str().unwrap_or_default().into(),
            model: LlmModelInfo {
                name: req.model.clone(),
                digest,
                size_bytes: None,
                context_length: req.num_ctx,
            },
            prompt_tokens: v["prompt_eval_count"].as_u64().map(|x| x as u32),
            completion_tokens: v["eval_count"].as_u64().map(|x| x as u32),
        })
    }

    async fn chat_stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, Result<String>>> {
        Err(Error::Unsupported("streaming not needed for eval".into()))
    }
}
