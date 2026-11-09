//! LLM provider contract (WS-C). Providers: Ollama (localhost), remote-peer (later), mock.

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LlmModelInfo {
    /// Provider-qualified name, e.g. "qwen3.5:9b".
    pub name: String,
    /// Content digest for provenance.
    pub digest: Option<String>,
    #[ts(type = "number | null")]
    pub size_bytes: Option<u64>,
    pub context_length: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    /// JSON Schema to constrain output (Ollama `format`). None = free text.
    pub json_schema: Option<serde_json::Value>,
    pub temperature: Option<f32>,
    /// Context window to request (Ollama `num_ctx`).
    pub num_ctx: Option<u32>,
    /// Disable model "thinking" output where supported.
    pub think: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ChatResponse {
    pub content: String,
    pub model: LlmModelInfo,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn health(&self) -> crate::Result<()>;
    async fn list_models(&self) -> crate::Result<Vec<LlmModelInfo>>;
    async fn chat(&self, req: ChatRequest) -> crate::Result<ChatResponse>;
    /// Streams content deltas.
    async fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> crate::Result<BoxStream<'static, crate::Result<String>>>;
}
