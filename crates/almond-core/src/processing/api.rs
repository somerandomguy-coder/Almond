//! Methods: `artifacts.*`, `jobs.*`, `pipeline.*`, `verify.*`, `actions.*`, `models.*`
//! (docs/API.md).

use super::caps::{self, SLOT_LLM_VERIFY, is_embed_model, ollama_id, ollama_name};
use super::{procs, stubs};
use crate::Core;
use crate::error::{ApiError, ApiResult};
use crate::router::{IdParams, NoParams, Router};
use almond_llm::processors::ENHANCE_NOTES;
use almond_pipeline::JobRequest;
use almond_pipeline::planner::{latest_live, live_artifacts};
use almond_types::actions::{ActionDescriptor, ActionProposal, ActionProposals, ActionResult};
use almond_types::api::{JobProgress, ModelEntry, ModelSlot, RerunParams, notifications};
use almond_types::artifact::ArtifactBody;
use almond_types::event::EventBody;
use almond_types::job::{Job, JobStatus};
use almond_types::pipeline::{Processor, ProcessorDescriptor};
use almond_types::store::Store;
use almond_types::verify::VerifierDescriptor;
use almond_types::{Artifact, ArtifactDraft, ArtifactKind, Error, Id, Provenance};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn register(r: &mut Router) {
    r.add(
        "artifacts.list",
        |core, p: ArtifactsListParams| async move {
            ApiResult::Ok(
                core.store()
                    .list_artifacts(&p.meeting_id, p.kind.as_deref())?,
            )
        },
    );
    r.add("artifacts.get", |core, p: IdParams| async move {
        core.store()
            .get_artifact(&p.id)?
            .ok_or_else(|| Error::NotFound(format!("artifact {}", p.id)).into())
    });
    r.add("artifacts.rerun", rerun);
    r.add("jobs.list", jobs_list);
    r.add("jobs.cancel", |core, p: IdParams| async move {
        core.processing.runner.cancel(&p.id)?;
        core.processing.kick();
        ApiResult::Ok(Value::Null)
    });
    r.add("pipeline.processors", |core, _: NoParams| async move {
        ApiResult::Ok(core.registry.descriptors())
    });
    r.add("pipeline.registerExternal", register_external);
    r.add("verify.run", verify_run);
    r.add("verify.verifiers", |core, _: NoParams| async move {
        let info = core.processing.capabilities();
        let model = caps::slot_model(&core, &info, SLOT_LLM_VERIFY, None);
        let v: Vec<VerifierDescriptor> = procs::verifiers(&core, &info, &model)?.descriptors();
        ApiResult::Ok(v)
    });
    r.add("actions.list", |core, _: NoParams| async move {
        let v: Vec<ActionDescriptor> = core
            .processing
            .actions
            .list
            .iter()
            .map(|a| a.descriptor())
            .collect();
        ApiResult::Ok(v)
    });
    r.add("actions.propose", actions_propose);
    r.add("actions.execute", actions_execute);
    r.add("models.list", models_list);
    r.add("models.download", models_download);
    r.add("models.select", |core, slot: ModelSlot| async move {
        let info = core.processing.refresh_capabilities(&core).await;
        caps::select(&core, &info, slot)?;
        ApiResult::Ok(Value::Null)
    });
    r.add("models.slots", |core, _: NoParams| async move {
        let info = core.processing.capabilities();
        ApiResult::Ok(caps::effective_slots(&core, &info))
    });
}

