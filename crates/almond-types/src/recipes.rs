//! Recipes ("cookbook"): reusable AI prompts run on a meeting, e.g. "Teach it back to me",
//! "Questions I forgot to ask", "How did I communicate?".
//!
//! A [`Recipe`] is a synced record (collection [`COLLECTION`]). Running one produces a
//! `recipe_output` artifact ([`crate::artifact::body::RecipeOutput`]) for the meeting, through
//! the normal job pipeline (processor `recipe.llm`, params `{ "recipe_id": ... }`), so a weak
//! device can leave it for the PC like any other job.

use crate::ids::Id;
use crate::time::UnixMs;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Record collection for recipes.
pub const COLLECTION: &str = "recipe";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Recipe {
    /// Built-ins use stable ids ("builtin:teach_back"); user recipes use a ULID.
    pub id: String,
    pub name: String,
    /// One line shown in the cookbook.
    #[serde(default)]
    pub description: String,
    /// Emoji or short icon name for the cookbook card.
    #[serde(default)]
    pub icon: Option<String>,
    /// Instructions to the model. The meeting (transcript, summary, the user's notes, attendees)
    /// is supplied separately; the prompt says what to do with it.
    pub prompt: String,
    /// Run automatically on every new meeting once its transcript exists.
    #[serde(default)]
    pub auto_run: bool,
    /// Shipped with the app. Editing a built-in saves a user copy with a new id.
    #[serde(default)]
    pub builtin: bool,
    #[ts(type = "number")]
    pub updated_at: UnixMs,
}

/// Params of the `recipe.llm` job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RecipeRunParams {
    pub recipe_id: String,
    /// Extra instruction for this run ("focus on the budget part").
    #[serde(default)]
    pub extra: Option<String>,
    /// Output language; default: the meeting's main language.
    #[serde(default)]
    pub lang: Option<String>,
    /// Filled by the core when the job is requested (the recipe as it was then).
    #[serde(default)]
    pub recipe: Option<Recipe>,
    #[serde(default)]
    pub meeting_id: Option<Id>,
}
