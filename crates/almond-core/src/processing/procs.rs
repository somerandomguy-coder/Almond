//! Processors this module registers. They are thin wrappers that resolve per-device settings at
//! run time (the job may have been requested on another device):
//! - LLM tasks: model from job params or the `llm_summary` slot, the Ollama URL setting, and (for
//!   `enhance_notes.llm`) the user's notes as `params.user_notes` when the requester didn't pass them;
//! - `verify.all`: the judge model from the `llm_verify` slot, the meeting's linked calendar event
//!   (`params.calendar_event`) and its time zone (`meeting.tz` / `params.tz`, read by almond-verify);
//! - `verify.extract_claims`: rule-based claim extraction (no LLM; runs on every device).

use super::caps::{self, CapsInfo, SLOT_LLM_SUMMARY, SLOT_LLM_VERIFY};
use super::{CoreRef, stubs};
use crate::{Core, Shared};
use almond_llm::{LlmProcessor, LlmTaskKind, LlmTasks, Ollama, OllamaConfig, PromptLibrary};
use almond_types::artifact::body::{Claim, Verdict};
use almond_types::calendar::CalendarEvent;
use almond_types::job::Capability;
use almond_types::pipeline::{JobCtx, Processor, ProcessorDescriptor};
use almond_types::store::Store;
use almond_types::verify::{Verifier, VerifierDescriptor, VerifyContext};
use almond_types::{Artifact, ArtifactDraft, ArtifactKind, Error, Meeting, Result};
use almond_verify::{
    ClaimExtractor, ExtractClaimsProcessor, VerifierRegistry, VerifyConfig, VerifyProcessor,
};
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::sync::{Arc, OnceLock};

pub const VERIFY_ALL: &str = "verify.all";
const DAY_MS: i64 = 86_400_000;

pub(crate) fn register(shared: &Shared, core: &CoreRef) -> Result<()> {
    for kind in LlmTaskKind::ALL {
        shared
            .registry
            .register(Arc::new(CoreLlm::new(kind, core.clone())?))?;
    }
    shared
        .registry
        .register(Arc::new(ExtractClaimsProcessor::new(
            ClaimExtractor::rules(),
        )))?;
    shared
        .registry
        .register(Arc::new(CoreVerify { core: core.clone() }))?;
    Ok(())
}

fn prompts() -> PromptLibrary {
    static P: OnceLock<PromptLibrary> = OnceLock::new();
    P.get_or_init(PromptLibrary::builtin).clone()
}

fn params_object(v: &Value) -> Result<Map<String, Value>> {
    match v {
        Value::Null => Ok(Map::new()),
        Value::Object(m) => Ok(m.clone()),
        other => Err(Error::Invalid(format!(
            "job params must be an object, got {other}"
        ))),
    }
}

