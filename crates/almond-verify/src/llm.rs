//! Structured-output helper over the `LlmProvider` trait (no dependency on almond-llm).

use almond_types::llm::{ChatMessage, ChatRequest, ChatResponse, LlmProvider, Role};
use serde::de::DeserializeOwned;

/// Strip `<think>…</think>` and code fences, then cut from the first `{` to the last `}`.
pub fn extract_json(s: &str) -> &str {
    let s = match s.rfind("</think>") {
        Some(i) => &s[i + "</think>".len()..],
        None => s,
    };
    match (s.find('{'), s.rfind('}')) {
        (Some(a), Some(b)) if b > a => &s[a..=b],
        _ => s.trim(),
    }
}

#[derive(Debug, Clone)]
pub struct JsonCall {
    pub model: String,
    pub temperature: f32,
    pub num_ctx: Option<u32>,
    /// Retries on unparsable output (each retry appends the parse error to the prompt).
    pub retries: u32,
}

impl JsonCall {
    pub fn new(model: impl Into<String>) -> Self {
        JsonCall {
            model: model.into(),
            temperature: 0.0,
            num_ctx: Some(8192),
            retries: 1,
        }
    }

    pub async fn call<T: DeserializeOwned>(
        &self,
        llm: &dyn LlmProvider,
        system: &str,
        user: &str,
        schema: serde_json::Value,
    ) -> almond_types::Result<(T, ChatResponse)> {
        let mut messages = vec![
            ChatMessage {
                role: Role::System,
                content: system.to_string(),
            },
            ChatMessage {
                role: Role::User,
                content: user.to_string(),
            },
        ];
        let mut last_err = None;
        for _ in 0..=self.retries {
            let resp = llm
                .chat(ChatRequest {
                    model: self.model.clone(),
                    messages: messages.clone(),
                    json_schema: Some(schema.clone()),
                    temperature: Some(self.temperature),
                    num_ctx: self.num_ctx,
                    think: Some(false),
                })
                .await?;
            match serde_json::from_str::<T>(extract_json(&resp.content)) {
                Ok(v) => return Ok((v, resp)),
                Err(e) => {
                    messages.push(ChatMessage {
                        role: Role::Assistant,
                        content: resp.content.clone(),
                    });
                    messages.push(ChatMessage {
                        role: Role::User,
                        content: format!(
                            "That was not valid JSON for the schema ({e}). Reply with JSON only."
                        ),
                    });
                    last_err = Some(e);
                }
            }
        }
        Err(almond_types::Error::Invalid(format!(
            "LLM returned invalid JSON: {}",
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )))
    }
}

/// "ollama:qwen3.5:9b@digest"-style model reference for provenance.
pub fn model_ref(provider: &str, resp: &ChatResponse) -> String {
    match &resp.model.digest {
        Some(d) => format!("{provider}:{}@{d}", resp.model.name),
        None => format!("{provider}:{}", resp.model.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_wrappers() {
        assert_eq!(
            extract_json("<think>x {}</think>\n```json\n{\"a\":1}\n```"),
            "{\"a\":1}"
        );
        assert_eq!(extract_json("{\"a\":{\"b\":2}}"), "{\"a\":{\"b\":2}}");
    }
}
