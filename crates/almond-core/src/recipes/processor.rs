//! Processor `recipe.llm`: runs one recipe on one meeting and produces a `recipe_output`.
//!
//! Params: [`RecipeRunParams`] (the requesting core snapshots the recipe into `recipe`, so a
//! device that hasn't synced the recipe record yet can still run it). Inputs: the meeting's
//! transcript (if the job has none, the latest live one from the store is used).
//!
//! The prompt gets, besides the recipe's instructions: the meeting's title and date, calendar
//! attendees, the user's own notes, the summary (if one exists) and conversation statistics
//! computed in code ([`super::stats`]). Transcripts over [`TRANSCRIPT_BUDGET`] tokens are
//! map-reduced: each part is condensed into cited notes for the recipe, then the final answer is
//! written from those notes (indices stay global, so citations survive).
//!
//! The answer is free Markdown; its `[index]` citations are validated and renumbered by
//! [`super::cites::renumber`].
//!
//! Superseding: the runner makes a new artifact supersede the live one of the same kind *and*
//! `provenance.processor`. Every recipe produces the same kind, so outputs carry
//! `provenance.processor = "recipe.llm/<recipe id>"`: a rerun of a recipe supersedes only that
//! recipe's previous output (docs/contract-requests/f-recipes.md).

use super::{builtins, cites, stats};
use crate::Core;
use crate::processing::caps::{self, SLOT_LLM_SUMMARY};
use almond_llm::cite::CiteStats;
use almond_llm::ollama::model_ref;
use almond_llm::prompts::{Template, language_name, part_note};
use almond_llm::tasks::iso_date;
use almond_llm::tokens::estimate_tokens;
use almond_llm::transcript::resolve_output_lang;
use almond_llm::{TranscriptView, processors::pick_transcript};
use almond_pipeline::planner::latest_live;
use almond_types::artifact::body::{RecipeOutput, Summary};
use almond_types::artifact::params_hash;
use almond_types::job::Capability;
use almond_types::llm::{ChatMessage, ChatRequest, LlmModelInfo, LlmProvider, Role};
use almond_types::pipeline::{JobCtx, Processor, ProcessorDescriptor};
use almond_types::recipes::{Recipe, RecipeRunParams};
use almond_types::store::Store;
use almond_types::{
    Artifact, ArtifactDraft, ArtifactKind, Error, LangTag, Meeting, Provenance, Result,
};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Instant;

pub const RECIPE_LLM: &str = "recipe.llm";

/// Transcript tokens per LLM call; longer meetings are map-reduced. Leaves room for the context
/// block and the answer within the 32k `num_ctx` the Ollama provider allows by default.
pub const TRANSCRIPT_BUDGET: usize = 14_000;
/// The summary is skipped from the context when it would be longer than this.
const SUMMARY_BUDGET: usize = 2_500;
/// The user's notes are cut to this many tokens.
const NOTES_BUDGET: usize = 1_500;

const TEMPLATE: &str = include_str!("prompts/recipe.txt");
const MAP_TEMPLATE: &str = include_str!("prompts/recipe_map.txt");

fn templates() -> &'static (Template, Template) {
    static T: OnceLock<(Template, Template)> = OnceLock::new();
    T.get_or_init(|| {
        (
            Template::parse("recipe", "en", TEMPLATE).expect("recipe.txt"),
            Template::parse("recipe_map", "en", MAP_TEMPLATE).expect("recipe_map.txt"),
        )
    })
}

/// `provenance.processor` of a recipe's outputs (see the module docs).
pub fn output_processor(recipe_id: &str) -> String {
    format!("{RECIPE_LLM}/{recipe_id}")
}

/// Late-bound core handle (the processor is registered before the `Core` exists).
pub(crate) type CoreSlot = Arc<OnceLock<Weak<Core>>>;

pub(crate) struct RecipeProcessor {
    pub(crate) core: CoreSlot,
}

pub fn descriptor() -> ProcessorDescriptor {
    ProcessorDescriptor {
        name: RECIPE_LLM.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        // Takes the transcript as its input, but declares no `consumes`: the planner would
        // otherwise request a (recipe-less) job for every meeting with a transcript. Recipe jobs
        // are only requested by `recipes.run` and auto-run (docs/contract-requests/f-recipes.md).
        consumes: vec![],
        produces: vec![ArtifactKind::RECIPE_OUTPUT.into()],
        needs_audio: false,
        requires: vec![Capability::new("llm")],
        description: "Runs a cookbook recipe (a reusable prompt) on a meeting's transcript; \
                      Markdown answer with transcript citations."
            .into(),
    }
}

