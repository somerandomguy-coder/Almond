# WS-F (UI) contract requests

The UI builds against `apps/desktop/src/api/types.ts` (`AlmondApi`), which mirrors docs/API.md.
These are the gaps hit while building the screens against the mock. All are **additive**.

## 1. Tauri transport conventions (integrator, not almond-types)

- **What:** confirm the UI's assumptions in `src/api/tauri.ts`:
  - every method is a command named with `.` → `_` (`notes.applyUpdate` → `notes_applyUpdate`),
    invoked as `invoke(cmd, { params })` — i.e. each command takes one argument named `params`
    whose type is the method's params struct;
  - notifications are Tauri events with the same `.` → `_` mapping (`transcript.update` →
    `transcript_update`), because Tauri event names may not contain `.`;
  - errors are rejected with `{ code, message }` (JSON-RPC codes from API.md).
- **Why:** the UI has to agree with `apps/desktop/src-tauri/src` on one convention.

## 2. Audio playback URL

- **What:** `audio.url { meeting_id, channel? } -> { url: string | null }` (Tauri asset-protocol or
  `http://127.0.0.1` URL for the blob, `null` if the blob isn't on this device — e.g. a phone that
  only synced text).
- **Why:** citation chips jump the transcript *and* the audio playhead; today the UI has a scrubber
  but no way to actually play `AudioTrack.blob`.

## 3. Enhance notes input

- **What:** document that `enhance_notes.*` processors read the meeting's Loro notes doc
  (`Meeting.notes_doc`) themselves. Optionally add `RerunParams.notes_doc: Option<Id>`.
- **Why:** `RerunParams.inputs` are artifact ids; the user's notes are a Loro doc, not an artifact.
  The UI currently sends `params: { notes_doc, user_notes /* plain-text snapshot */ }` as a stopgap.
- Processor name the UI uses: `enhance_notes.llm` (and `summarize.llm` for summary reruns with
  `params.model`). Please confirm or tell me the real names (or expose them via
  `pipeline.processors` with a `produces` the UI can look up — it can switch to that).

## 4. Model slot getter

- **What:** `models.slots {} -> ModelSlot[]` (current selection per task/language).
- **Why:** there is `models.select` but no way to read the selection back. The UI mirrors it in
  `settings` key `models.slots` for now.

## 5. Auto-sync toggle

- **What:** `sync.setAuto { enabled: bool } -> null`, or document that the core reads
  `settings` key `sync.auto_while_open` (bool) and reflects it in `SyncStatus.auto_sync_while_open`.
- **Why:** `SyncStatus.auto_sync_while_open` is readable but nothing sets it. The UI writes the
  settings key and (for now) runs the periodic `sync.now` itself every 2 min while open.

## 6. Claim → summary bullet mapping

- **What:** document (in `Claim.source` docs) that for summaries `source.artifact = summary.id` and
  `source.segment` = **flat bullet index across all sections** (section 0 bullets first, …).
- **Why:** the UI maps verdicts onto bullets with exactly that rule (`lib/artifacts.ts:verdictsByBullet`).
  If WS-H prefers `(section, bullet)`, a `chars`-style pair or a new field would be needed.

## 7. Settings keys used by the UI (local-only, never synced)

`ui.language` ("vi" | "en"), `ui.theme` ("system" | "light" | "dark"),
`calendar.reminder_lead_min` (number, read by the reminder scheduler, WS-E),
`sync.auto_while_open` (bool), `models.slots` (stopgap, see 4).

## 8. Storage usage (F5 "storage")

- **What:** `storage.usage {} -> { db_bytes, blobs_bytes, models_bytes, audio_bytes }`.
- **Why:** F5 asks for a storage section; the UI shows a placeholder until this exists.

## 9. `platform` for UI gating

- **What:** keep `app_info` (or add `app.info`) returning `platform` ("windows" | "linux" | "android").
- **Why:** "Import phone calendars" is shown only on Android and recording channels differ
  (phone = mic only). The UI calls `app_info` once.
