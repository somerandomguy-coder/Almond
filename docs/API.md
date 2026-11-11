# Almond Local API (v1)

One API surface, three transports:

| Transport | Who uses it | Naming |
|---|---|---|
| Tauri commands + events | the UI | method `meetings.list` → command `meetings_list` |
| JSON-RPC 2.0 over WebSocket `ws://127.0.0.1:<port>/rpc` + bearer token | CLI, scripts, external processors | method names as below |
| MCP server (later) | AI agents | tools mirror read-only methods + `artifacts.rerun` |

Types are defined in Rust (`crates/almond-types/src/api.rs` and friends) and exported to TypeScript
in `apps/desktop/src/gen/` by `cargo test -p almond-types` (ts-rs). **Never hand-edit `src/gen`.**

The UI never touches storage directly — it only calls these methods.

**Tauri conventions** (`apps/desktop/src/api/tauri.ts` ↔ `apps/desktop/src-tauri/src`):
- every method is a command named with `.` → `_` (`notes.applyUpdate` → `notes_applyUpdate`),
  invoked as `invoke(cmd, { params })`: one argument named `params` holding the method's params;
- notifications are Tauri events with the same mapping (`transcript.update` → `transcript_update`),
  because Tauri event names may not contain `.`;
- errors reject with `{ code, message }` using the codes under [Errors](#errors).

## Methods

| Method | Params | Result |
|---|---|---|
| `meetings.list` | `MeetingQuery` | `Meeting[]` |
| `meetings.get` | `{ id }` | `MeetingDetail` |
| `meetings.create` | `CreateMeetingParams` | `Meeting` |
| `meetings.update` | `UpdateMeetingParams` | `Meeting` |
| `meetings.delete` | `{ id }` | `null` |
| `meetings.import` | `{ path, title?, language?: "vi" \| "en" }` — an audio/video file on this device (WAV, MP3, M4A/AAC, FLAC, OGG Opus/Vorbis, MP4/MOV, MKV/WebM decoded in-process; other formats through `ffmpeg` on `PATH`, else `-32005` naming the format) | `Meeting` (`kind: imported`, one `mixed` Opus track, `blob.available`; resolves when stored, then processing runs `transcribe.local` → … as for recordings; `ended_at` = file modification time, `started_at` = `ended_at` − duration; `title` defaults to the file name). Progress: `import.progress` |
| `meetings.importCancel` | `{ meeting_id }` (from `import.progress`) | `boolean` (an import was running; it then fails and stores nothing) |
| `recording.start` | `StartRecordingParams` | `RecordingStatus` |
| `recording.stop` | `{}` | `RecordingStatus` (pipeline jobs are queued automatically) |
| `recording.status` | `{}` | `RecordingStatus` |
| `audio.devices` | `{}` | `AudioDeviceInfo[]` |
| `audio.url` | `{ meeting_id, channel? }` | `{ url: string \| null }` — playable URL for the meeting's audio track (Tauri asset protocol); `null` if the blob is not on this device (e.g. a phone that synced text only) |
| `artifacts.list` | `{ meeting_id, kind? }` | `Artifact[]` |
| `artifacts.get` | `{ id }` | `Artifact` |
| `artifacts.rerun` | `RerunParams` | `Job` (for `enhance_notes.llm` the core reads the meeting's notes doc and passes its plain text as `params.user_notes`) |
| `notes.get` | `{ doc }` | `NotesSnapshot` |
| `notes.applyUpdate` | `{ doc, update_b64 }` | `null` |
| `search.query` | `{ text, limit? }` | `SearchHit[]` |
| `calendar.sources` | `{}` | `CalendarSourcesResult` |
| `calendar.addGoogle` | `{ gmail?: boolean, login_hint?: string }` (opens browser OAuth; asks for Gmail read-only only when `gmail` is true; re-run with `login_hint` to add/remove Gmail for an account) | `CalendarSourceInfo` |
| `calendar.addIcs` | `{ url, label? }` | `CalendarSourceInfo` |
| `calendar.importPhone` | `{}` (Android only; asks READ_CALENDAR) | `CalendarSourceInfo` |
| `calendar.remove` | `{ source }` | `null` |
| `calendar.upcoming` | `{ hours? }` (default 24) | `UpcomingEvent[]` |
| `calendar.refresh` | `{}` | `null` |
| `mail.context` | `{ event: EventRef }` | `MailThreadSummary[]` |
| `actions.list` | `{}` | `ActionDescriptor[]` |
| `actions.propose` | `{ meeting_id }` | `ActionProposals` |
| `actions.execute` | `{ proposal: ActionProposal }` | `ActionResult` |
| `pipeline.processors` | `{}` | `ProcessorDescriptor[]` |
| `pipeline.registerExternal` | `{ command? , url? }` | `ProcessorDescriptor[]` |
| `jobs.list` | `{ status? }` | `JobProgress[]` |
| `jobs.cancel` | `{ id }` | `null` |
| `verify.run` | `{ artifact_id, verifiers? }` | `Job` |
| `verify.verifiers` | `{}` | `VerifierDescriptor[]` |
| `sync.status` | `{}` | `SyncStatus` (`auto_sync_while_open` mirrors setting `sync.auto_while_open`) |
| `sync.now` | `{ peers? }` | `SyncReport[]` |
| `sync.pairBegin` | `{}` | `PairBeginResult` |
| `sync.pairAccept` | `{ qr_payload? , words? }` | `PeerInfo` |
| `sync.unpair` | `{ device }` | `null` |
| `sync.addAddr` | `{ device, addr }` (device id required: iroh dials by key; shown under Settings → Devices and in the pairing QR) | `null` |
| `models.list` | `{}` | `ModelEntry[]` |
| `models.download` | `{ id }` | `Job` |
| `models.select` | `ModelSlot` | `null` |
| `models.slots` | `{}` | `ModelSlot[]` (current selection per task/language) |
| `storage.usage` | `{}` | `{ db_bytes, blobs_bytes, models_bytes, audio_bytes }` |
| `app.info` | `{}` | `{ name, version, api_version, platform: "windows" \| "linux" \| "android" }` (Tauri command `app_info`) |
| `settings.get` / `settings.set` | `{ key }` / `{ key, value }` | JSON |

### Recipes (cookbook)

Recipes are synced records (collection `recipe`). Built-ins have ids `builtin:teach_back`,
`builtin:forgot_to_ask`, `builtin:communication`, `builtin:decisions`, `builtin:followup_email`;
their text ships with the app, and a record with a built-in's id only stores the user's `hidden` /
`auto_run` for it. `RecipeInfo` = `Recipe` + `hidden: boolean`.

| Method | Params | Result |
|---|---|---|
| `recipes.list` | `{}` | `RecipeInfo[]` (built-ins first, in a fixed order, then the user's by name; hidden ones included) |
| `recipes.save` | `{ id?, name?, description?, icon?, prompt?, auto_run?, hidden? }` — no `id`: create (name + prompt required); with `id`: change only the given fields (`icon: ""` clears it). For a built-in, `auto_run` / `hidden` are stored as preferences, and a change to its name, description, icon or prompt saves a **copy** with a new id instead | `RecipeInfo` (the copy when one was made) |
| `recipes.delete` | `{ id }` | `null`; built-ins: `-32602` ("hide them instead") |
| `recipes.run` | `{ recipe_id, meeting_id, extra?, lang? }` (`lang`: output language, default setting `ui.language`, else the meeting's dominant language) | `Job` (`recipe.llm` on the meeting's latest transcript; the recipe is snapshotted into `params.recipe`). `-32602` when the meeting has no transcript yet |
| `recipes.outputs` | `{ meeting_id }` | `Artifact[]`: live `recipe_output` artifacts, latest first, one per recipe |

`recipe.llm` (processor; `requires: ["llm"]`, so a phone or laptop leaves it for the PC like any
LLM job) writes a `recipe_output` artifact: `{ recipe_id, recipe_name, markdown, citations }`, where
`[n]` in `markdown` is `citations[n - 1]` (a transcript `SpanRef`, validated). Its
`provenance.processor` is `recipe.llm/<recipe_id>`, so a rerun of a recipe supersedes only that
recipe's previous output. "How did I communicate?" starts with a statistics table computed in code
(talk time, share, turns, longest turn, words/min, interruptions, filler words). Recipes with
`auto_run` are requested once for each new transcript, by the device that wrote it.

### People (feature `people`, `crates/almond-core/src/people/`)

People are synced records (`Person`, collection `"person"`); who each transcript speaker is
lives in `speaker_labels` artifacts (`SpeakerLabels`, latest non-superseded per meeting wins).
Responses carry voice prints **without vectors** (`vector: []`).

| Method | Params | Result |
|---|---|---|
| `people.list` | `{}` | `{ person: Person, meeting_count, talk_time_s, last_met: number \| null }[]` (me first, then most recently met) |
| `people.get` | `{ id }` | `{ person, meetings: { meeting_id, title, started_at, speakers: string[], talk_time_s, source: LabelSource \| null, attendee: boolean }[], talk_time_s, last_met }` — meetings by speaker label and by calendar attendee email, newest first |
| `people.save` | `{ id?, name, emails?, aliases?, notes?, is_self?, voiceprints? }` — no `id` creates; omitted fields are kept; `voiceprints` = the prints to keep (matched by `meeting_id` + `speaker` + `created_at`), so leaving one out deletes it; `is_self: true` clears it on everyone else | `Person` |
| `people.delete` | `{ id }` | `null` (also removes the person from every meeting's labels) |
| `people.merge` | `{ keep, remove }` | `Person` (`keep` gets `remove`'s name as an alias, emails, notes, voice prints and speaker labels; `remove` is deleted) |
| `people.labelSpeaker` | `{ meeting_id, speaker, person_id? \| name? \| me?: true }` — exactly one; `name` reuses a person with that name/alias or creates one; `me` = the `is_self` person (created if missing) | `SpeakerLabels` (`source: user`; adds the speaker's voice print in this meeting to the person when the audio is on this device and the build has the `sherpa` engine) |
| `people.unlabelSpeaker` | `{ meeting_id, speaker }` | `SpeakerLabels` (a user label's voice print is removed from the person) |
| `people.suggestions` | `{ meeting_id }` | `SpeakerLabel[]` — what the core would suggest now for speakers without a user label (voice, one unambiguous calendar attendee, `me`); nothing is written |

**Automatic labels:** when this device adds a transcript, the core writes a `speaker_labels`
artifact once: `me` (mic channel) → the `is_self` person; remote speakers → the person whose
voice prints match best (`source: voice`, `confidence` = cosine similarity, at least setting
`people.voice_threshold`, default 0.5, and 0.05 ahead of the runner-up; one speaker per person);
if exactly one non-self calendar attendee and one remote speaker are left, that attendee by
email (`source: calendar`; the person is created from the invite when unknown). User labels are
never overwritten. Calibration: docs/people-voice.md.

**Settings keys** (local to the device, never synced): `ui.language` (`"vi" | "en"`), `ui.theme`
(`"system" | "light" | "dark"`), `calendar.reminder_lead_min` (number), `sync.auto_while_open`
(bool; the core syncs every 2 min with paired peers while the app is open), `google.oauth_client`
(`{ client_id, client_secret }`, the user's own Desktop OAuth client; see docs/google-setup.md),
`llm.ollama_url` (default `http://127.0.0.1:11434`).

## Notifications (server → client)

| Name | Payload |
|---|---|
| `recording.status` | `RecordingStatus` (≈ every 250 ms while recording) |
| `transcript.update` | `TranscriptUpdate` (live captions; `is_final=false` may be replaced) |
| `job.progress` | `JobProgress` |
| `artifact.added` | `Artifact` (from this or any synced device) |
| `sync.progress` | `SyncProgress` |
| `calendar.reminder` | `MeetingReminder` |
| `models.downloadProgress` | `ModelDownloadProgress` |
| `import.progress` | `{ meeting_id, path, file_name, stage: "decoding" \| "storing" \| "done" \| "failed" \| "cancelled", progress: number \| null, processed_ms, duration_ms: number \| null, decoder: "symphonia" \| "opus" \| "ffmpeg" \| null, error: string \| null }` (≈ every 250 ms during `meetings.import`) |

## Ask Almond (chat)

Chat with the local LLM about the user's meetings, notes, calendar and people
(`crates/almond-core/src/chat/`). A thread is a synced record (collection `chat`, type
`ChatThread`); the whole thread is one record, last writer wins.

| Method | Params | Result |
|---|---|---|
| `chat.ask` | `{ thread_id?, scope?: ChatScope, message, lang?: "vi" \| "en" }`: no `thread_id`, or one that doesn't exist yet (ASCII letters, digits, `-`, `_`, ≤ 64 chars), starts a thread with that id and `scope` (default `{ type: "all" }`); an existing thread keeps its scope. `lang` defaults to the question's language, else `ui.language` | `{ thread_id, entry: ChatEntry }`: the assistant's answer, resolved when it finished streaming and the thread was saved. `entry.text` is Markdown with `[n]` markers; `entry.sources` are the cited `ChatSource`s (`n` = marker; `meeting_id`, `span` for transcript lines, `event` for calendar events). Markers without a source are removed. Errors: `-32602` empty/too long (4000 chars) message, bad id, scope mismatch, or the thread is already answering; `-32004` unknown meeting/person in `scope`; `-32603` with a message starting "Ollama isn't running at …" / "Ask Almond can't reach Ollama at …" when there is no LLM |
| `chat.threads` | `{ scope?: ChatScope, limit? }` | `ChatThread[]`, most recently updated first |
| `chat.get` | `{ thread_id }` | `ChatThread` (`-32004` if missing) |
| `chat.delete` | `{ thread_id }` | `null` (a synced delete; stops an answer in progress) |
| `chat.cancel` | `{ thread_id }` | `boolean`: an answer was streaming; it stops and what was written so far is saved |

Notification `chat.delta` `{ thread_id, entry_id, text }` while an answer streams: `text` is the
next piece (append it); `entry_id` is the id the final `entry` will have.

**Context per scope** (numbered sources, ≈10k tokens; `num_ctx` follows the prompt size):
- `all`: calendar events from today through 7 days ahead (attendees, linked meeting), full-text
  hits for the question's keywords (diacritic-folded; transcript lines with their neighbours,
  summaries), the user's notes and recipe outputs that mention them, digests of the most recent
  meetings, people mentioned by name, and the people catalog with "last met".
- `meeting`: summary, action items, the user's notes, AI notes (without a summary), recipe outputs,
  and the transcript in chunks (when too long: chunks matching the question first, the rest spread
  evenly). Speaker labels show people's names.
- `person`: the person (emails, aliases, notes, their meetings), upcoming events with them (by
  attendee email), and per meeting a digest plus what they said (`speaker_labels`).

**Model:** slot `llm_chat` when the user selected one (`models.select { task: "llm_chat", model }`),
else `llm_summary` (default `gemma4:12b` on the PC). Instructions are English; the answer is in
Vietnamese or English as above.

## External processors (JSON-RPC over stdio or HTTP)

An external processor is any program implementing:

- `processor.describe` → `ProcessorDescriptor[]`
- `processor.run` `{ processor, job_id, meeting: Meeting, params, inputs: Artifact[] }` → `ArtifactDraft[]`
- optional notification `processor.progress` `{ job_id, progress, message }` (processor → host)
- notification `processor.cancel` `{ job_id }` (host → processor): stop the job and return an error
  (the host gives up on it after a grace period)

This is the intended path for research verifiers (Python + Z3/Prolog/NLI models).

## Errors

JSON-RPC errors use standard codes plus: `-32004` not found, `-32001` auth required (e.g. Google
re-consent), `-32005` unsupported on this platform.