/// A recipe by id: built-in (from code) or a user record.
pub(crate) fn find_recipe(core: &Core, id: &str) -> Result<Option<Recipe>> {
    if builtins::is_builtin_id(id) {
        return Ok(builtins::get(id));
    }
    Ok(core
        .record::<super::StoredRecipe>(almond_types::recipes::COLLECTION, id)?
        .map(|s| s.recipe))
}

#[async_trait]
impl Processor for RecipeProcessor {
    fn descriptor(&self) -> ProcessorDescriptor {
        descriptor()
    }

    async fn run(&self, ctx: &JobCtx, inputs: Vec<Artifact>) -> Result<Vec<ArtifactDraft>> {
        ctx.check_cancel()?;
        let core = self
            .core
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| Error::Other("core is not running".into()))?;
        let p: RecipeRunParams = serde_json::from_value(ctx.params.clone())
            .map_err(|e| Error::Invalid(format!("bad params for {RECIPE_LLM}: {e}")))?;
        let recipe = match &p.recipe {
            Some(r) => r.clone(),
            None => find_recipe(&core, &p.recipe_id)?
                .ok_or_else(|| Error::NotFound(format!("recipe {}", p.recipe_id)))?,
        };
        let transcript = match pick_transcript(&inputs) {
            Ok(t) => t.clone(),
            Err(_) => {
                let arts = core
                    .store()
                    .list_artifacts(&ctx.meeting.id, Some(ArtifactKind::TRANSCRIPT))?;
                latest_live(&arts, ArtifactKind::TRANSCRIPT)
                    .cloned()
                    .ok_or_else(|| {
                        Error::NotFound("this meeting has no transcript yet".to_string())
                    })?
            }
        };
        let view = TranscriptView::from_artifact(&transcript)?;
        if view.segments().iter().all(|s| s.text.trim().is_empty()) {
            return Err(Error::Invalid(
                "no speech was detected in this recording".into(),
            ));
        }
        let lang = resolve_output_lang(p.lang.as_deref(), &view, &ctx.meeting.languages);

        let st = &core.processing;
        let info = st.capabilities();
        let model = caps::slot_model(&core, &info, SLOT_LLM_SUMMARY, Some(&lang.primary()));
        if !info.llm_models.is_empty() && !info.has_model(&model) {
            return Err(Error::NotFound(format!(
                "model {model} ({SLOT_LLM_SUMMARY}) is not installed in Ollama at {}; run \
                 `ollama pull {model}` or choose another model in Settings",
                crate::processing::ollama_url(&core)
            )));
        }
        let llm = st.llm_provider(&core)?;
        let context = build_context(&core, &ctx.meeting, &view)?;
        let run = Run {
            llm,
            model: model.clone(),
            ctx,
            recipe: &recipe,
            view: &view,
            lang: &lang,
            meeting: &ctx.meeting,
        };
        let t0 = Instant::now();
        let (answer, info_model, calls) = run.answer(&context, p.extra.as_deref()).await?;
        let (mut markdown, citations, cstats) =
            cites::renumber(&cites::clean_answer(&answer), &view);
        if markdown.trim().is_empty() {
            return Err(Error::Invalid(format!(
                "model {model} returned an empty answer"
            )));
        }
        if recipe.id == builtins::COMMUNICATION {
            // The numbers reach the user straight from the code, not through the model.
            let table = stats::render_section(&stats::compute(&view), &lang.primary());
            if !table.is_empty() {
                markdown = format!("{table}\n\n{markdown}");
            }
        }
        log_stats(&recipe, &cstats, calls, t0);

        let provider = run.llm.name().to_string();
        let (tpl, map_tpl) = templates();
        let hash = params_hash(
            &format!("{}+{}", tpl.id(), map_tpl.id()),
            &json!({
                "recipe": { "id": recipe.id, "prompt": recipe.prompt, "name": recipe.name },
                "extra": p.extra,
                "lang": lang.0,
                "model": model,
            }),
        );
        let mut draft = ArtifactDraft::new(
            ctx.meeting.id.clone(),
            &RecipeOutput {
                recipe_id: recipe.id.clone(),
                recipe_name: recipe.name.clone(),
                markdown,
                citations,
            },
            Provenance {
                processor: output_processor(&recipe.id),
                processor_version: env!("CARGO_PKG_VERSION").into(),
                model: info_model.as_ref().map(|m| model_ref(&provider, m)),
                params_hash: Some(hash),
                inputs: vec![transcript.id.clone()],
            },
        )?;
        // Explicitly supersede this recipe's live output (the runner would find the same one by
        // `provenance.processor`; outputs from older versions may lack the suffix).
        draft.supersedes = previous_output(&core, &ctx.meeting.id, &recipe.id)?;
        Ok(vec![draft])
    }
}

