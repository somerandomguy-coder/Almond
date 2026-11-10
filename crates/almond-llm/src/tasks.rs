//! The LLM tasks: summary (map-reduce), enhanced notes, action items, title.
//!
//! All tasks use structured output (JSON Schema) and cite transcript segments by index; indices
//! are validated with [`CiteResolver`] before they become [`SpanRef`](almond_types::SpanRef)s.

use crate::cite::{CiteResolver, CiteStats};
use crate::notes::{UserLine, parse_user_notes, render_for_prompt};
use crate::prompts::{PromptLibrary, Template, language_name, part_note, task};
use crate::schema;
use crate::tokens::estimate_tokens;
use crate::transcript::TranscriptView;
use almond_types::artifact::body::{
    ActionItem, ActionItems, CitedText, EnhancedNotes, NoteBlock, NoteOrigin, Summary,
    SummarySection, Title,
};
use almond_types::llm::{ChatMessage, ChatRequest, LlmModelInfo, LlmProvider, Role};
use almond_types::pipeline::ProgressFn;
use almond_types::{Error, LangTag, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Knobs shared by all tasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskConfig {
    /// Provider model name, e.g. `qwen3.5:4b` (default: [`default_model`]).
    pub model: String,
    pub temperature: Option<f32>,
    /// `Some(false)` disables reasoning traces (faster, and JSON stays clean).
    pub think: Option<bool>,
    /// Explicit context window; None = provider estimates from the prompt.
    pub num_ctx: Option<u32>,
    /// Transcript token budget per LLM call; longer transcripts are map-reduced.
    pub chunk_tokens: usize,
    /// Transcript prefix used for titles.
    pub title_tokens: usize,
    /// Language of the prompt *instructions*; None = same as the output language. Default
    /// `en`: English instructions + "write in Vietnamese" scored better for small models on
    /// VI output (eval/summary/RESULTS.md) and are equal for gemma4:12b. The vi templates
    /// stay available (`template_lang: None` or `"vi"`).
    pub template_lang: Option<LangTag>,
    /// Extra attempts when the model returns unparsable JSON.
    pub json_retries: u32,
}

/// Built-in default summarizer model (PC class, 16 GB VRAM); override per device with
/// `ALMOND_LLM_MODEL`, `TaskConfig::model`, or the `model` job param (weak laptop:
/// `qwen3.5:4b`). See eval/summary/RESULTS.md.
pub const DEFAULT_MODEL: &str = "gemma4:12b";

/// `ALMOND_LLM_MODEL` if set, else [`DEFAULT_MODEL`].
pub fn default_model() -> String {
    std::env::var("ALMOND_LLM_MODEL")
        .ok()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

impl Default for TaskConfig {
    fn default() -> Self {
        TaskConfig {
            model: default_model(),
            temperature: Some(0.2),
            think: Some(false),
            num_ctx: None,
            chunk_tokens: 6_000,
            title_tokens: 1_500,
            template_lang: Some(LangTag::en()),
            json_retries: 1,
        }
    }
}

/// Progress + cancellation plumbing (mirrors `JobCtx`).
#[derive(Clone, Default)]
pub struct Hooks {
    pub progress: Option<ProgressFn>,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Hooks {
    fn report(&self, f: f32, msg: &str) {
        if let Some(p) = &self.progress {
            p(f.clamp(0.0, 1.0), msg);
        }
    }
    fn check(&self) -> Result<()> {
        match &self.cancel {
            Some(c) if c.load(Ordering::Relaxed) => Err(Error::Cancelled),
            _ => Ok(()),
        }
    }
}

/// What every task needs to know about the meeting.
pub struct TaskInput<'a> {
    pub transcript: &'a TranscriptView,
    pub meeting_title: String,
    /// `YYYY-MM-DD`, lets the model resolve "thứ Sáu" / "next Friday".
    pub meeting_date: Option<String>,
    pub output_lang: LangTag,
}

/// Diagnostics for one task run (also used by the eval harness).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskReport {
    /// Template ids (`summary/vi@<hash>`) used, in call order (deduplicated).
    pub templates: Vec<String>,
    pub model: Option<LlmModelInfo>,
    pub cites: CiteStats,
    /// Raw citation values that were dropped.
    pub dropped_cites: Vec<String>,
    pub llm_calls: u32,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub elapsed_ms: u64,
    /// Number of map calls (0 = single pass).
    pub map_parts: u32,
}

impl TaskReport {
    fn template(&mut self, t: &Template) {
        let id = t.id();
        if !self.templates.contains(&id) {
            self.templates.push(id);
        }
    }
}

pub struct TaskOutput<T> {
    pub body: T,
    pub report: TaskReport,
}

/// Runs the tasks against one [`LlmProvider`].
#[derive(Clone)]
pub struct LlmTasks {
    llm: Arc<dyn LlmProvider>,
    prompts: PromptLibrary,
    cfg: TaskConfig,
}

#[derive(Debug, Clone)]
struct RawBullet {
    text: String,
    cites: Vec<Value>,
}

type RawSections = Vec<(String, Vec<RawBullet>)>;

impl LlmTasks {
    pub fn new(llm: Arc<dyn LlmProvider>, prompts: PromptLibrary, cfg: TaskConfig) -> Self {
        LlmTasks { llm, prompts, cfg }
    }

    pub fn config(&self) -> &TaskConfig {
        &self.cfg
    }

    pub fn config_mut(&mut self) -> &mut TaskConfig {
        &mut self.cfg
    }

    pub fn provider(&self) -> &Arc<dyn LlmProvider> {
        &self.llm
    }

    fn template(&self, name: &str, out: &LangTag) -> Result<Template> {
        let l = self.cfg.template_lang.as_ref().unwrap_or(out);
        self.prompts.get(name, l)
    }

    /// One structured call; retries on unparsable JSON.
    async fn call_json(
        &self,
        tpl: &Template,
        vars: &[(&str, &str)],
        schema: Value,
        report: &mut TaskReport,
    ) -> Result<Value> {
        report.template(tpl);
        let (system, user) = tpl.render(vars)?;
        let mut messages = vec![];
        if !system.is_empty() {
            messages.push(ChatMessage {
                role: Role::System,
                content: system,
            });
        }
        messages.push(ChatMessage {
            role: Role::User,
            content: user,
        });
        let mut last_err = String::new();
        for attempt in 0..=self.cfg.json_retries {
            let req = ChatRequest {
                model: self.cfg.model.clone(),
                messages: messages.clone(),
                json_schema: Some(schema.clone()),
                temperature: self.cfg.temperature,
                num_ctx: self.cfg.num_ctx,
                think: self.cfg.think,
            };
            let resp = self.llm.chat(req).await?;
            report.llm_calls += 1;
            report.prompt_tokens += resp.prompt_tokens.unwrap_or(0);
            report.completion_tokens += resp.completion_tokens.unwrap_or(0);
            report.model = Some(resp.model.clone());
            if let Some(v) = schema::extract_json(&resp.content) {
                return Ok(v);
            }
            last_err = resp.content.chars().take(200).collect();
            if attempt < self.cfg.json_retries {
                messages.push(ChatMessage {
                    role: Role::Assistant,
                    content: resp.content,
                });
                messages.push(ChatMessage {
                    role: Role::User,
                    content: "That was not valid JSON. Reply again with only the JSON object."
                        .into(),
                });
            }
        }
        Err(Error::Invalid(format!(
            "model {} returned no valid JSON for {}: {last_err:?}",
            self.cfg.model,
            tpl.id()
        )))
    }

    fn base_vars(&self, input: &TaskInput<'_>, tpl: &Template) -> (String, String, String) {
        (
            language_name(&tpl.lang, &input.output_lang),
            if input.meeting_title.trim().is_empty() {
                "-".to_string()
            } else {
                input.meeting_title.clone()
            },
            input.meeting_date.clone().unwrap_or_else(|| "-".into()),
        )
    }

    // ------------------------------------------------------------------ summary

    /// Summary with cited bullets. Transcripts over `chunk_tokens` are map-reduced
    /// (indices stay global, so citations survive both steps).
    pub async fn summarize(
        &self,
        input: &TaskInput<'_>,
        hooks: &Hooks,
    ) -> Result<TaskOutput<Summary>> {
        let t0 = Instant::now();
        let mut report = TaskReport::default();
        let tpl = self.template(task::SUMMARY, &input.output_lang)?;
        let (lang_name, title, _) = self.base_vars(input, &tpl);
        let chunks = input.transcript.chunks(self.cfg.chunk_tokens);
        if chunks.is_empty() {
            return Err(Error::Invalid("transcript is empty".into()));
        }
        let n = chunks.len();
        let mut parts: Vec<RawSections> = vec![];
        for (i, chunk) in chunks.iter().enumerate() {
            hooks.check()?;
            hooks.report(
                i as f32 / (n as f32 + 1.0),
                &format!("summarizing part {}/{}", i + 1, n),
            );
            let text = input.transcript.render(chunk);
            let note = part_note(&tpl.lang, i + 1, n);
            let v = self
                .call_json(
                    &tpl,
                    &[
                        ("output_language", &lang_name),
                        ("meeting_title", &title),
                        ("part_note", &note),
                        ("transcript", &text),
                    ],
                    schema::summary(),
                    &mut report,
                )
                .await?;
            parts.push(parse_sections(&v));
        }
        let raw = if n == 1 {
            parts.pop().unwrap_or_default()
        } else {
            report.map_parts = n as u32;
            hooks.report(n as f32 / (n as f32 + 1.0), "merging parts");
            self.reduce(input, parts, hooks, &mut report).await?
        };

        let mut res = CiteResolver::new(input.transcript);
        let mut sections = vec![];
        for (heading, bullets) in raw {
            let bullets: Vec<CitedText> = bullets
                .into_iter()
                .filter(|b| !b.text.trim().is_empty())
                .map(|b| {
                    let mut text = b.text.trim().to_string();
                    let cites = res.resolve(&b.cites, &mut text);
                    CitedText { text, cites }
                })
                .collect();
            if !bullets.is_empty() {
                sections.push(SummarySection {
                    heading: heading.trim().to_string(),
                    bullets,
                });
            }
        }
        report.cites = res.stats;
        report.dropped_cites = res.dropped;
        report.elapsed_ms = t0.elapsed().as_millis() as u64;
        hooks.report(1.0, "summary done");
        Ok(TaskOutput {
            body: Summary {
                lang: Some(input.output_lang.clone()),
                sections,
            },
            report,
        })
    }

    async fn reduce(
        &self,
        input: &TaskInput<'_>,
        mut parts: Vec<RawSections>,
        hooks: &Hooks,
        report: &mut TaskReport,
    ) -> Result<RawSections> {
        let tpl = self.template(task::SUMMARY_REDUCE, &input.output_lang)?;
        let (lang_name, title, _) = self.base_vars(input, &tpl);
        // Hierarchical: merge as many consecutive parts as fit the budget, repeat until one.
        loop {
            hooks.check()?;
            let mut groups: Vec<Vec<RawSections>> = vec![];
            let mut cur: Vec<RawSections> = vec![];
            let mut cur_tok = 0;
            for p in parts.drain(..) {
                let t = estimate_tokens(&render_partials(std::slice::from_ref(&p), 0));
                // Groups hold >= 2 parts so every round shrinks the list (guaranteed progress).
                if cur.len() >= 2 && cur_tok + t > self.cfg.chunk_tokens {
                    groups.push(std::mem::take(&mut cur));
                    cur_tok = 0;
                }
                cur_tok += t;
                cur.push(p);
            }
            if !cur.is_empty() {
                groups.push(cur);
            }
            let single = groups.len() == 1;
            let mut next = vec![];
            for (gi, g) in groups.iter().enumerate() {
                let partials = render_partials(g, gi * 1000);
                let v = self
                    .call_json(
                        &tpl,
                        &[
                            ("output_language", &lang_name),
                            ("meeting_title", &title),
                            ("partials", &partials),
                        ],
                        schema::summary(),
                        report,
                    )
                    .await?;
                next.push(parse_sections(&v));
            }
            if single {
                return Ok(next.pop().unwrap_or_default());
            }
            parts = next;
        }
    }

    // ------------------------------------------------------------------ enhanced notes

    /// Keeps every user line verbatim (`origin: user`), attaches citations to it, and inserts
    /// AI blocks (`origin: ai`) under the user line they expand.
    pub async fn enhance_notes(
        &self,
        input: &TaskInput<'_>,
        user_notes: &str,
        hooks: &Hooks,
    ) -> Result<TaskOutput<EnhancedNotes>> {
        let t0 = Instant::now();
        let mut report = TaskReport::default();
        let tpl = self.template(task::ENHANCE_NOTES, &input.output_lang)?;
        let (lang_name, title, _) = self.base_vars(input, &tpl);
        let lines = parse_user_notes(user_notes);
        let notes_txt = render_for_prompt(&lines);
        let chunks = input.transcript.chunks(self.cfg.chunk_tokens);
        if chunks.is_empty() {
            return Err(Error::Invalid("transcript is empty".into()));
        }
        let n = chunks.len();
        let mut user_cites: BTreeMap<usize, Vec<Value>> = BTreeMap::new();
        let mut additions: Vec<RawAddition> = vec![];
        for (i, chunk) in chunks.iter().enumerate() {
            hooks.check()?;
            hooks.report(
                i as f32 / n as f32,
                &format!("enhancing notes {}/{}", i + 1, n),
            );
            let text = input.transcript.render(chunk);
            let v = self
                .call_json(
                    &tpl,
                    &[
                        ("output_language", &lang_name),
                        ("meeting_title", &title),
                        ("notes", &notes_txt),
                        ("transcript", &text),
                    ],
                    schema::enhance_notes(),
                    &mut report,
                )
                .await?;
            for uc in v["user_cites"].as_array().into_iter().flatten() {
                if let Some(k) = uc["note"].as_i64().filter(|k| *k >= 0) {
                    user_cites
                        .entry(k as usize)
                        .or_default()
                        .extend(uc["cites"].as_array().cloned().unwrap_or_default());
                }
            }
            additions.extend(parse_additions(&v));
        }
        if n > 1 {
            report.map_parts = n as u32;
            dedupe_additions(&mut additions);
        }
        let mut res = CiteResolver::new(input.transcript);
        let blocks = merge_notes(&lines, &user_cites, additions, &mut res);
        report.cites = res.stats;
        report.dropped_cites = res.dropped;
        report.elapsed_ms = t0.elapsed().as_millis() as u64;
        hooks.report(1.0, "notes done");
        Ok(TaskOutput {
            body: EnhancedNotes {
                lang: Some(input.output_lang.clone()),
                blocks,
            },
            report,
        })
    }

    // ------------------------------------------------------------------ action items

    pub async fn action_items(
        &self,
        input: &TaskInput<'_>,
        hooks: &Hooks,
    ) -> Result<TaskOutput<ActionItems>> {
        let t0 = Instant::now();
        let mut report = TaskReport::default();
        let tpl = self.template(task::ACTION_ITEMS, &input.output_lang)?;
        let (lang_name, title, date) = self.base_vars(input, &tpl);
        let chunks = input.transcript.chunks(self.cfg.chunk_tokens);
        let n = chunks.len();
        let mut res = CiteResolver::new(input.transcript);
        let mut items: Vec<ActionItem> = vec![];
        for (i, chunk) in chunks.iter().enumerate() {
            hooks.check()?;
            hooks.report(i as f32 / n.max(1) as f32, "extracting action items");
            let text = input.transcript.render(chunk);
            let v = self
                .call_json(
                    &tpl,
                    &[
                        ("output_language", &lang_name),
                        ("meeting_title", &title),
                        ("meeting_date", &date),
                        ("transcript", &text),
                    ],
                    schema::action_items(),
                    &mut report,
                )
                .await?;
            for it in v["items"].as_array().into_iter().flatten() {
                let mut text = str_field(it, "text").unwrap_or_default();
                if text.trim().is_empty() {
                    continue;
                }
                let cites = res.resolve(
                    it["cites"].as_array().map(Vec::as_slice).unwrap_or(&[]),
                    &mut text,
                );
                let key = norm(&text);
                if items.iter().any(|x| norm(&x.text) == key) {
                    continue;
                }
                items.push(ActionItem {
                    text,
                    owner: str_field(it, "owner"),
                    due: str_field(it, "due"),
                    cites,
                });
            }
        }
        if n > 1 {
            report.map_parts = n as u32;
        }
        report.cites = res.stats;
        report.dropped_cites = res.dropped;
        report.elapsed_ms = t0.elapsed().as_millis() as u64;
        hooks.report(1.0, "action items done");
        Ok(TaskOutput {
            body: ActionItems { items },
            report,
        })
    }

    // ------------------------------------------------------------------ title

    pub async fn title(&self, input: &TaskInput<'_>, hooks: &Hooks) -> Result<TaskOutput<Title>> {
        let t0 = Instant::now();
        let mut report = TaskReport::default();
        hooks.check()?;
        let tpl = self.template(task::TITLE, &input.output_lang)?;
        let (lang_name, _, _) = self.base_vars(input, &tpl);
        let first = input
            .transcript
            .chunks(self.cfg.title_tokens)
            .into_iter()
            .next()
            .ok_or_else(|| Error::Invalid("transcript is empty".into()))?;
        let text = input.transcript.render(&first);
        let v = self
            .call_json(
                &tpl,
                &[("output_language", &lang_name), ("transcript", &text)],
                schema::title(),
                &mut report,
            )
            .await?;
        let mut title = str_field(&v, "title").unwrap_or_default();
        title = title
            .trim()
            .trim_matches(['"', '\'', '“', '”', '«', '»'])
            .trim_end_matches('.')
            .trim()
            .to_string();
        if title.is_empty() {
            return Err(Error::Invalid("model returned an empty title".into()));
        }
        report.elapsed_ms = t0.elapsed().as_millis() as u64;
        hooks.report(1.0, "title done");
        Ok(TaskOutput {
            body: Title { title },
            report,
        })
    }
}

// ---------------------------------------------------------------------- parsing helpers

fn str_field(v: &Value, k: &str) -> Option<String> {
    let s = v.get(k)?.as_str()?.trim();
    let lower = s.to_lowercase();
    if s.is_empty()
        || matches!(
            lower.as_str(),
            "null" | "none" | "n/a" | "na" | "-" | "unknown" | "không" | "không có" | "chưa rõ"
        )
    {
        None
    } else {
        Some(s.to_string())
    }
}

fn norm(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_sections(v: &Value) -> RawSections {
    let mut out = vec![];
    for s in v["sections"].as_array().into_iter().flatten() {
        let heading = s["heading"].as_str().unwrap_or("").to_string();
        let bullets = s["bullets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| match b {
                Value::String(t) => Some(RawBullet {
                    text: t.clone(),
                    cites: vec![],
                }),
                Value::Object(_) => Some(RawBullet {
                    text: b["text"].as_str().unwrap_or("").to_string(),
                    cites: b["cites"].as_array().cloned().unwrap_or_default(),
                }),
                _ => None,
            })
            .collect();
        out.push((heading, bullets));
    }
    out
}

/// Render partial summaries for the reduce prompt: `- text (cites: 1, 2)`.
fn render_partials(parts: &[RawSections], offset: usize) -> String {
    let mut s = String::new();
    for (i, p) in parts.iter().enumerate() {
        s.push_str(&format!("## Part {}\n", offset + i + 1));
        for (h, bullets) in p {
            if !h.trim().is_empty() {
                s.push_str(&format!("### {}\n", h.trim()));
            }
            for b in bullets {
                let cites: Vec<String> = b
                    .cites
                    .iter()
                    .map(|c| match c {
                        Value::String(x) => x.clone(),
                        other => other.to_string(),
                    })
                    .collect();
                s.push_str(&format!(
                    "- {} (cites: {})\n",
                    b.text.trim(),
                    cites.join(", ")
                ));
            }
        }
        s.push('\n');
    }
    s
}

#[derive(Debug, Clone)]
struct RawAddition {
    after: i64,
    style: String,
    level: u8,
    text: String,
    cites: Vec<Value>,
}

fn parse_additions(v: &Value) -> Vec<RawAddition> {
    v["additions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let text = a["text"].as_str()?.trim().to_string();
            if text.is_empty() {
                return None;
            }
            let style = match a["style"].as_str().unwrap_or("bullet") {
                s @ ("heading" | "bullet" | "todo" | "paragraph") => s.to_string(),
                _ => "bullet".to_string(),
            };
            Some(RawAddition {
                after: a["after"].as_i64().unwrap_or(-1),
                level: a["level"].as_u64().unwrap_or(0).min(4) as u8,
                style,
                text,
                cites: a["cites"].as_array().cloned().unwrap_or_default(),
            })
        })
        .collect()
}