#[derive(Deserialize)]
struct ArtifactsListParams {
    meeting_id: Id,
    kind: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JobsListParams {
    status: Option<JobStatus>,
}

#[derive(Deserialize)]
struct VerifyRunParams {
    artifact_id: Id,
    verifiers: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct MeetingParams {
    meeting_id: Id,
}

#[derive(Deserialize)]
struct ExecuteParams {
    proposal: ActionProposal,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ExternalParams {
    command: Option<String>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct ModelIdParams {
    id: String,
}

async fn rerun(core: Arc<Core>, mut p: RerunParams) -> ApiResult<Job> {
    if p.processor == ENHANCE_NOTES {
        let mut params = match p.params.take() {
            Value::Null => serde_json::Map::new(),
            Value::Object(m) => m,
            other => {
                return Err(ApiError::invalid(format!(
                    "params must be an object: {other}"
                )));
            }
        };
        if !params.contains_key("user_notes") {
            let m = core
                .store()
                .get_meeting(&p.meeting_id)?
                .ok_or_else(|| Error::NotFound(format!("meeting {}", p.meeting_id)))?;
            params.insert(
                "user_notes".into(),
                Value::String(stubs::notes_text(&core, &m.notes_doc)?),
            );
        }
        p.params = Value::Object(params);
    }
    let job = core.processing.runner.rerun(p)?;
    core.processing.kick();
    Ok(job)
}

async fn jobs_list(core: Arc<Core>, p: JobsListParams) -> ApiResult<Vec<JobProgress>> {
    let mut v = core.processing.runner.list(p.status)?;
    let live = core.processing.progress.lock().unwrap().clone();
    for j in &mut v {
        if let Some(lp) = live.get(&j.job.id)
            && j.status == JobStatus::Claimed
        {
            j.progress = lp.progress;
            j.message = lp.message.clone();
        }
    }
    // Newest first.
    v.sort_by(|a, b| b.job.requested_at.cmp(&a.job.requested_at));
    Ok(v)
}

async fn register_external(
    core: Arc<Core>,
    p: ExternalParams,
) -> ApiResult<Vec<ProcessorDescriptor>> {
    let procs = match (p.command, p.url) {
        (Some(cmd), None) => {
            let mut parts = cmd.split_whitespace().map(String::from);
            let prog = parts
                .next()
                .ok_or_else(|| ApiError::invalid("command is empty"))?;
            let args: Vec<String> = parts.collect();
            almond_pipeline::external::spawn_stdio(&prog, &args).await?
        }
        (None, Some(url)) => almond_pipeline::external::connect_http(&url).await?,
        _ => return Err(ApiError::invalid("pass exactly one of `command` or `url`")),
    };
    let mut out = vec![];
    for x in procs {
        out.push(x.descriptor());
        core.registry.register(x as Arc<dyn Processor>)?;
    }
    core.processing.kick();
    Ok(out)
}

/// `verify.run`: on a `claims` artifact, (re)run `verify.all` (optionally a subset of
/// verifiers); on any other artifact (summary, enhanced notes, action items), use the live claims
/// extracted from it, or extract claims first (verification then follows automatically).
async fn verify_run(core: Arc<Core>, p: VerifyRunParams) -> ApiResult<Job> {
    let a = core
        .store()
        .get_artifact(&p.artifact_id)?
        .ok_or_else(|| Error::NotFound(format!("artifact {}", p.artifact_id)))?;
    let arts = core.store().list_artifacts(&a.meeting_id, None)?;
    let claims = if a.kind.is(ArtifactKind::CLAIMS) {
        Some(a.clone())
    } else {
        live_artifacts(&arts)
            .into_iter()
            .find(|c| c.kind.is(ArtifactKind::CLAIMS) && c.provenance.inputs.contains(&a.id))
            .cloned()
    };
    let runner = &core.processing.runner;
    let job = match claims {
        Some(c) => {
            let transcript = latest_live(&arts, ArtifactKind::TRANSCRIPT)
                .ok_or_else(|| Error::NotFound("no transcript to verify against".into()))?;
            let params = match p.verifiers {
                Some(v) => json!({ "verifiers": v }),
                None => Value::Null,
            };
            runner.request(JobRequest {
                meeting_id: a.meeting_id.clone(),
                processor: procs::VERIFY_ALL.into(),
                inputs: vec![c.id, transcript.id.clone()],
                params,
                priority: 10,
                requires: None,
            })?
        }
        None => runner.request(JobRequest {
            meeting_id: a.meeting_id.clone(),
            processor: almond_verify::extract::NAME.into(),
            inputs: vec![a.id.clone()],
            params: Value::Null,
            priority: 10,
            requires: None,
        })?,
    };
    core.processing.kick();
    Ok(job)
}

/// Append an artifact authored by this module (not by a job).
fn add_artifact(
    core: &Core,
    draft: ArtifactDraft,
    supersedes: Option<Id>,
    existing: &[Artifact],
) -> ApiResult<Artifact> {
    let clock = &core.processing.clock;
    if let Some(max) = existing.iter().map(|a| a.created_at).max() {
        clock.observe(max);
    }
    let mut d = draft;
    d.supersedes = supersedes;
    let a = d.into_artifact(Id::new(), clock.now(), core.device_id().clone());
    core.store()
        .append_local(EventBody::ArtifactAdd(a.clone()))?;
    core.processing.kick();
    Ok(a)
}

async fn actions_propose(core: Arc<Core>, p: MeetingParams) -> ApiResult<ActionProposals> {
    let m = core
        .store()
        .get_meeting(&p.meeting_id)?
        .ok_or_else(|| Error::NotFound(format!("meeting {}", p.meeting_id)))?;
    let arts = core.store().list_artifacts(&m.id, None)?;
    let mut proposals = vec![];
    for a in &core.processing.actions.list {
        proposals.extend(a.propose(&m, &arts).await?);
    }
    let body = ActionProposals { proposals };
    let prev = latest_live(&arts, ArtifactKind::ACTION_PROPOSALS);
    let unchanged = prev
        .and_then(|a| a.decode::<ActionProposals>().ok())
        .is_some_and(|b| b == body);
    if !unchanged && (!body.proposals.is_empty() || prev.is_some()) {
        let inputs = [
            ArtifactKind::SUMMARY,
            ArtifactKind::ACTION_ITEMS,
            ArtifactKind::ENHANCED_NOTES,
        ]
        .iter()
        .filter_map(|k| latest_live(&arts, k).map(|a| a.id.clone()))
        .collect();
        let draft = ArtifactDraft::new(
            m.id.clone(),
            &body,
            Provenance {
                processor: "actions.propose".into(),
                processor_version: env!("CARGO_PKG_VERSION").into(),
                model: None,
                params_hash: None,
                inputs,
            },
        )?;
        add_artifact(&core, draft, prev.map(|a| a.id.clone()), &arts)?;
    }
    Ok(body)
}

async fn actions_execute(core: Arc<Core>, p: ExecuteParams) -> ApiResult<ActionResult> {
    let action = core
        .processing
        .actions
        .get(&p.proposal.action)
        .ok_or_else(|| Error::NotFound(format!("action {}", p.proposal.action)))?;
    let res = match action.execute(&p.proposal).await {
        Ok(r) => r,
        Err(e) => ActionResult {
            proposal_id: p.proposal.id.clone(),
            ok: false,
            message: e.to_string(),
            url: None,
        },
    };
    let meeting = p.proposal.payload["meeting_id"]
        .as_str()
        .map(|s| Id(s.to_string()));
    if let Some(mid) = meeting
        && core.store().get_meeting(&mid)?.is_some()
    {
        let arts = core.store().list_artifacts(&mid, None)?;
        let draft = ArtifactDraft::new(
            mid,
            &res,
            Provenance {
                processor: format!("actions.execute:{}", p.proposal.action),
                processor_version: env!("CARGO_PKG_VERSION").into(),
                model: None,
                params_hash: None,
                inputs: latest_live(&arts, ActionProposals::KIND)
                    .map(|a| vec![a.id.clone()])
                    .unwrap_or_default(),
            },
        )?;
        add_artifact(&core, draft, None, &arts)?;
    }
    Ok(res)
}

async fn models_list(core: Arc<Core>, _: NoParams) -> ApiResult<Vec<ModelEntry>> {
    let info = core.processing.refresh_capabilities(&core).await;
    let mut v = stubs::speech_models(&core)?;
    let slots = caps::effective_slots(&core, &info);
    let used_by = |name: &str| -> Option<String> {
        let tasks: Vec<String> = slots
            .iter()
            .filter(|s| ollama_name(&s.model) == name)
            .map(|s| match &s.lang {
                Some(l) => format!("{} ({l})", s.task),
                None => s.task.clone(),
            })
            .collect();
        (!tasks.is_empty()).then(|| format!("Ollama · used for {}", tasks.join(", ")))
    };
    for m in &info.llm_models {
        v.push(ModelEntry {
            id: ollama_id(&m.name),
            kind: if is_embed_model(&m.name) {
                "embed"
            } else {
                "llm"
            }
            .into(),
            installed: true,
            size_bytes: m.size_bytes,
            languages: vec![],
            note: used_by(&m.name).or_else(|| Some("Ollama".into())),
        });
    }
    // Recommended models that are missing: shown so the user knows what to pull.
    for s in &slots {
        if (s.task.starts_with("llm") || s.task == caps::SLOT_EMBED)
            && !info.has_model(&s.model)
            && !v.iter().any(|e| e.id == ollama_id(&s.model))
        {
            let name = ollama_name(&s.model);
            v.push(ModelEntry {
                id: ollama_id(name),
                kind: if is_embed_model(name) { "embed" } else { "llm" }.into(),
                installed: false,
                size_bytes: None,
                languages: vec![],
                note: Some(match &info.llm_error {
                    Some(e) => format!("Ollama unavailable: {e}"),
                    None => format!("not installed: run `ollama pull {name}`"),
                }),
            });
        }
    }
    Ok(v)
}

/// `models.download`: speech models download through `capture` (progress as
/// `models.downloadProgress`); the returned job is local bookkeeping, not a pipeline job.
/// Ollama models are pulled with `ollama pull`.
async fn models_download(core: Arc<Core>, p: ModelIdParams) -> ApiResult<Job> {
    let speech = stubs::speech_models(&core)?;
    if !speech.iter().any(|m| m.id == p.id) {
        let info = core.processing.capabilities();
        if info.has_model(&p.id) {
            return Err(ApiError::invalid(format!("{} is already installed", p.id)));
        }
        if p.id.starts_with("ollama:") || p.id.contains(':') {
            return Err(Error::Unsupported(format!(
                "Almond does not pull Ollama models; run `ollama pull {}`",
                ollama_name(&p.id)
            ))
            .into());
        }
        return Err(Error::NotFound(format!("model {}", p.id)).into());
    }
    let job = Job {
        id: Id::new(),
        meeting_id: Id(String::new()),
        processor: "models.download".into(),
        inputs: vec![],
        params: json!({ "id": p.id }),
        requires: vec![],
        requested_by: core.device_id().clone(),
        requested_at: core.processing.clock.now(),
        priority: 0,
    };
    let emit = |core: &Core, job: &Job, status, progress, message: String| {
        core.notifier.emit(
            notifications::JOB_PROGRESS,
            &JobProgress {
                job: job.clone(),
                status,
                device: Some(core.device_id().clone()),
                progress,
                message,
            },
        );
    };
    emit(&core, &job, JobStatus::Claimed, 0.0, "downloading".into());
    let (c, j) = (core.clone(), job.clone());
    tokio::spawn(async move {
        match stubs::download_speech_model(&c, &p.id).await {
            Ok(()) => {
                emit(&c, &j, JobStatus::Done, 1.0, "installed".into());
                c.processing.refresh_capabilities(&c).await;
            }
            Err(e) => emit(&c, &j, JobStatus::Failed, 0.0, e.to_string()),
        }
    });
    Ok(job)
}