/// The live `recipe_output` of `recipe_id` for a meeting, if any.
pub(crate) fn previous_output(
    core: &Core,
    meeting: &almond_types::Id,
    recipe_id: &str,
) -> Result<Option<almond_types::Id>> {
    let arts = core
        .store()
        .list_artifacts(meeting, Some(ArtifactKind::RECIPE_OUTPUT))?;
    Ok(super::live_outputs(&arts)
        .into_iter()
        .find(|a| {
            a.decode::<RecipeOutput>()
                .is_ok_and(|o| o.recipe_id == recipe_id)
        })
        .map(|a| a.id.clone()))
}

fn log_stats(recipe: &Recipe, c: &CiteStats, calls: u32, t0: Instant) {
    tracing::info!(
        "recipe {} done in {:.1}s ({calls} LLM call(s)): {} citation group(s), {}/{} cites valid",
        recipe.id,
        t0.elapsed().as_secs_f32(),
        c.items_total,
        c.cites_valid,
        c.cites_total
    );
}

/// Attendees, the user's notes, the summary and the statistics, as one prompt block.
fn build_context(core: &Core, m: &Meeting, view: &TranscriptView) -> Result<String> {
    let mut parts: Vec<String> = vec![];
    if let Some(ev) = linked_event(core, m)? {
        let names: Vec<String> = ev
            .attendees
            .iter()
            .filter_map(|a| {
                let n = a.name.clone().or_else(|| a.email.clone())?;
                Some(if a.is_self { format!("{n} (me)") } else { n })
            })
            .collect();
        if !names.is_empty() {
            parts.push(format!(
                "Calendar event \"{}\", attendees: {}.",
                ev.title,
                names.join(", ")
            ));
        }
    }
    let notes = crate::library::notes_text(core, &m.notes_doc).unwrap_or_default();
    let notes = notes.trim();
    if !notes.is_empty() {
        parts.push(format!(
            "The user's own notes from the meeting:\n<notes>\n{}\n</notes>",
            truncate_tokens(notes, NOTES_BUDGET)
        ));
    }
    let arts = core
        .store()
        .list_artifacts(&m.id, Some(ArtifactKind::SUMMARY))?;
    if let Some(s) = latest_live(&arts, ArtifactKind::SUMMARY)
        && let Ok(sum) = s.decode::<Summary>()
    {
        let txt = render_summary(&sum);
        if !txt.is_empty() && estimate_tokens(&txt) <= SUMMARY_BUDGET {
            parts.push(format!(
                "Summary written earlier (its [index] numbers are transcript lines):\n<summary>\n{txt}</summary>"
            ));
        }
    }
    let st = stats::render(&stats::compute(view));
    if !st.is_empty() {
        parts.push(st);
    }
    Ok(parts.join("\n\n"))
}

fn render_summary(s: &Summary) -> String {
    let mut out = String::new();
    for sec in &s.sections {
        if !sec.heading.trim().is_empty() {
            out.push_str(&format!("{}:\n", sec.heading.trim()));
        }
        for b in &sec.bullets {
            let c: String = b.cites.iter().map(|c| format!("[{}]", c.segment)).collect();
            out.push_str(&format!("- {} {c}\n", b.text.trim()));
        }
    }
    out
}

