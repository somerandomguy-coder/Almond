//! Pipeline [`Processor`]s backed by an [`LlmProvider`]: `summarize.llm`, `enhance_notes.llm`,
//! `action_items.llm`, `title.llm`. Each consumes the meeting's `transcript` artifact.
//!
//! Job params (all optional):
//! ```json
//! { "lang": "vi" | "en" | "auto",       // output language (default: dominant transcript language)
//!   "model": "gemma4:e4b",              // override the configured model
//!   "template_lang": "en",              // prompt template language override
//!   "temperature": 0.2,
//!   "user_notes": "# ...\n- ..." }      // enhance_notes only: plain text of the user's notes doc
//! ```
//! `user_notes` is passed in by the requester (the core reads the Loro notes doc), so the processor
//! stays independent of the CRDT library.

use crate::ollama::model_ref;
use crate::tasks::{Hooks, LlmTasks, TaskInput, TaskReport, iso_date};
use crate::transcript::{TranscriptView, resolve_output_lang};
use almond_types::artifact::ArtifactBody;
use almond_types::artifact::body::{ActionItems, EnhancedNotes, Summary, Title};
use almond_types::job::Capability;
use almond_types::pipeline::{JobCtx, Processor, ProcessorDescriptor};
use almond_types::{Artifact, ArtifactDraft, ArtifactKind, Error, LangTag, Provenance, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub const SUMMARIZE: &str = "summarize.llm";
pub const ENHANCE_NOTES: &str = "enhance_notes.llm";
pub const ACTION_ITEMS: &str = "action_items.llm";
pub const TITLE: &str = "title.llm";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmTaskKind {
    Summary,
    EnhancedNotes,
    ActionItems,
    Title,
}

impl LlmTaskKind {
    pub const ALL: [LlmTaskKind; 4] = [
        LlmTaskKind::Summary,
        LlmTaskKind::EnhancedNotes,
        LlmTaskKind::ActionItems,
        LlmTaskKind::Title,
    ];

    pub fn processor_name(self) -> &'static str {
        match self {
            LlmTaskKind::Summary => SUMMARIZE,
            LlmTaskKind::EnhancedNotes => ENHANCE_NOTES,
            LlmTaskKind::ActionItems => ACTION_ITEMS,
            LlmTaskKind::Title => TITLE,
        }
    }

    pub fn produces(self) -> &'static str {
        match self {
            LlmTaskKind::Summary => Summary::KIND,
            LlmTaskKind::EnhancedNotes => EnhancedNotes::KIND,
            LlmTaskKind::ActionItems => ActionItems::KIND,
            LlmTaskKind::Title => Title::KIND,
        }
    }

    fn description(self) -> &'static str {
        match self {
            LlmTaskKind::Summary => {
                "Sectioned meeting summary; every bullet cites transcript segments (map-reduce for long meetings)"
            }
            LlmTaskKind::EnhancedNotes => "User notes kept verbatim + AI additions with citations",
            LlmTaskKind::ActionItems => "Action items with owner, due date and citations",
            LlmTaskKind::Title => "Short meeting title",
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JobParams {
    lang: Option<String>,
    model: Option<String>,
    template_lang: Option<String>,
    temperature: Option<f32>,
    user_notes: Option<String>,
}

/// One LLM task exposed as a pipeline processor.
pub struct LlmProcessor {
    kind: LlmTaskKind,
    tasks: LlmTasks,
}

impl LlmProcessor {
    pub fn new(kind: LlmTaskKind, tasks: LlmTasks) -> Self {
        LlmProcessor { kind, tasks }
    }

    /// All four LLM processors sharing one provider/config.
    pub fn all(tasks: LlmTasks) -> Vec<Arc<dyn Processor>> {
        LlmTaskKind::ALL
            .iter()
            .map(|k| Arc::new(LlmProcessor::new(*k, tasks.clone())) as Arc<dyn Processor>)
            .collect()
    }
}

/// Latest (by HLC) transcript among `inputs`.
pub fn pick_transcript(inputs: &[Artifact]) -> Result<&Artifact> {
    inputs
        .iter()
        .filter(|a| a.kind.is(ArtifactKind::TRANSCRIPT))
        .max_by_key(|a| a.created_at)
        .ok_or_else(|| Error::Invalid("no transcript artifact among inputs".into()))
}

fn provenance(
    name: &str,
    provider: &str,
    report: &TaskReport,
    hash_input: serde_json::Value,
    transcript: &Artifact,
) -> Provenance {
    Provenance {
        processor: name.into(),
        processor_version: env!("CARGO_PKG_VERSION").into(),
        model: report.model.as_ref().map(|m| model_ref(provider, m)),
        params_hash: Some(
            blake3::hash(hash_input.to_string().as_bytes())
                .to_hex()
                .to_string(),
        ),
        inputs: vec![transcript.id.clone()],
    }
}

#[async_trait]
impl Processor for LlmProcessor {
    fn descriptor(&self) -> ProcessorDescriptor {
        ProcessorDescriptor {
            name: self.kind.processor_name().into(),
            version: env!("CARGO_PKG_VERSION").into(),
            consumes: vec![ArtifactKind::TRANSCRIPT.into()],
            produces: vec![self.kind.produces().into()],
            needs_audio: false,
            requires: vec![Capability::new("llm")],
            description: self.kind.description().into(),
        }
    }

    async fn run(&self, ctx: &JobCtx, inputs: Vec<Artifact>) -> Result<Vec<ArtifactDraft>> {
        ctx.check_cancel()?;
        let transcript = pick_transcript(&inputs)?;
        let view = TranscriptView::from_artifact(transcript)?;
        let p: JobParams = if ctx.params.is_null() {
            JobParams::default()
        } else {
            serde_json::from_value(ctx.params.clone()).map_err(|e| {
                Error::Invalid(format!(
                    "bad params for {}: {e}",
                    self.kind.processor_name()
                ))
            })?
        };
        let mut tasks = self.tasks.clone();
        {
            let cfg = tasks.config_mut();
            if let Some(m) = &p.model {
                cfg.model = m.clone();
            }
            if let Some(t) = &p.template_lang {
                cfg.template_lang = Some(LangTag(t.clone()));
            }
            if p.temperature.is_some() {
                cfg.temperature = p.temperature;
            }
        }
        let output_lang = resolve_output_lang(p.lang.as_deref(), &view, &ctx.meeting.languages);
        let input = TaskInput {
            transcript: &view,
            meeting_title: ctx.meeting.title.clone(),
            meeting_date: Some(iso_date(ctx.meeting.started_at)),
            output_lang: output_lang.clone(),
        };
        let hooks = Hooks {
            progress: Some(ctx.progress.clone()),
            cancel: Some(ctx.cancel.clone()),
        };
        let provider = tasks.provider().name().to_string();
        let cfg = tasks.config().clone();
        let mut hash_input = json!({
            "processor": self.kind.processor_name(),
            "model": cfg.model,
            "temperature": cfg.temperature,
            "think": cfg.think,
            "chunk_tokens": cfg.chunk_tokens,
            "output_lang": output_lang.0,
        });
        let name = self.kind.processor_name();
        let draft = match self.kind {
            LlmTaskKind::Summary => {
                let o = tasks.summarize(&input, &hooks).await?;
                hash_input["templates"] = json!(o.report.templates);
                ArtifactDraft::new(
                    ctx.meeting.id.clone(),
                    &o.body,
                    provenance(name, &provider, &o.report, hash_input, transcript),
                )?
            }
            LlmTaskKind::EnhancedNotes => {
                let notes = p.user_notes.clone().unwrap_or_default();
                let o = tasks.enhance_notes(&input, &notes, &hooks).await?;
                hash_input["templates"] = json!(o.report.templates);
                hash_input["user_notes"] =
                    json!(blake3::hash(notes.as_bytes()).to_hex().to_string());
                ArtifactDraft::new(
                    ctx.meeting.id.clone(),
                    &o.body,
                    provenance(name, &provider, &o.report, hash_input, transcript),
                )?
            }
            LlmTaskKind::ActionItems => {
                let o = tasks.action_items(&input, &hooks).await?;
                hash_input["templates"] = json!(o.report.templates);
                ArtifactDraft::new(
                    ctx.meeting.id.clone(),
                    &o.body,
                    provenance(name, &provider, &o.report, hash_input, transcript),
                )?
            }
            LlmTaskKind::Title => {
                let o = tasks.title(&input, &hooks).await?;
                hash_input["templates"] = json!(o.report.templates);
                ArtifactDraft::new(
                    ctx.meeting.id.clone(),
                    &o.body,
                    provenance(name, &provider, &o.report, hash_input, transcript),
                )?
            }
        };
        Ok(vec![draft])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::PromptLibrary;
    use crate::tasks::TaskConfig;
    use almond_types::artifact::body::Summary;
    use almond_types::ids::DeviceId;
    use almond_types::mock::{MemBlobStore, MockLlm, fixtures};
    use std::sync::atomic::AtomicBool;

    fn ctx(params: serde_json::Value) -> JobCtx {
        JobCtx {
            job_id: almond_types::Id::new(),
            meeting: fixtures::meeting(1_762_646_400_000),
            params,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(|_, _| {}),
            blobs: Arc::new(MemBlobStore::default()),
        }
    }

    #[tokio::test]
    async fn summarize_processor_builds_draft_with_provenance() {
        let llm = Arc::new(MockLlm {
            reply: String::new(),
            json_reply: json!({"sections": [{"heading": "Quyết định", "bullets": [
                {"text": "Ngân sách marketing 200 triệu đồng.", "cites": [1]}
            ]}]}),
        });
        let tasks = LlmTasks::new(llm, PromptLibrary::builtin(), TaskConfig::default());
        let procs = LlmProcessor::all(tasks);
        assert_eq!(procs.len(), 4);
        let p = procs
            .iter()
            .find(|p| p.descriptor().name == SUMMARIZE)
            .unwrap();
        let c = ctx(json!({"lang": "en"}));
        let t = fixtures::transcript(&c.meeting.id, &DeviceId("d".into()));
        let out = p.run(&c, vec![t.clone()]).await.unwrap();
        assert_eq!(out.len(), 1);
        let d = &out[0];
        assert!(d.kind.is("summary"));
        assert_eq!(d.meeting_id, c.meeting.id);
        assert_eq!(d.provenance.processor, SUMMARIZE);
        assert_eq!(d.provenance.model.as_deref(), Some("mock:mock:latest@mock"));
        assert_eq!(d.provenance.inputs, vec![t.id.clone()]);
        assert!(d.provenance.params_hash.is_some());
        let s: Summary = serde_json::from_value(d.body.clone()).unwrap();
        assert_eq!(s.lang, Some(LangTag::en()));
        assert_eq!(s.sections[0].bullets[0].cites[0].artifact, t.id);

        // Different params -> different params_hash.
        let out2 = p
            .run(&ctx(json!({"lang": "vi"})), vec![t.clone()])
            .await
            .unwrap();
        assert_ne!(out2[0].provenance.params_hash, d.provenance.params_hash);

        // No transcript -> error; bad params -> error.
        assert!(p.run(&c, vec![]).await.is_err());
        assert!(p.run(&ctx(json!({"lang": 5})), vec![t]).await.is_err());
    }
}
