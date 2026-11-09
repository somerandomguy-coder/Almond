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

## Methods

| Method | Params | Result |
|---|---|---|
| `meetings.list` | `MeetingQuery` | `Meeting[]` |
| `meetings.get` | `{ id }` | `MeetingDetail` |
| `meetings.create` | `CreateMeetingParams` | `Meeting` |
| `meetings.update` | `UpdateMeetingParams` | `Meeting` |
| `meetings.delete` | `{ id }` | `null` |
| `recording.start` | `StartRecordingParams` | `RecordingStatus` |
| `recording.stop` | `{}` | `RecordingStatus` (pipeline jobs are queued automatically) |
| `recording.status` | `{}` | `RecordingStatus` |
| `audio.devices` | `{}` | `AudioDeviceInfo[]` |
| `artifacts.list` | `{ meeting_id, kind? }` | `Artifact[]` |
| `artifacts.get` | `{ id }` | `Artifact` |
| `artifacts.rerun` | `RerunParams` | `Job` |
| `notes.get` | `{ doc }` | `NotesSnapshot` |
| `notes.applyUpdate` | `{ doc, update_b64 }` | `null` |
| `search.query` | `{ text, limit? }` | `SearchHit[]` |
| `calendar.sources` | `{}` | `CalendarSourcesResult` |
| `calendar.addGoogle` | `{}` (opens browser OAuth) | `CalendarSourceInfo` |
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
| `sync.status` | `{}` | `SyncStatus` |
| `sync.now` | `{ peers? }` | `SyncReport[]` |
| `sync.pairBegin` | `{}` | `PairBeginResult` |
| `sync.pairAccept` | `{ qr_payload? , words? }` | `PeerInfo` |
| `sync.unpair` | `{ device }` | `null` |
| `sync.addAddr` | `{ addr }` | `null` |
| `models.list` | `{}` | `ModelEntry[]` |
| `models.download` | `{ id }` | `Job` |
| `models.select` | `ModelSlot` | `null` |
| `settings.get` / `settings.set` | `{ key }` / `{ key, value }` | JSON |

## Notifications (server → client)

| Name | Payload |
|---|---|
| `recording.status` | `RecordingStatus` (≈ every 250 ms while recording) |
| `transcript.update` | `TranscriptUpdate` (live captions; `is_final=false` may be replaced) |
| `job.progress` | `JobProgress` |
| `artifact.added` | `Artifact` (from this or any synced device) |
| `sync.progress` | `SyncProgress` |
| `calendar.reminder` | `MeetingReminder` |

## External processors (JSON-RPC over stdio or HTTP)

An external processor is any program implementing:

- `processor.describe` → `ProcessorDescriptor[]`
- `processor.run` `{ processor, job_id, meeting: Meeting, params, inputs: Artifact[] }` → `ArtifactDraft[]`
- optional notification `processor.progress` `{ job_id, progress, message }` (processor → host)

This is the intended path for research verifiers (Python + Z3/Prolog/NLI models).

## Errors

JSON-RPC errors use standard codes plus: `-32004` not found, `-32001` auth required (e.g. Google
re-consent), `-32005` unsupported on this platform.
