//! # almond-llm
//!
//! - [`ollama::Ollama`]: [`LlmProvider`](almond_types::llm::LlmProvider) for a local Ollama
//!   (`/api/chat` with streaming, JSON-Schema `format`, `think`, `num_ctx`, model digests).
//! - [`prompts`]: vi/en prompt templates stored as files (`prompts/<lang>/<task>.txt`).
//! - [`tasks::LlmTasks`]: summary (map-reduce, cited), enhanced notes (user text verbatim +
//!   AI blocks), action items, title — all with validated transcript citations ([`cite`]).
//! - [`processors`]: the tasks as pipeline [`Processor`](almond_types::pipeline::Processor)s.

pub mod cite;
pub mod notes;
pub mod ollama;
pub mod processors;
pub mod prompts;
pub mod schema;
pub mod tasks;
pub mod tokens;
pub mod transcript;

pub use almond_types as types;
pub use ollama::{Ollama, OllamaConfig};
pub use processors::{LlmProcessor, LlmTaskKind};
pub use prompts::PromptLibrary;
pub use tasks::{Hooks, LlmTasks, TaskConfig, TaskInput, TaskOutput, TaskReport};
pub use transcript::TranscriptView;
