# Phase 2 — Integration: briefs for the three module owners

Goal: the desktop app works for real on the Windows PC. Record a Google Meet call, see live
captions, press Stop, then get a transcript, summary, enhanced notes, action items and a fact-check
in the app. Sync with the Fedora laptop.

The skeleton is on `main` (tag `m2-skeleton`):
- `crates/almond-core/src/lib.rs`: `Core`, `Shared` (config, `SyncNode` with store/blobs/sync
  engine, `Notifier`, processor registry) and the module lifecycle `State::init` → `register` →
  `start` → `shutdown`.
- `router.rs`: typed handlers via `Router::add("ns.verb", |core, params: P| async move { ... })`.
  The router answers `-32601` for methods nobody has registered.
- `error.rs`: `ApiError { code, message }`. `almond_types::Error` converts to it with `?`.
- `events.rs`: `core.notifier.emit(notifications::X, payload)`. The Tauri shell forwards these
  to the UI as events named with `.` → `_`.
- `settings.rs`: `settings.get/set`, `app.info`, and the helpers `core.setting_as::<T>(key)` and
  `core.ollama_url()`.
- `apps/desktop/src-tauri/src/lib.rs`: one generic invoke handler. `meetings_list` →
  `api.call("meetings.list", params)`. **No per-method Tauri commands are needed.**
- `crates/almond-core/tests/common/mod.rs`: `open_core(name)` opens a real core in a temp dir.

## Rules for every module owner

- **Branch:** your own, `ws/i-<module>`. Commit and push at each milestone. Open a PR to `main` when done.
- **Files you own:**
  - `crates/almond-core/src/<module>.rs`. You may turn it into `src/<module>/mod.rs` with submodules.
  - `crates/almond-core/tests/<module>*.rs`.
  - The lower crates listed for your module. You are now the only one editing them; keep their tests green.
- **Files you don't own:**
  - **Don't edit** `lib.rs`, `router.rs`, `error.rs`, `events.rs`, `settings.rs`, the other two
    modules, or `crates/almond-types`.
  - If you need something there, write `docs/contract-requests/i-<module>.md`, work around it
    locally, and say so in your report.
  - New dependencies: add them to `crates/almond-core/Cargo.toml`, one per line at the end of
    `[dependencies]`. The integrator resolves the trivial merge conflicts.
- **UI** (`apps/desktop/src`): keep changes minimal and limited to what your methods need to work
  in the real app. This is mostly `api/types.ts` and the screens that call your methods. The UI
  must keep working with `?mock=1` (`pnpm build` and `pnpm test` stay green). List every UI file
  you touched in your report.
- **Don't run `pnpm tauri dev`.** Three agents share this PC and port 1420. Test through the core
  instead (`open_core` + `api.call`) and with examples. The integrator does the real-app run after
  merging.
- **Shared PC resources:**
  - One GPU (16 GB) runs both Ollama and whisper CUDA.
  - The Windows Ollama is at `http://127.0.0.1:11434`. Its models are `gemma4:12b`,
    `qwen3.5:4b`, `llama3.1:8b`, `qwen3-embedding:0.6b` and others; check `ollama list`.
    Don't pull models.
  - Speech models are in `%USERPROFILE%\.almond\models`. Treat them as read-only, except that
    `capture` may install missing ones through `almond-stt`'s model manager.
