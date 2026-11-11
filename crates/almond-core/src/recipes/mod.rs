//! Feature module "recipes" (the cookbook): reusable AI prompts run on a meeting
//! (docs/FEATURES-2.md, "Feature `recipes`").
//!
//! - Recipes are synced records (collection `recipe`). Built-ins ([`builtins`], ids
//!   `builtin:*`) live in code; a record with a built-in's id only stores the user's preferences
//!   for it (hidden, auto-run). Editing a built-in's text saves a user copy with a new id.
//! - [`processor`]: `recipe.llm`, registered in `State::init`; the processing job loop runs it
//!   (and offloads it to a stronger device like any job).
//! - Auto-run: recipes with `auto_run` are requested once for every new transcript this device
//!   produces (watching `artifact.added`), plus a sweep at start for transcripts produced while
//!   the app was closing. Only the device that wrote the transcript requests, so two devices
//!   never request the same run.
//!
//! Methods: `recipes.list`, `recipes.save`, `recipes.delete`, `recipes.run`, `recipes.outputs`.

pub mod builtins;
pub mod cites;
pub mod processor;
pub mod stats;

use crate::error::ApiResult;
use crate::router::{NoParams, Router};
use crate::{Core, Shared};
use almond_pipeline::JobRequest;
use almond_pipeline::planner::{latest_live, live_artifacts};
use almond_types::api::notifications;
use almond_types::artifact::body::RecipeOutput;
use almond_types::job::{Job, JobStatus};
use almond_types::recipes::{COLLECTION, Recipe, RecipeRunParams};
use almond_types::store::{MeetingQuery, Store};
use almond_types::time::now_ms;
use almond_types::{Artifact, ArtifactKind, Error, Id, Result};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::broadcast;

pub use processor::RECIPE_LLM;

/// Priority of user-requested runs (ahead of background summaries) and of auto-runs.
const RUN_PRIORITY: i32 = 20;
const AUTO_RUN_PRIORITY: i32 = 0;
/// At start, auto-run recipes for transcripts this device wrote within this window.
const SWEEP_WINDOW_MS: i64 = 6 * 3_600_000;

/// What a `recipe` record holds: the recipe plus per-user state that `Recipe` has no field for
/// (docs/contract-requests/f-recipes.md asks for `Recipe::hidden`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredRecipe {
    #[serde(flatten)]
    pub recipe: Recipe,
    #[serde(default)]
    pub hidden: bool,
}

/// A recipe as the API returns it (`Recipe` + `hidden`).
pub type RecipeInfo = StoredRecipe;

pub struct State {
    core: processor::CoreSlot,
    /// Serializes auto-run checks so one transcript never gets two requests.
    auto_lock: Mutex<()>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl State {
    pub(crate) async fn init(shared: &Shared) -> Result<State> {
        let core: processor::CoreSlot = Arc::new(OnceLock::new());
        shared
            .registry
            .register(Arc::new(processor::RecipeProcessor { core: core.clone() }))?;
        Ok(State {
            core,
            auto_lock: Mutex::new(()),
            tasks: Mutex::new(vec![]),
        })
    }

    pub(crate) async fn shutdown(&self) {
        for t in self.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
    }
}

// ------------------------------------------------------------------------------ recipes

/// Every recipe: built-ins first (in their fixed order), then the user's by name.
pub fn list(core: &Core) -> Result<Vec<RecipeInfo>> {
    let stored: Vec<StoredRecipe> = core.records(COLLECTION)?;
    let mut out = vec![];
    for b in builtins::all() {
        let prefs = stored.iter().find(|s| s.recipe.id == b.id);
        out.push(StoredRecipe {
            hidden: prefs.is_some_and(|p| p.hidden),
            recipe: Recipe {
                auto_run: prefs.is_some_and(|p| p.recipe.auto_run),
                updated_at: prefs.map_or(0, |p| p.recipe.updated_at),
                ..b
            },
        });
    }
    let mut users: Vec<StoredRecipe> = stored
        .into_iter()
        .filter(|s| !builtins::is_builtin_id(&s.recipe.id))
        .map(|mut s| {
            s.recipe.builtin = false;
            s
        })
        .collect();
    users.sort_by_key(|s| (s.recipe.name.to_lowercase(), s.recipe.id.clone()));
    out.extend(users);
    Ok(out)
}

pub fn get(core: &Core, id: &str) -> Result<Option<RecipeInfo>> {
    Ok(list(core)?.into_iter().find(|r| r.recipe.id == id))
}

/// `recipes.save` params: a new recipe (no `id`) or changes to an existing one.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SaveParams {
    pub id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    /// `""` clears the icon.
    pub icon: Option<String>,
    pub prompt: Option<String>,
    pub auto_run: Option<bool>,
    pub hidden: Option<bool>,
}

