//! JSON Schemas passed to the model as Ollama `format` (structured outputs), plus lenient
//! extraction of a JSON object from model text.

use serde_json::{Value, json};

fn cites() -> Value {
    json!({ "type": "array", "items": { "type": "integer" } })
}

/// `{ sections: [{ heading, bullets: [{ text, cites: [int] }] }] }`
pub fn summary() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sections": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "heading": { "type": "string" },
                        "bullets": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": { "text": { "type": "string" }, "cites": cites() },
                                "required": ["text", "cites"]
                            }
                        }
                    },
                    "required": ["heading", "bullets"]
                }
            }
        },
        "required": ["sections"]
    })
}

/// `{ user_cites: [{ note, cites }], additions: [{ after, style, level, text, cites }] }`
pub fn enhance_notes() -> Value {
    json!({
        "type": "object",
        "properties": {
            "user_cites": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "note": { "type": "integer" }, "cites": cites() },
                    "required": ["note", "cites"]
                }
            },
            "additions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "after": { "type": "integer" },
                        "style": { "type": "string", "enum": ["heading", "bullet", "todo", "paragraph"] },
                        "level": { "type": "integer" },
                        "text": { "type": "string" },
                        "cites": cites()
                    },
                    "required": ["after", "style", "text", "cites"]
                }
            }
        },
        "required": ["user_cites", "additions"]
    })
}

/// `{ items: [{ text, owner, due, cites }] }` (owner/due: "" when unknown — plain strings keep
/// the grammar simple for every Ollama engine).
pub fn action_items() -> Value {
    json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "owner": { "type": "string" },
                        "due": { "type": "string" },
                        "cites": cites()
                    },
                    "required": ["text", "owner", "due", "cites"]
                }
            }
        },
        "required": ["items"]
    })
}

/// `{ title }`
pub fn title() -> Value {
    json!({
        "type": "object",
        "properties": { "title": { "type": "string" } },
        "required": ["title"]
    })
}

/// Parse the first JSON object in `s` (tolerates code fences / leading prose / `<think>` blocks).
pub fn extract_json(s: &str) -> Option<Value> {
    let s = match s.rfind("</think>") {
        Some(i) => &s[i + "</think>".len()..],
        None => s,
    };
    if let Ok(v) = serde_json::from_str::<Value>(s.trim())
        && v.is_object()
    {
        return Some(v);
    }
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&s[start..=end])
        .ok()
        .filter(|v| v.is_object())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts() {
        assert_eq!(extract_json("{\"a\":1}").unwrap()["a"], 1);
        assert_eq!(
            extract_json("Sure!\n```json\n{\"a\": [1]}\n```").unwrap()["a"][0],
            1
        );
        assert_eq!(
            extract_json("<think>{x}</think>{\"t\":\"ok\"}").unwrap()["t"],
            "ok"
        );
        assert!(extract_json("no json").is_none());
        assert!(extract_json("[1,2]").is_none());
    }

    #[test]
    fn schemas_are_objects() {
        for s in [summary(), enhance_notes(), action_items(), title()] {
            assert_eq!(s["type"], "object");
        }
    }
}
