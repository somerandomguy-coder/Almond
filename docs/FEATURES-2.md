# Feature round 2: Ask Almond (chat), Recipes (cookbook), People

Goal: make Almond feel like Granola day to day.
- **Ask Almond:** a chat on the home screen, under the calendar and notes, that knows your
  meetings, notes and upcoming events.
- **Recipes:** a cookbook of AI prompts to run on a meeting: "teach it back to me", "questions I
  forgot to ask", "how did I communicate", plus your own.
- **People:** a catalog of the people you meet, with their voices recognised so transcripts show
  names, and every meeting with someone in one place.

Everything runs locally (Ollama, the speech models in `%USERPROFILE%\.almond\models`) and syncs
between the user's devices.

## Shared foundation (already on `main`, tag `m3-foundation`)

- **Synced records** (`almond_types::store::Record`, events `record.upsert` / `record.delete`,
  LWW per `(collection, id)`): `core.put_record(collection, id, &value)`,
  `core.record::<T>(…)`, `core.records::<T>(collection)`, `core.delete_record(…)`
  (`crates/almond-core/src/records.rs`). They work in both stores and sync like everything else.
  **Use records for new synced data. Don't add event kinds or store tables.**
- **Types** (`crates/almond-types/src/`):
  - `people.rs`: `Person`, `VoicePrint`, collection `"person"`.
  - `recipes.rs`: `Recipe`, `RecipeRunParams`, collection `"recipe"`.
  - `chat.rs`: `ChatThread`, `ChatScope`, `ChatEntry`, `ChatSource`, collection `"chat"`.
  - Artifact bodies: `body::RecipeOutput` (kind `recipe_output`) and `body::SpeakerLabels` /
    `SpeakerLabel` / `LabelSource` (kind `speaker_labels`).
  - TS types are generated into `apps/desktop/src/gen`.
- **Core modules** with empty stubs: `crates/almond-core/src/{chat,recipes,people}.rs`, already wired
  into `Core` (fields `core.chat`, `core.recipes`, `core.people`) with the usual
  `State::init` → `register` → `start` → `shutdown` lifecycle (see `docs/PHASE2.md`).

## Rules (same as Phase 2)

- **Branch:** your own, `ws/f-<feature>`. Commit and push at milestones. Open a PR to `main` when done.
- **Files you own:**
  - `crates/almond-core/src/<feature>.rs` (or `<feature>/`) and `crates/almond-core/tests/<feature>*.rs`.
  - Your section of `docs/API.md`.
  - The UI files listed for you.
- **Shared files:** small, clearly separated additions only:
  - `apps/desktop/src/api/types.ts` and `api/mock/MockApi.ts` (the mock must keep working with `?mock=1`);
  - `i18n/en.ts` + `i18n/vi.ts` (your own key block);
  - `App.tsx` routes;
  - `components/Shell.tsx` navigation.

  Expect the integrator to resolve trivial conflicts there.
- **Don't edit:**
  - `crates/almond-types`, `lib.rs`, `router.rs`, `records.rs`;
  - the other two features' modules;
  - capture/processing/library internals, except the hooks listed for you.

  If you need something, work around it locally and write it in
  `docs/contract-requests/f-<feature>.md`.
- **LLM access:**
  - Go through the processing module's existing helpers: model slot via
    `processing::caps::slot_model`, Ollama URL via `processing::ollama_url` (remote-Ollama aware), the
    `almond_llm::Ollama` provider.
  - Respect the user's slot choice (default `gemma4:12b` on the PC).
  - Fail with a clear message when Ollama isn't running, as `jobs.list` does now.
  - Prompts: English instructions, answer in the user's language (VI or EN, from `ui.language` or
    the question), cite sources.
