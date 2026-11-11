# Contract requests: feature `recipes`

Worked around locally; nothing blocks the feature.

1. **`Recipe::hidden` (almond-types).** The brief has built-ins that can be hidden, but `Recipe` has
   no field for it. Workaround: the `recipe` record body is `almond_core::recipes::StoredRecipe`
   (`Recipe` flattened + `hidden: bool`); the UI types it as `RecipeInfo = Recipe & { hidden }`.
   Request: add `#[serde(default)] pub hidden: bool` to `Recipe` (old records still decode), then
   `StoredRecipe` can go.

2. **Planner: processors that consume an artifact but must not be auto-planned (almond-pipeline).**
   With `consumes: ["transcript"]`, the job loop's planner would request a recipe-less
   `recipe.llm` job for every meeting with a transcript (`PlanPolicy.auto` is `None` = every
   processor). Workaround: `recipe.llm` declares `consumes: []` (and `needs_audio: false`, which
   the planner skips) and gets its transcript through `Job::inputs`. Request: a descriptor flag
   (e.g. `ProcessorDescriptor::manual: bool`) or a planner exclusion list, so the descriptor can
   say `consumes: ["transcript"]` for the DAG view in `pipeline.processors`.

3. **Runner: supersede by a key, not by (kind, processor) (almond-pipeline).** `JobRunner::publish`
   makes a draft without `supersedes` replace the live artifact of the same kind and
   `provenance.processor`. All recipe outputs share kind and processor, so a run of recipe B
   would supersede recipe A's output, and there is no way to say "supersedes nothing".
   Workaround: recipe outputs carry `provenance.processor = "recipe.llm/<recipe_id>"` (and set
   `supersedes` explicitly to the same recipe's live output). Request: e.g. an
   `ArtifactDraft::supersede_key` (or "explicit `None` means none"), so provenance can say
   `recipe.llm`.