fn dedupe_additions(v: &mut Vec<RawAddition>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|a| seen.insert((a.after, norm(&a.text))));
}

fn merge_notes(
    lines: &[UserLine],
    user_cites: &BTreeMap<usize, Vec<Value>>,
    additions: Vec<RawAddition>,
    res: &mut CiteResolver<'_>,
) -> Vec<NoteBlock> {
    let n = lines.len() as i64;
    let mut by_after: BTreeMap<i64, Vec<RawAddition>> = BTreeMap::new();
    for a in additions {
        let k = if a.after < 0 {
            -1
        } else if a.after >= n {
            n // misplaced -> append at the end
        } else {
            a.after
        };
        by_after.entry(k).or_default().push(a);
    }
    let ai = |a: RawAddition, res: &mut CiteResolver<'_>| {
        let mut text = a.text;
        let cites = res.resolve(&a.cites, &mut text);
        NoteBlock {
            origin: NoteOrigin::Ai,
            style: a.style,
            level: a.level,
            text,
            cites,
        }
    };
    let mut out = vec![];
    for a in by_after.remove(&-1).unwrap_or_default() {
        out.push(ai(a, res));
    }
    for (i, l) in lines.iter().enumerate() {
        // User text is never touched: resolve cites against a scratch copy.
        let cites = match user_cites.get(&i) {
            Some(c) if !c.is_empty() => {
                let mut scratch = String::new();
                res.resolve(c, &mut scratch)
            }
            _ => vec![],
        };
        out.push(NoteBlock {
            origin: NoteOrigin::User,
            style: l.style.clone(),
            level: l.level,
            text: l.text.clone(),
            cites,
        });
        for a in by_after.remove(&(i as i64)).unwrap_or_default() {
            out.push(ai(a, res));
        }
    }
    for (_, rest) in by_after {
        for a in rest {
            out.push(ai(a, res));
        }
    }
    out
}

