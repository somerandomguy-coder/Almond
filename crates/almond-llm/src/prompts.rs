//! Prompt templates, stored as files keyed by `(task, template language)`:
//! `crates/almond-llm/prompts/<lang>/<task>.txt`. Built-ins are compiled in; a user directory
//! with the same layout can override any of them (`PromptLibrary::with_override_dir`).
//!
//! File format:
//! ```text
//! # comment lines before the first section are ignored
//! === system ===
//! ...system prompt...
//! === user ===
//! ...user prompt with {{placeholders}}...
//! ```
//! A template's identity for provenance is `"<task>/<lang>@<blake3[..12]>"`, so editing a prompt
//! changes `Provenance::params_hash` and reruns are comparable.
//!
//! Languages: `vi`, `en` ship today. Any other output language (e.g. `zh`) falls back to the
//! English template, which always says "Write in {{output_language}}".

use almond_types::{Error, LangTag, Result};
use std::path::PathBuf;

/// Tasks with templates.
pub mod task {
    pub const SUMMARY: &str = "summary";
    pub const SUMMARY_REDUCE: &str = "summary_reduce";
    pub const ENHANCE_NOTES: &str = "enhance_notes";
    pub const ACTION_ITEMS: &str = "action_items";
    pub const TITLE: &str = "title";
    pub const ALL: [&str; 5] = [SUMMARY, SUMMARY_REDUCE, ENHANCE_NOTES, ACTION_ITEMS, TITLE];
}

/// Template languages shipped with the binary.
pub const BUILTIN_LANGS: [&str; 2] = ["en", "vi"];

fn builtin(lang: &str, task: &str) -> Option<&'static str> {
    Some(match (lang, task) {
        ("en", task::SUMMARY) => include_str!("../prompts/en/summary.txt"),
        ("en", task::SUMMARY_REDUCE) => include_str!("../prompts/en/summary_reduce.txt"),
        ("en", task::ENHANCE_NOTES) => include_str!("../prompts/en/enhance_notes.txt"),
        ("en", task::ACTION_ITEMS) => include_str!("../prompts/en/action_items.txt"),
        ("en", task::TITLE) => include_str!("../prompts/en/title.txt"),
        ("vi", task::SUMMARY) => include_str!("../prompts/vi/summary.txt"),
        ("vi", task::SUMMARY_REDUCE) => include_str!("../prompts/vi/summary_reduce.txt"),
        ("vi", task::ENHANCE_NOTES) => include_str!("../prompts/vi/enhance_notes.txt"),
        ("vi", task::ACTION_ITEMS) => include_str!("../prompts/vi/action_items.txt"),
        ("vi", task::TITLE) => include_str!("../prompts/vi/title.txt"),
        _ => return None,
    })
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub task: String,
    /// Language the template is written in (may differ from the output language).
    pub lang: String,
    /// BLAKE3 of the file contents, first 12 hex chars.
    pub hash: String,
    pub system: String,
    pub user: String,
}

impl Template {
    pub fn parse(task: &str, lang: &str, src: &str) -> Result<Self> {
        let mut system = String::new();
        let mut user = String::new();
        let mut cur: Option<&mut String> = None;
        for line in src.lines() {
            match line.trim() {
                "=== system ===" => cur = Some(&mut system),
                "=== user ===" => cur = Some(&mut user),
                _ => {
                    if let Some(buf) = cur.as_deref_mut() {
                        buf.push_str(line);
                        buf.push('\n');
                    }
                }
            }
        }
        if user.trim().is_empty() {
            return Err(Error::Invalid(format!(
                "prompt template {task}/{lang} has no `=== user ===` section"
            )));
        }
        // Hash with normalized line endings so CRLF checkouts produce the same id.
        let normalized = src.replace("\r\n", "\n");
        let hash = blake3::hash(normalized.as_bytes()).to_hex()[..12].to_string();
        Ok(Template {
            task: task.into(),
            lang: lang.into(),
            hash,
            system: system.trim().to_string(),
            user: user.trim().to_string(),
        })
    }

    /// `"summary/vi@3f2a9c0d1e2b"`.
    pub fn id(&self) -> String {
        format!("{}/{}@{}", self.task, self.lang, self.hash)
    }

    /// Replace `{{name}}` placeholders. Unknown placeholders are an error (catches typos).
    pub fn render(&self, vars: &[(&str, &str)]) -> Result<(String, String)> {
        Ok((fill(&self.system, vars)?, fill(&self.user, vars)?))
    }
}

fn fill(s: &str, vars: &[(&str, &str)]) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        let Some(end) = rest[start..].find("}}") else {
            break;
        };
        let name = rest[start + 2..start + end].trim();
        out.push_str(&rest[..start]);
        match vars.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => out.push_str(v),
            None => {
                return Err(Error::Invalid(format!(
                    "prompt placeholder {{{{{name}}}}} has no value"
                )));
            }
        }
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Lookup of templates by `(task, language)` with an optional override directory.
#[derive(Debug, Clone, Default)]
pub struct PromptLibrary {
    override_dir: Option<PathBuf>,
}