/// Fail early, with a message the user can act on, when the chosen model is not installed.
/// (When the model list is unknown the call itself reports the problem.)
fn ensure_model(core: &Core, info: &CapsInfo, slot: &str, model: &str) -> Result<()> {
    if !info.llm_models.is_empty() && !info.has_model(model) {
        return Err(Error::NotFound(format!(
            "model {model} ({slot}) is not installed in Ollama at {}; run `ollama pull {model}` \
             or choose another model in Settings",
            super::ollama_url(core)
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------- LLM tasks

struct CoreLlm {
    kind: LlmTaskKind,
    core: CoreRef,
    desc: ProcessorDescriptor,
}

impl CoreLlm {
    fn new(kind: LlmTaskKind, core: CoreRef) -> Result<Self> {
        // Descriptor from the real processor (the provider is irrelevant for it).
        let probe = LlmTasks::new(
            Arc::new(Ollama::new(OllamaConfig::default())?),
            prompts(),
            Default::default(),
        );
        let desc = LlmProcessor::new(kind, probe).descriptor();
        Ok(CoreLlm { kind, core, desc })
    }
}

#[async_trait]
impl Processor for CoreLlm {
    fn descriptor(&self) -> ProcessorDescriptor {
        self.desc.clone()
    }

    async fn run(&self, ctx: &JobCtx, inputs: Vec<Artifact>) -> Result<Vec<ArtifactDraft>> {
        let core = self.core.get()?;
        let st = &core.processing;
        let info = st.capabilities();
        let mut params = params_object(&ctx.params)?;
        let lang = params
            .get("lang")
            .and_then(Value::as_str)
            .filter(|l| *l != "auto")
            .map(String::from);
        let model = match params.get("model").and_then(Value::as_str) {
            Some(m) => caps::ollama_name(m).to_string(),
            None => caps::slot_model(&core, &info, SLOT_LLM_SUMMARY, lang.as_deref()),
        };
        ensure_model(&core, &info, SLOT_LLM_SUMMARY, &model)?;
        params.insert("model".into(), Value::String(model.clone()));
        if self.kind == LlmTaskKind::EnhancedNotes && !params.contains_key("user_notes") {
            let notes = stubs::notes_text(&core, &ctx.meeting.notes_doc)?;
            params.insert("user_notes".into(), Value::String(notes));
        }
        let provider = st.llm_provider(&core)?;
        let tasks = LlmTasks::new(
            provider,
            prompts(),
            almond_llm::TaskConfig {
                model,
                ..Default::default()
            },
        );
        let mut ctx = ctx.clone();
        ctx.params = Value::Object(params);
        LlmProcessor::new(self.kind, tasks).run(&ctx, inputs).await
    }
}

// ---------------------------------------------------------------------------------- verify

struct CoreVerify {
    core: CoreRef,
}

/// Stands in for `verify.nli` when its model is missing, so the failure shows up in
/// `Verdicts.errors` while the symbolic verifiers still run.
struct UnavailableVerifier {
    name: String,
    message: String,
}

#[async_trait]
impl Verifier for UnavailableVerifier {
    fn descriptor(&self) -> VerifierDescriptor {
        VerifierDescriptor {
            name: self.name.clone(),
            version: "0".into(),
            family: "neural".into(),
            logical_forms: vec![],
            description: self.message.clone(),
        }
    }
    async fn verify(&self, _: &[Claim], _: &VerifyContext) -> Result<Vec<Verdict>> {
        Err(Error::NotFound(self.message.clone()))
    }
}

/// The verifier set this device runs for `verify.all` with judge `model`.
pub(crate) fn verifiers(core: &Core, info: &CapsInfo, model: &str) -> Result<VerifierRegistry> {
    let cfg = VerifyConfig::default();
    Ok(match ensure_model(core, info, SLOT_LLM_VERIFY, model) {
        Ok(()) => VerifierRegistry::with_llm(cfg, core.processing.llm_provider(core)?, model),
        Err(e) => {
            let mut r = VerifierRegistry::symbolic(cfg);
            r.register(Arc::new(UnavailableVerifier {
                name: almond_verify::verifiers::nli::NAME.into(),
                message: e.to_string(),
            }));
            r
        }
    })
}

/// The calendar event a meeting is linked to, from the synced calendar.
pub(crate) fn linked_event(core: &Core, m: &Meeting) -> Result<Option<CalendarEvent>> {
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

#[async_trait]
impl Processor for CoreVerify {
    fn descriptor(&self) -> ProcessorDescriptor {
        ProcessorDescriptor {
            name: VERIFY_ALL.into(),
            version: "1".into(),
            consumes: vec![ArtifactKind::CLAIMS.into(), ArtifactKind::TRANSCRIPT.into()],
            produces: vec![ArtifactKind::VERDICTS.into()],
            needs_audio: false,
            requires: vec![Capability::new("llm")],
            description: "Fact-checks claims: citation, numbers, dates, entities, calendar, \
                          cross-claim consistency (Datalog) and an LLM judge (verify.nli); one \
                          aggregated label per claim."
                .into(),
        }
    }

    async fn run(&self, ctx: &JobCtx, inputs: Vec<Artifact>) -> Result<Vec<ArtifactDraft>> {
        let core = self.core.get()?;
        let info = core.processing.capabilities();
        let mut params = params_object(&ctx.params)?;
        if !params.contains_key("calendar_event")
            && let Some(ev) = linked_event(&core, &ctx.meeting)?
        {
            params.insert("calendar_event".into(), serde_json::to_value(ev)?);
        }
        let model = match params.get("model").and_then(Value::as_str) {
            Some(m) => caps::ollama_name(m).to_string(),
            None => caps::slot_model(&core, &info, SLOT_LLM_VERIFY, None),
        };
        let reg = verifiers(&core, &info, &model)?;
        let mut ctx = ctx.clone();
        ctx.params = Value::Object(params);
        let mut p = VerifyProcessor::all(reg);
        p.name = VERIFY_ALL.into();
        p.run(&ctx, inputs).await
    }
}