- **Quality bar:**
  - `cargo fmt --all`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test --workspace` (no `--all-features`: CI excludes almond-stt's GPU features);
  - `pnpm -C apps/desktop build && pnpm -C apps/desktop test` if you touched the UI.
- **Report:**
  - what works end to end (with evidence);
  - what only compiles;
  - your contract requests;
  - UI files touched;
  - anything the integrator must do when merging.

### Cross-module interfaces (implement your side; don't wait for the others)

| Provider | Function (in the provider's module) | Used by |
|---|---|---|
| capture | `pub fn speech_models(core: &Core) -> Result<Vec<ModelEntry>>` and `pub async fn download_speech_model(core: &Arc<Core>, id: &str) -> Result<()>` (emits `models.downloadProgress`) | processing (`models.list`, `models.download`) |
| processing | Runs automatically. Its job loop watches the store (local writes **and** synced events) and plans and requests every job that can run: a meeting with audio and no transcript gets `transcribe.local`, a transcript gets summary/notes/action items/title, and so on. capture therefore only has to finish writing the meeting's audio. | capture, library |
| processing | Emits `artifact.added` for every new artifact in the store, including ones that arrived by sync | UI |
| library | `pub fn notes_text(core: &Core, doc: &Id) -> Result<String>`: plain text of a meeting's Loro notes | processing (`enhance_notes.llm` gets it as `params.user_notes`) |

Until the other side lands, stub it locally (a private function with the same signature and a TODO).

---

## Module `capture`: recording, live captions, transcription

**Owns:** `crates/almond-core/src/capture.rs`, `crates/almond-audio`, `crates/almond-stt`, UI
recording controls. Also `apps/desktop/src-tauri/tauri.conf.json`, if the asset protocol is
needed for `audio.url`.

**Methods:** `recording.start`, `recording.stop`, `recording.status`, `audio.devices`, `audio.url`.
**Notifications:** `recording.status` (~4/s while recording), `transcript.update`.
**Processor:** `transcribe.local`, registered in `State::init` into `shared.registry`.

1. **Recording:** `recording.start`.
   - Create the meeting if needed (`meeting.upsert` event) with `tz` set to the local IANA zone.
   - Open a `CaptureSession`: mic + system loopback on desktop, mic only on Android.
   - Write crash-safe Opus chunks under `data_dir/recordings/<meeting>/`.
   - On start, recover unfinished sessions (`almond_audio::unfinished_sessions`).
2. **Stop:**
   - Finalize the files and `put_file` each track into the blob store.
   - Append `blob.available { hash, size, mime: "audio/ogg" }` for each.
   - Upsert the meeting with its `audio` tracks and `ended_at`.
   - Processing then takes over.
3. **Live captions:**
   - Stream the mic and system channels through the live engine (`sherpa:zipformer-vi-30m-int8` for
     Vietnamese; follow `almond_stt::routing` for others).
   - Emit `transcript.update { meeting_id, update }` and follow the `SttUpdate` partial/final
     protocol.
   - Skip `AudioFrame.synthesized` frames.
4. **`transcribe.local` processor:**
   - Consumes nothing (`needs_audio: true`) and produces a `transcript`.
   - Uses `RoutingTable::default_for(device class)`: PC with GPU = Zipformer for Vietnamese, `large-v3-turbo` for English and language ID.
   - Diarizes the system channel; the mic is "me".
   - **Run diarization concurrently with transcription.** It is ~80 % of the post-meeting time.
   - Without the `whisper`/`sherpa` features (CI builds), return a clear `Unsupported` error.
5. **`audio.url`:**
   - Returns a URL the webview can play, using the Tauri asset protocol (or `convertFileSrc` on
     the UI side, returning a path).
   - Returns `null` when the blob is not on this device.
6. **Speech models:** `speech_models` / `download_speech_model` (see the interface table).

**Done when:** a core integration test records 5 s from a WAV/push source (no real device needed in
CI) → meeting with audio + `blob.available` → `transcribe.local` produces a transcript (an
`#[ignore]`d test with real models on this PC). Also a real-device check on this PC: record ~10 s of
mic + system audio through the core (example or ignored test), report levels and track durations,
and time the post-meeting pass on `%USERPROFILE%\.almond\eval\stt\meeting\meeting_stereo.wav`
(target: under 4.5 min for the 30-min meeting with GPU features).

## Module `processing`: jobs, LLM, fact-checking, models

**Owns:** `crates/almond-core/src/processing.rs`, `crates/almond-pipeline`, `crates/almond-llm`,
`crates/almond-verify`, UI meeting-view artifact components where your methods need it.

**Methods:**
- `artifacts.list`, `artifacts.get`, `artifacts.rerun`;
- `jobs.list`, `jobs.cancel`;
- `pipeline.processors`, `pipeline.registerExternal`;
- `verify.run`, `verify.verifiers`;
- `actions.list`, `actions.propose`, `actions.execute`;
- `models.list`, `models.download`, `models.select`, `models.slots`.

**Notifications:** `job.progress`, `artifact.added`.

1. **Capabilities:**
   - Detect Ollama at `core.ollama_url()` and list its models.
   - Detect an NVIDIA GPU.
   - Expose capabilities (`gpu`, `llm`, `llm:<model>`, `stt:vi`, `stt:en`, `diarize` as appropriate).
   - Choose default model slots: `gemma4:12b` for summaries and the fact-check judge when present
     on a GPU device, `qwen3.5:4b` on CPU-only devices.
   - Selection is readable and writable via `models.slots` / `models.select`, stored under
     setting `models.slots`.
2. **Processors:** register the LLM processors (`summarize.llm`, `enhance_notes.llm`,
   `action_items.llm`, `title.llm`) and the verify processors (claim extraction + `verify.*`) in
   `State::init`.