impl PromptLibrary {
    pub fn builtin() -> Self {
        Self::default()
    }

    /// Files in `dir/<lang>/<task>.txt` take precedence over built-ins.
    pub fn with_override_dir(dir: impl Into<PathBuf>) -> Self {
        PromptLibrary {
            override_dir: Some(dir.into()),
        }
    }

    /// Template for `task` written in `lang` (falls back to English).
    pub fn get(&self, task: &str, lang: &LangTag) -> Result<Template> {
        let primary = lang.primary();
        for l in [primary.as_str(), "en"] {
            if let Some(dir) = &self.override_dir {
                let p = dir.join(l).join(format!("{task}.txt"));
                if p.is_file() {
                    return Template::parse(task, l, &std::fs::read_to_string(&p)?);
                }
            }
            if let Some(src) = builtin(l, task) {
                return Template::parse(task, l, src);
            }
        }
        Err(Error::NotFound(format!("prompt template {task}/{primary}")))
    }
}

/// Human name of `lang` as written inside a template in `template_lang`
/// (the `{{output_language}}` placeholder).
pub fn language_name(template_lang: &str, lang: &LangTag) -> String {
    let p = lang.primary();
    let trad = lang.0.to_ascii_lowercase().contains("hant");
    if template_lang == "vi" {
        return match p.as_str() {
            "vi" => "tiếng Việt".into(),
            "en" => "tiếng Anh".into(),
            "zh" if trad => "tiếng Trung phồn thể".into(),
            "zh" => "tiếng Trung giản thể".into(),
            _ => lang.0.clone(),
        };
    }
    match p.as_str() {
        "vi" => "Vietnamese (tiếng Việt)".into(),
        "en" => "English".into(),
        "zh" if trad => "Traditional Chinese (繁體中文)".into(),
        "zh" => "Simplified Chinese (简体中文)".into(),
        "ja" => "Japanese (日本語)".into(),
        "ko" => "Korean (한국어)".into(),
        "fr" => "French (français)".into(),
        "de" => "German (Deutsch)".into(),
        _ => lang.0.clone(),
    }
}

/// The "part i of n" note used in the map step, in the template's language.
pub fn part_note(template_lang: &str, part: usize, parts: usize) -> String {
    if parts <= 1 {
        return String::new();
    }
    match template_lang {
        "vi" => format!(
            "Lưu ý: đây là phần {part}/{parts} của một cuộc họp dài. Chỉ tóm tắt nội dung của phần này."
        ),
        _ => format!(
            "Note: this is part {part} of {parts} of a longer meeting. Summarize only this part."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtins_parse_and_render() {
        let lib = PromptLibrary::builtin();
        let vars = [
            ("output_language", "English"),
            ("meeting_title", "T"),
            ("meeting_date", "2026-11-09"),
            ("transcript", "[0] 00:00 A: hi"),
            ("part_note", ""),
            ("notes", "(n0) x"),
            ("partials", "- a (cites: 0)"),
        ];
        for l in BUILTIN_LANGS {
            for t in task::ALL {
                let tpl = lib.get(t, &LangTag(l.into())).unwrap();
                assert_eq!(tpl.lang, l);
                let (sys, user) = tpl.render(&vars).unwrap_or_else(|e| panic!("{t}/{l}: {e}"));
                assert!(!sys.is_empty(), "{t}/{l} system");
                assert!(!user.contains("{{"), "{t}/{l}");
            }
        }
    }

    #[test]
    fn fallback_and_errors() {
        let lib = PromptLibrary::builtin();
        let t = lib.get(task::SUMMARY, &LangTag::zh()).unwrap();
        assert_eq!(t.lang, "en");
        assert!(t.id().starts_with("summary/en@"));
        assert!(lib.get("nope", &LangTag::en()).is_err());
        let t = Template::parse("x", "en", "=== user ===\nhi {{a}}").unwrap();
        assert!(t.render(&[]).is_err());
        assert_eq!(t.render(&[("a", "b")]).unwrap().1, "hi b");
        assert!(Template::parse("x", "en", "no sections").is_err());
    }

    #[test]
    fn override_dir_wins() {
        let dir = std::env::temp_dir().join(format!("almond-prompts-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("vi")).unwrap();
        std::fs::write(
            dir.join("vi").join("title.txt"),
            "=== system ===\ns\n=== user ===\ncustom {{transcript}}",
        )
        .unwrap();
        let lib = PromptLibrary::with_override_dir(&dir);
        let t = lib.get(task::TITLE, &LangTag::vi()).unwrap();
        assert_eq!(t.user, "custom {{transcript}}");
        // Others still come from built-ins.
        assert!(lib.get(task::SUMMARY, &LangTag::vi()).unwrap().user.len() > 50);
        std::fs::remove_dir_all(&dir).ok();
    }
}