/// Create or update a recipe. For a built-in, `auto_run` / `hidden` are stored as preferences;
/// a change to its name, description, icon or prompt saves a user copy (new id) instead.
pub fn save(core: &Core, p: SaveParams) -> Result<RecipeInfo> {
    let existing = match &p.id {
        Some(id) => Some(get(core, id)?.ok_or_else(|| Error::NotFound(format!("recipe {id}")))?),
        None => None,
    };
    let base = existing.clone().unwrap_or_else(|| StoredRecipe {
        recipe: Recipe {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            icon: None,
            prompt: String::new(),
            auto_run: false,
            builtin: false,
            updated_at: 0,
        },
        hidden: false,
    });
    let mut next = base.clone();
    if let Some(n) = &p.name {
        next.recipe.name = n.trim().to_string();
    }
    if let Some(d) = &p.description {
        next.recipe.description = d.trim().to_string();
    }
    if let Some(i) = &p.icon {
        let i = i.trim();
        next.recipe.icon = (!i.is_empty()).then(|| i.to_string());
    }
    if let Some(pr) = &p.prompt {
        next.recipe.prompt = pr.trim().to_string();
    }
    if let Some(a) = p.auto_run {
        next.recipe.auto_run = a;
    }
    if let Some(h) = p.hidden {
        next.hidden = h;
    }
    if next.recipe.name.is_empty() {
        return Err(Error::Invalid("a recipe needs a name".into()));
    }
    if next.recipe.prompt.is_empty() {
        return Err(Error::Invalid("a recipe needs a prompt".into()));
    }
    next.recipe.updated_at = now_ms();

    let content_changed = |a: &Recipe, b: &Recipe| {
        a.name != b.name
            || a.description != b.description
            || a.icon != b.icon
            || a.prompt != b.prompt
    };
    if base.recipe.builtin && content_changed(&base.recipe, &next.recipe) {
        // Editing a built-in: a user copy with the edits; the built-in stays as it was.
        next.recipe.id = Id::new().0;
        next.recipe.builtin = false;
        next.hidden = false;
    } else if existing.is_none() {
        next.recipe.id = Id::new().0;
        next.recipe.builtin = false;
    }
    core.put_record(COLLECTION, &next.recipe.id, &next)?;
    Ok(next)
}

/// Delete a user recipe. Built-ins can only be hidden.
pub fn delete(core: &Core, id: &str) -> Result<()> {
    if builtins::is_builtin_id(id) {
        return Err(Error::Invalid(
            "built-in recipes can't be deleted; hide them instead".into(),
        ));
    }
    if core.record::<StoredRecipe>(COLLECTION, id)?.is_none() {
        return Err(Error::NotFound(format!("recipe {id}")));
    }
    core.delete_record(COLLECTION, id)
}

// ------------------------------------------------------------------------------ runs

/// `recipes.run` params.
#[derive(Debug, Clone, Deserialize)]
pub struct RunParams {
    pub recipe_id: String,
    pub meeting_id: Id,
    #[serde(default)]
    pub extra: Option<String>,
    /// Output language; default: setting `ui.language` on this device, else the meeting's.
    #[serde(default)]
    pub lang: Option<String>,
}

fn latest_transcript(core: &Core, meeting: &Id) -> Result<Option<Artifact>> {
    let arts = core
        .store()
        .list_artifacts(meeting, Some(ArtifactKind::TRANSCRIPT))?;
    Ok(latest_live(&arts, ArtifactKind::TRANSCRIPT).cloned())
}