3. **Job loop** (`start`):
   - Planner + `JobRunner` over the store.
   - Reacts to new meetings/artifacts and to synced events.
   - Runs what this device can run and leaves the rest for a more capable device.
   - Lease renewal and expiry.
   - Emits `job.progress` and `artifact.added`.
   - A new `title` artifact also updates the meeting title if the user hasn't set one.
4. **`enhance_notes.llm`:** pass `params.user_notes` from `library::notes_text`.
5. **Verify wiring:**
   - Fill `VerifyContext.calendar_event` from the meeting's linked event (store) and the time zone
     from `meeting.tz` / `params.tz`.
   - Report failing verifiers in `Verdicts.errors`.
6. **Actions:** `actions.list` with the built-ins that exist; "export Markdown" is a good first one.
   The Gmail follow-up draft is out of scope.

**Done when:**
- A core integration test with mock LLM/STT processors runs a meeting with audio through the whole
  chain: transcript → summary, notes, action items, title → claims → verdicts. All artifacts are
  visible via `artifacts.list`, and `job.progress` / `artifact.added` were emitted.
- An `#[ignore]`d test against the real Ollama does the same from the real 30-min transcript at
  `%USERPROFILE%\.almond\eval\stt\meeting\transcript_gpu_zip30_turbo.json`. This exercises the
  long-meeting split/merge path for the first time. Report the time and the output quality.
- An offload test: two cores in one process (A without LLM, B with mock LLM), sync over the
  in-process or localhost transport; B runs A's jobs and A gets the artifacts.

## Module `library`: meetings, notes, search, calendar, mail, sync

**Owns:** `crates/almond-core/src/library.rs`, `crates/almond-store`, `crates/almond-sync`,
`crates/almond-calendar`, `crates/almond-mail`, UI Home / Devices / Settings screens where your
methods need it.

**Methods:**
- `meetings.list`, `meetings.get` (`MeetingDetail`: latest non-superseded artifact per kind first, then older versions), `meetings.create`, `meetings.update`, `meetings.delete`;
- `notes.get`, `notes.applyUpdate`;
- `search.query`;
- `calendar.sources`, `calendar.addGoogle`, `calendar.addIcs`, `calendar.importPhone` (`Unsupported` on desktop), `calendar.remove`, `calendar.upcoming`, `calendar.refresh`;
- `mail.context`;
- `sync.status`, `sync.now`, `sync.pairBegin`, `sync.pairAccept`, `sync.unpair`, `sync.addAddr`;
- `storage.usage`.

**Notifications:** `sync.progress`, `calendar.reminder`.

1. **Meetings, notes, search:** on the store. `notes_text` for processing (see the interface table).
2. **Calendar:**
   - Google OAuth uses the client from setting `google.oauth_client`; return a clear error telling
     the user to set it up (docs/google-setup.md) when it is missing.
   - Google sign-in opens the system browser through the existing loopback + PKCE flow.
   - ICS subscriptions.
   - A poll loop following `PollPolicy` while the app is open, writing `calendar.upsert` /
     `calendar.delete` events.
   - **E3:** apply `CalendarDelta.reset_calendars` by deleting those calendars' stored events
     before the upserts, and make the Google 410 resync path fill it.
3. **Reminders:**
   - Scheduler emits `calendar.reminder` `calendar.reminder_lead_min` minutes before (default 5).
   - Uses `ActiveDevicePolicy`: record this device's `last_active` from API activity, re-announce
     `device.info` at most every few minutes while in use, and read peers' values from the store.
4. **Mail:** `mail.context` for an event's attendees through the account's Gmail scope, when granted.
5. **Sync:**
   - Wire `SyncEngine` / `IrohTransport` from `core.node`.
   - `sync.now` syncs with all paired peers (or the given ones) and emits `sync.progress`.
   - Pairing via QR payload or 6 words.
   - `sync.addAddr { device, addr }`.
   - Auto-sync every 2 min while the app is open when setting `sync.auto_while_open` is true.
   - `SyncReport.status` distinguishes offline peers.
6. **`storage.usage`:** sizes of the db, blobs (audio vs other by mime) and the models dir.

**Done when:**
- Core integration tests cover:
  - meetings CRUD and notes round trip (Loro update in, text out);
  - search;
  - ICS calendar from a local file/HTTP fixture → `calendar.upcoming`;
  - a reminder fires;
  - two cores in one process pair and sync a meeting with notes over localhost.
- Google is tested against mocks only. Real sign-in needs the user.