/// `YYYY-MM-DD` (UTC) for a Unix-ms timestamp.
pub fn iso_date(ms: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::ids::DeviceId;
    use almond_types::mock::{MockLlm, fixtures};
    use serde_json::json;

    fn input(v: &TranscriptView) -> TaskInput<'_> {
        TaskInput {
            transcript: v,
            meeting_title: "Họp kế hoạch Q4".into(),
            meeting_date: Some("2026-11-09".into()),
            output_lang: LangTag::vi(),
        }
    }

    fn view() -> TranscriptView {
        let m = fixtures::meeting(0);
        TranscriptView::from_artifact(&fixtures::transcript(&m.id, &DeviceId("d".into()))).unwrap()
    }

    fn tasks(reply: Value) -> LlmTasks {
        LlmTasks::new(
            Arc::new(MockLlm {
                reply: String::new(),
                json_reply: reply,
            }),
            PromptLibrary::builtin(),
            TaskConfig {
                template_lang: None, // exercise the vi templates
                ..Default::default()
            },
        )
    }

    #[test]
    fn dates() {
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(1_762_646_400_000), "2025-11-09");
        assert_eq!(iso_date(951_782_400_000), "2000-02-29");
    }

    #[tokio::test]
    async fn summary_validates_cites() {
        let v = view();
        let t = tasks(json!({"sections": [
            {"heading": "Quyết định", "bullets": [
                {"text": "Ngân sách marketing 200 triệu.", "cites": [1]},
                {"text": "Tuyển 2 kỹ sư backend [2].", "cites": [42]},
                {"text": "", "cites": [0]}
            ]},
            {"heading": "Trống", "bullets": []}
        ]}));
        let out = t.summarize(&input(&v), &Hooks::default()).await.unwrap();
        assert_eq!(out.body.lang, Some(LangTag::vi()));
        assert_eq!(out.body.sections.len(), 1);
        let b = &out.body.sections[0].bullets;
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].cites[0].segment, 1);
        assert_eq!(b[1].text, "Tuyển 2 kỹ sư backend.");
        assert_eq!(
            b[1].cites.iter().map(|c| c.segment).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(out.report.cites.cites_invalid, 1);
        assert_eq!(out.report.dropped_cites, vec!["42"]);
        assert_eq!(out.report.llm_calls, 1);
        assert!(out.report.templates[0].starts_with("summary/vi@"));
    }

    #[tokio::test]
    async fn summary_map_reduce() {
        let v = view();
        let mut t = tasks(json!({"sections": [
            {"heading": "Tổng quan", "bullets": [{"text": "x", "cites": [0, 3]}]}
        ]}));
        t.cfg.chunk_tokens = 20; // forces several parts
        let calls = Arc::new(std::sync::Mutex::new(vec![]));
        let c2 = calls.clone();
        let hooks = Hooks {
            progress: Some(Arc::new(move |f, m: &str| {
                c2.lock().unwrap().push((f, m.to_string()))
            })),
            cancel: None,
        };
        let out = t.summarize(&input(&v), &hooks).await.unwrap();
        assert!(out.report.map_parts >= 2);
        assert!(out.report.llm_calls > out.report.map_parts);
        assert!(
            out.report
                .templates
                .iter()
                .any(|t| t.starts_with("summary_reduce/vi@"))
        );
        assert_eq!(out.body.sections[0].bullets[0].cites.len(), 2);
        assert_eq!(calls.lock().unwrap().last().unwrap().0, 1.0);
    }

    #[tokio::test]
    async fn cancel_is_honoured() {
        let v = view();
        let t = tasks(json!({}));
        let hooks = Hooks {
            progress: None,
            cancel: Some(Arc::new(AtomicBool::new(true))),
        };
        assert!(matches!(
            t.summarize(&input(&v), &hooks).await,
            Err(Error::Cancelled)
        ));
    }

    #[tokio::test]
    async fn enhance_keeps_user_text_verbatim() {
        let v = view();
        let t = tasks(json!({
            "user_cites": [{"note": 1, "cites": [1, 99]}],
            "additions": [
                {"after": 1, "style": "bullet", "level": 1, "text": "Tuyển 2 kỹ sư backend trước tháng 12", "cites": [2]},
                {"after": -1, "style": "heading", "level": 0, "text": "Tóm tắt", "cites": []},
                {"after": 17, "style": "todo", "level": 0, "text": "Lan gửi kế hoạch trước thứ Sáu", "cites": [3]},
                {"after": 0, "style": "weird", "level": 9, "text": "x", "cites": [0]}
            ]
        }));
        let notes = "# Ngân sách Q4\n- marketing 200tr ??";
        let out = t
            .enhance_notes(&input(&v), notes, &Hooks::default())
            .await
            .unwrap();
        let b = &out.body.blocks;
        let summary: Vec<(NoteOrigin, &str)> =
            b.iter().map(|x| (x.origin, x.text.as_str())).collect();
        assert_eq!(
            summary,
            vec![
                (NoteOrigin::Ai, "Tóm tắt"),
                (NoteOrigin::User, "Ngân sách Q4"),
                (NoteOrigin::Ai, "x"),
                (NoteOrigin::User, "marketing 200tr ??"),
                (NoteOrigin::Ai, "Tuyển 2 kỹ sư backend trước tháng 12"),
                (NoteOrigin::Ai, "Lan gửi kế hoạch trước thứ Sáu"),
            ]
        );
        assert_eq!(b[1].style, "heading");
        assert_eq!(b[2].style, "bullet");
        assert_eq!(b[2].level, 4);
        assert_eq!(
            b[3].cites.iter().map(|c| c.segment).collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(b[5].style, "todo");
        // All user lines appear verbatim and in order.
        let users: Vec<&str> = b
            .iter()
            .filter(|x| x.origin == NoteOrigin::User)
            .map(|x| x.text.as_str())
            .collect();
        assert_eq!(users, vec!["Ngân sách Q4", "marketing 200tr ??"]);
        assert_eq!(out.report.cites.cites_invalid, 1);
    }

    #[tokio::test]
    async fn action_items_and_title() {
        let v = view();
        let t = tasks(json!({"items": [
            {"text": "Gửi bản kế hoạch", "owner": "Lan", "due": "thứ Sáu", "cites": [3]},
            {"text": "Gửi bản kế hoạch!", "owner": "", "due": "null", "cites": [3]},
            {"text": "Tuyển kỹ sư", "owner": "null", "due": "", "cites": []}
        ]}));
        let out = t.action_items(&input(&v), &Hooks::default()).await.unwrap();
        let items = &out.body.items;
        assert_eq!(items.len(), 2, "dedupes by normalized text");
        assert_eq!(items[0].owner.as_deref(), Some("Lan"));
        assert_eq!(items[0].due.as_deref(), Some("thứ Sáu"));
        assert_eq!(items[1].owner, None);
        assert_eq!(out.report.cites.items_uncited, 1);

        let t = tasks(json!({"title": "  \"Kế hoạch ngân sách Q4.\" "}));
        let out = t.title(&input(&v), &Hooks::default()).await.unwrap();
        assert_eq!(out.body.title, "Kế hoạch ngân sách Q4");
        let t = tasks(json!({"title": ""}));
        assert!(t.title(&input(&v), &Hooks::default()).await.is_err());
    }

    #[tokio::test]
    async fn bad_json_is_an_error() {
        let v = view();
        let t = tasks(json!("not an object"));
        let e = t.title(&input(&v), &Hooks::default()).await;
        assert!(matches!(e, Err(Error::Invalid(_))));
    }
}