fn request(
    core: &Core,
    recipe: &Recipe,
    meeting: &Id,
    transcript: &Id,
    extra: Option<String>,
    lang: Option<String>,
    priority: i32,
) -> Result<Job> {
    let lang = lang
        .filter(|l| !l.trim().is_empty() && l != "auto")
        .or_else(|| {
            core.setting_as::<String>(crate::settings::keys::UI_LANGUAGE)
                .ok()
                .flatten()
                .filter(|l| matches!(l.as_str(), "vi" | "en"))
        });
    let params = RecipeRunParams {
        recipe_id: recipe.id.clone(),
        extra: extra.filter(|e| !e.trim().is_empty()),
        lang,
        recipe: Some(recipe.clone()),
        meeting_id: Some(meeting.clone()),
    };
    let job = core.processing.runner().request(JobRequest {
        meeting_id: meeting.clone(),
        processor: RECIPE_LLM.into(),
        inputs: vec![transcript.clone()],
        params: serde_json::to_value(params)?,
        priority,
        requires: None,
    })?;
    core.processing.kick();
    Ok(job)
}

/// Request a run of `recipe_id` on a meeting's latest transcript.
pub fn run(core: &Core, p: RunParams) -> Result<Job> {
    let recipe = get(core, &p.recipe_id)?
        .ok_or_else(|| Error::NotFound(format!("recipe {}", p.recipe_id)))?
        .recipe;
    if core.store().get_meeting(&p.meeting_id)?.is_none() {
        return Err(Error::NotFound(format!("meeting {}", p.meeting_id)));
    }
    let t = latest_transcript(core, &p.meeting_id)?.ok_or_else(|| {
        Error::Invalid(
            "this meeting has no transcript yet; run recipes once it is transcribed".into(),
        )
    })?;
    request(
        core,
        &recipe,
        &p.meeting_id,
        &t.id,
        p.extra,
        p.lang,
        RUN_PRIORITY,
    )
}

/// Live (not superseded) recipe outputs, latest first.
pub(crate) fn live_outputs(arts: &[Artifact]) -> Vec<&Artifact> {
    live_artifacts(arts)
        .into_iter()
        .filter(|a| a.kind.is(ArtifactKind::RECIPE_OUTPUT))
        .collect()
}

/// A meeting's live recipe outputs, latest first (one per recipe).
pub fn outputs(core: &Core, meeting: &Id) -> Result<Vec<Artifact>> {
    let arts = core
        .store()
        .list_artifacts(meeting, Some(ArtifactKind::RECIPE_OUTPUT))?;
    let mut seen = std::collections::HashSet::new();
    Ok(live_outputs(&arts)
        .into_iter()
        .filter(|a| match a.decode::<RecipeOutput>() {
            Ok(o) => seen.insert(o.recipe_id),
            Err(_) => false,
        })
        .cloned()
        .collect())
}

// ------------------------------------------------------------------------------ auto-run

/// Was `recipe` already requested (or produced) for this transcript, on any device?
fn already_ran(core: &Core, recipe: &str, transcript: &Artifact) -> Result<bool> {
    let states = core.processing.runner().job_states()?;
    let requested = states.values().any(|s| {
        s.job.processor == RECIPE_LLM
            && s.job.meeting_id == transcript.meeting_id
            && s.job.inputs.contains(&transcript.id)
            && s.job.params.get("recipe_id").and_then(|v| v.as_str()) == Some(recipe)
            && !(s.status() == JobStatus::Cancelled)
    });
    if requested {
        return Ok(true);
    }
    let arts = core
        .store()
        .list_artifacts(&transcript.meeting_id, Some(ArtifactKind::RECIPE_OUTPUT))?;
    Ok(arts.iter().any(|a| {
        a.provenance.inputs.contains(&transcript.id)
            && a.decode::<RecipeOutput>()
                .is_ok_and(|o| o.recipe_id == recipe)
    }))
}