- **Don't run `pnpm tauri dev`:** others share this PC. Test through the core (`open_core` +
  `api.call`) with mock LLMs, plus `#[ignore]`d tests against the real Ollama
  (`http://127.0.0.1:11434`, don't pull models). The integrator does the real-app run.
- **Quality bar:**
  - `cargo fmt --all`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test --workspace`;
  - `pnpm -C apps/desktop build && pnpm -C apps/desktop test`.
- **Report:**
  - what works end to end, with evidence;
  - what only compiles;
  - UI files touched;
  - contract requests;
  - the PR URL.

---

## Feature `chat`: "Ask Almond"

**Owns:**
- `crates/almond-core/src/chat*`;
- new UI components under `apps/desktop/src/components/chat/`;
- the chat area of `screens/Home.tsx`;
- a chat side panel or tab in `screens/Meeting.tsx` (keep that edit small).

**Methods:** `chat.ask { thread_id?, scope, message }` → `{ thread_id, entry }` (the assistant's
answer), `chat.threads { scope? }`, `chat.get { thread_id }`, `chat.delete { thread_id }`.
**Notification:** `chat.delta { thread_id, entry_id, text }` while the answer streams.

1. **Context builder per `ChatScope`:**
   - `All`:
     - upcoming events (next 7 days, with attendees and linked meetings);
     - the most recent meetings (titles, dates, summaries);
     - full-text search hits for the question (`store.search`, Vietnamese diacritic folding
       already works) in transcripts, notes, summaries and recipe outputs;
     - people (names, last met) once records exist.
   - `Meeting`: that meeting's transcript, user notes (`library::notes_text`), summary, action
     items and recipe outputs, with long transcripts trimmed or retrieved by search.
   - `Person`: meetings with that person, via `speaker_labels` artifacts and calendar attendees by
     email.

   Keep it within the model's context (`almond_llm` estimates tokens). Number the sources so the
   answer can cite `[n]`, and return them as `ChatSource`s that link to the meeting or transcript
   segment.
2. **Streaming:** stream the answer through `chat.delta`, then save the thread (a `chat` record).
   The first question titles the thread.
3. **UI:**
   - **Home:** under the calendar/notes, a chat box: input with suggestions ("What do I have
     today?", "What did we decide about…", "Prep me for my next meeting"), streaming answer,
     clickable source chips (open the meeting at that segment), recent threads.
   - **Meeting:** "Ask about this meeting" panel.
   - VI + EN strings.

**Done when:**
- a core test with a mock LLM covers context building for each scope (including a search hit
  and an upcoming event), streaming notifications, and thread persistence and sync to a second
  core;
- an ignored real-Ollama test answers "what's coming up" and a question about a fixture meeting
  with correct citations;
- UI vitest for the Home chat on the mock.

## Feature `recipes`: the cookbook

**Owns:**
- `crates/almond-core/src/recipes*`;
- a new `screens/Cookbook.tsx`;
- a "Recipes" tab or section in `screens/Meeting.tsx` (keep it small);
- `components/recipes/`.

**Methods:** `recipes.list`, `recipes.save` (create/update; editing a built-in saves a copy),
`recipes.delete`, `recipes.run { recipe_id, meeting_id, extra? }` → `Job`,
`recipes.outputs { meeting_id }` → `Artifact[]`.

1. **Built-in recipes**, ids `builtin:*`, shipped in code and seeded into the list. The user can
   hide them but not delete them.
   - **Teach it back:** explain what happened as if teaching me, with key concepts and a short
     quiz at the end.
   - **Questions I forgot to ask:** gaps, unclear decisions, risks, plus follow-up questions per
     person.
   - **How did I communicate?** Talk-time share (from transcript speakers; "me" is the mic
     channel), interruptions, filler words, clarity, what went well, one thing to try next time.
     Use the transcript numbers, not vibes.
   - **Decisions & owners:** a table of decisions, owner, due date.
   - **Follow-up email draft:** text only; sending is a later Action.
2. **Processor `recipe.llm`:**
   - Consumes `transcript`, produces `recipe_output`, requires `llm`.
   - Register it in `State::init` into `shared.registry` so the job loop runs it (it then also
     offloads from the laptop to the PC like any job).
   - Params `RecipeRunParams` (the core snapshots the recipe into params when requesting).
   - Output Markdown with `[n]` citations into the transcript, validated like the summary's (see
     `almond_llm` cite handling).
   - A rerun of the same recipe on the same meeting supersedes the previous output.
3. **Auto-run:**
   - Recipes with `auto_run` are requested for each new meeting once its transcript exists.
   - Watch `artifact.added` notifications or the store; don't touch the processing planner.
   - Don't request twice for the same transcript.
4. **UI:**
   - **Cookbook screen:** cards per recipe, create and edit (name, icon, prompt, auto-run),
     "Run on…" a meeting.
   - **Meeting view:** recipe outputs rendered as Markdown with citation chips (reuse the summary's
     citation components), a "Run a recipe" menu, and rerun.
   - Add a nav entry in `Shell.tsx`.

**Done when:**
- a core test with a mock LLM covers the built-ins listed, save, edit-builtin-makes-copy, run →
  job → `recipe_output` artifact with valid citations, auto-run on a new transcript (once), and
  sync of a user recipe to a second core;
- an ignored real-Ollama run of the built-ins on a fixture meeting, with output quality notes
  in the report;
- UI vitest for the Cookbook on the mock.

## Feature `people`: catalog + voice labels

**Owns:**
- `crates/almond-core/src/people*`;
- a new `crates/almond-core/src/capture/voice.rs` (voice embeddings; the only file you add in
  capture, plus one `pub mod voice;` line in `capture/mod.rs`);
- new `screens/People.tsx` and `screens/Person.tsx`;
- speaker-label UI in `components/TranscriptPanel.tsx`, and showing names instead of "S2" there
  and in summaries.

**Methods:**
- `people.list`, `people.get { id }` → `{ person, meetings, talk_time_s, last_met }`;
- `people.save`, `people.delete`, `people.merge { keep, remove }`;
- `people.labelSpeaker { meeting_id, speaker, person_id? | name? }` → `SpeakerLabels` (creates
  the person if given a name and adds a `VoicePrint` from that speaker's audio);
- `people.unlabelSpeaker`;
- `people.suggestions { meeting_id }` → suggested labels.

1. **Voice prints** (`capture/voice.rs`):
   - Embed a speaker's segments with the diarization embedding model already installed
     (`sherpa:3dspeaker-eres2net-base`, through almond-stt).
   - Mean + L2-normalise. Use at least ~5 s of speech; skip segments under 1 s.
   - Decode audio from the meeting's blobs (see `capture/transcribe.rs` for how tracks are read).
   - Without the `sherpa` feature, return `Unsupported` (labels still work, just without voice
     matching).
2. **Auto-labelling:** when a transcript is added (watch `artifact.added`), compute an embedding
   per speaker and compare (cosine) with every person's voice prints.
   - Above a threshold you calibrate (report how), write a `speaker_labels` artifact with
     `source: voice` and the confidence.
   - Also match calendar attendees by email for meetings linked to an event (`source:
     calendar`, only when unambiguous).
   - The mic channel / "me" maps to the person with `is_self`; create that person on first use,
     named after the device's user if known, else "Me".
   - User labels (`source: user`) always win and are never overwritten by suggestions.
3. **Catalog:**
   - Person page: meetings with them (by labels and by calendar attendee email), talk time,
     last met, notes, voice prints (count, delete), merge duplicates.
   - People list with search.
4. **UI:**
   - In the transcript, click a speaker chip → "Who is this?" with existing people, new name, or
     "this is me". Suggested labels show as dimmed names with a confirm tick.
   - Names replace "S2" in transcript and summary views.
   - People and Person screens; a nav entry.

**Done when:**
- a core test covers labelling a speaker (person created, voice print added; use synthetic
  audio, or a mock embedder behind a small trait if the real model isn't available in CI), auto-
  suggestion on a second meeting from the same "voice", calendar-email matching, user-label
  precedence, merge, person page aggregation, and sync of people to a second core;
- an ignored real-model test on two recordings of the same speaker (the eval data is gone: make
  short clips with the existing test tools or the WAV fixtures), reporting similarity
  same-speaker vs different-speaker;
- UI vitest for the label popover on the mock.