fn truncate_tokens(s: &str, budget: usize) -> String {
    if estimate_tokens(s) <= budget {
        return s.to_string();
    }
    let mut out = String::new();
    for line in s.lines() {
        if estimate_tokens(&out) + estimate_tokens(line) > budget {
            out.push_str("…\n");
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The calendar event a meeting is linked to (same lookup as the processing module's).
fn linked_event(core: &Core, m: &Meeting) -> Result<Option<almond_types::calendar::CalendarEvent>> {
    const DAY_MS: i64 = 86_400_000;
    let Some(r) = &m.calendar_event else {
        return Ok(None);
    };
    for days in [2, 120] {
        let evs = core
            .store()
            .calendar_events(m.started_at - days * DAY_MS, m.started_at + days * DAY_MS)?;
        if let Some(e) = evs
            .into_iter()
            .find(|e| e.source == r.source && e.event_id == r.event_id)
        {
            return Ok(Some(e));
        }
    }
    Ok(None)
}

struct Run<'a> {
    llm: Arc<dyn LlmProvider>,
    model: String,
    ctx: &'a JobCtx,
    recipe: &'a Recipe,
    view: &'a TranscriptView,
    lang: &'a LangTag,
    meeting: &'a Meeting,
}

impl Run<'_> {
    async fn chat(&self, tpl: &Template, vars: &[(&str, &str)]) -> Result<(String, LlmModelInfo)> {
        let (system, user) = tpl.render(vars)?;
        let resp = self
            .llm
            .chat(ChatRequest {
                model: self.model.clone(),
                messages: vec![
                    ChatMessage {
                        role: Role::System,
                        content: system,
                    },
                    ChatMessage {
                        role: Role::User,
                        content: user,
                    },
                ],
                json_schema: None,
                temperature: Some(0.3),
                num_ctx: None,
                think: Some(false),
            })
            .await?;
        Ok((resp.content, resp.model))
    }

    /// Final answer (map-reducing long transcripts). Returns (answer, model, LLM calls).
    async fn answer(
        &self,
        context: &str,
        extra: Option<&str>,
    ) -> Result<(String, Option<LlmModelInfo>, u32)> {
        let (tpl, map_tpl) = templates();
        let lang_name = language_name("en", self.lang);
        let title = if self.meeting.title.trim().is_empty() {
            "-".to_string()
        } else {
            self.meeting.title.clone()
        };
        let mut calls = 0;
        let chunks = self.view.chunks(TRANSCRIPT_BUDGET);
        let material = if chunks.len() <= 1 {
            format!(
                "Transcript (each line: [index] time speaker: text):\n<transcript>\n{}</transcript>",
                self.view.render_all()
            )
        } else {
            let n = chunks.len();
            let mut notes = String::new();
            for (i, chunk) in chunks.iter().enumerate() {
                self.ctx.check_cancel()?;
                (self.ctx.progress)(
                    i as f32 / (n as f32 + 1.0),
                    &format!("reading part {}/{n}", i + 1),
                );
                let text = self.view.render(chunk);
                let note = part_note("en", i + 1, n)
                    .replace("Summarize only this part.", "Use only this part.");
                let (out, _) = self
                    .chat(
                        map_tpl,
                        &[
                            ("output_language", &lang_name),
                            ("meeting_title", &title),
                            ("part_note", &note),
                            ("transcript", &text),
                            ("recipe_name", &self.recipe.name),
                            ("recipe_prompt", &self.recipe.prompt),
                        ],
                    )
                    .await?;
                calls += 1;
                let (from, to) = (
                    chunk.first().map_or(0, |s| s.t0_ms),
                    chunk.last().map_or(0, |s| s.t1_ms),
                );
                notes.push_str(&format!(
                    "## Part {}/{n} ({}-{})\n{}\n\n",
                    i + 1,
                    almond_llm::transcript::fmt_ms(from),
                    almond_llm::transcript::fmt_ms(to),
                    cites::clean_answer(&out)
                ));
            }
            format!(
                "The transcript is long, so it was read in {n} parts. Notes from each part (the \
                 [index] numbers are transcript lines; cite them):\n<notes>\n{notes}</notes>"
            )
        };
        self.ctx.check_cancel()?;
        (self.ctx.progress)(
            if chunks.len() <= 1 {
                0.1
            } else {
                chunks.len() as f32 / (chunks.len() as f32 + 1.0)
            },
            "writing the answer",
        );
        let extra = match extra.map(str::trim).filter(|e| !e.is_empty()) {
            Some(e) => format!("Additional instruction for this run: {e}\n"),
            None => String::new(),
        };
        let date = iso_date(self.meeting.started_at);
        let (out, m) = self
            .chat(
                tpl,
                &[
                    ("output_language", &lang_name),
                    ("meeting_title", &title),
                    ("meeting_date", &date),
                    ("context", context),
                    ("material", &material),
                    ("recipe_name", &self.recipe.name),
                    ("recipe_prompt", &self.recipe.prompt),
                    ("extra", &extra),
                ],
            )
            .await?;
        calls += 1;
        (self.ctx.progress)(1.0, "done");
        Ok((out, Some(m), calls))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_parse_and_render() {
        let (t, m) = templates();
        let (sys, user) = t
            .render(&[
                ("output_language", "English"),
                ("meeting_title", "T"),
                ("meeting_date", "2026-11-12"),
                ("context", "CTX"),
                ("material", "MAT"),
                ("recipe_name", "R"),
                ("recipe_prompt", "P"),
                ("extra", ""),
            ])
            .unwrap();
        assert!(sys.contains("NĂM MƯƠI HAI = 52"));
        assert!(user.contains("CTX") && user.contains("MAT") && user.contains("<recipe>\nP\n"));
        m.render(&[
            ("output_language", "English"),
            ("meeting_title", "T"),
            ("part_note", ""),
            ("transcript", "x"),
            ("recipe_name", "R"),
            ("recipe_prompt", "P"),
        ])
        .unwrap();
        assert_eq!(output_processor("builtin:x"), "recipe.llm/builtin:x");
    }
}