/// Request every auto-run recipe for a new transcript (once). Returns the jobs requested.
pub fn auto_run(core: &Core, transcript: &Artifact) -> Result<Vec<Job>> {
    if !transcript.kind.is(ArtifactKind::TRANSCRIPT) || transcript.device != *core.device_id() {
        return Ok(vec![]);
    }
    let recipes: Vec<Recipe> = list(core)?
        .into_iter()
        .filter(|r| r.recipe.auto_run)
        .map(|r| r.recipe)
        .collect();
    if recipes.is_empty() {
        return Ok(vec![]);
    }
    // Only the meeting's live transcript (not one already replaced by a re-transcription).
    match latest_transcript(core, &transcript.meeting_id)? {
        Some(t) if t.id == transcript.id => {}
        _ => return Ok(vec![]),
    }
    let _g = core.recipes.auto_lock.lock().unwrap();
    let mut jobs = vec![];
    for r in recipes {
        if already_ran(core, &r.id, transcript)? {
            continue;
        }
        jobs.push(request(
            core,
            &r,
            &transcript.meeting_id,
            &transcript.id,
            None,
            None,
            AUTO_RUN_PRIORITY,
        )?);
    }
    if !jobs.is_empty() {
        tracing::info!(
            "recipes: auto-run {} recipe(s) for meeting {}",
            jobs.len(),
            transcript.meeting_id
        );
    }
    Ok(jobs)
}

/// Transcripts this device wrote recently (while the app may have closed before auto-run).
fn sweep(core: &Core) -> Result<()> {
    if !list(core)?.iter().any(|r| r.recipe.auto_run) {
        return Ok(());
    }
    let since = now_ms() - SWEEP_WINDOW_MS;
    for m in core.store().list_meetings(&MeetingQuery::default())? {
        if m.ended_at.is_some_and(|e| e < since) {
            continue;
        }
        if let Some(t) = latest_transcript(core, &m.id)?
            && t.created_at.wall_ms >= since
        {
            auto_run(core, &t)?;
        }
    }
    Ok(())
}

async fn watch(core: std::sync::Weak<Core>, mut rx: broadcast::Receiver<crate::Notification>) {
    loop {
        let n = match rx.recv().await {
            Ok(n) => n,
            Err(broadcast::error::RecvError::Lagged(k)) => {
                tracing::warn!("recipes: missed {k} notifications; sweeping");
                if let Some(c) = core.upgrade()
                    && let Err(e) = sweep(&c)
                {
                    tracing::warn!("recipes: sweep failed: {e}");
                }
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if n.name != notifications::ARTIFACT_ADDED
            || n.payload.get("kind").and_then(|k| k.as_str()) != Some(ArtifactKind::TRANSCRIPT)
        {
            continue;
        }
        let Some(c) = core.upgrade() else { return };
        match serde_json::from_value::<Artifact>(n.payload) {
            Ok(a) => {
                if let Err(e) = auto_run(&c, &a) {
                    tracing::warn!("recipes: auto-run for {}: {e}", a.meeting_id);
                }
            }
            Err(e) => tracing::warn!("recipes: bad artifact.added payload: {e}"),
        }
    }
}

// ------------------------------------------------------------------------------ API

#[derive(Debug, Deserialize)]
struct MeetingParams {
    meeting_id: Id,
}

#[derive(Debug, Deserialize)]
struct StrIdParams {
    id: String,
}

pub(crate) fn register(r: &mut Router) {
    r.add("recipes.list", |core, _: NoParams| async move {
        ApiResult::Ok(list(&core)?)
    });
    r.add("recipes.save", |core, p: SaveParams| async move {
        ApiResult::Ok(save(&core, p)?)
    });
    r.add("recipes.delete", |core, p: StrIdParams| async move {
        delete(&core, &p.id)?;
        ApiResult::Ok(serde_json::Value::Null)
    });
    r.add("recipes.run", |core, p: RunParams| async move {
        ApiResult::Ok(run(&core, p)?)
    });
    r.add("recipes.outputs", |core, p: MeetingParams| async move {
        ApiResult::Ok(outputs(&core, &p.meeting_id)?)
    });
}

pub(crate) fn start(core: Arc<Core>) {
    let _ = core.recipes.core.set(Arc::downgrade(&core));
    let rx = core.notifier.subscribe();
    let weak = Arc::downgrade(&core);
    let sweep_core = weak.clone();
    let handles = vec![
        tokio::spawn(watch(weak, rx)),
        tokio::spawn(async move {
            // After the job loop's first scan, so jobs requested before a restart are visible.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if let Some(c) = sweep_core.upgrade()
                && let Err(e) = sweep(&c)
            {
                tracing::warn!("recipes: startup sweep failed: {e}");
            }
        }),
    ];
    core.recipes.tasks.lock().unwrap().extend(handles);
}
