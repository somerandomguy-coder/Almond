# Contract requests — module `processing` (Phase 2)

## P1. Runtime capabilities in the announced `DeviceInfo`

`processing` detects capabilities at runtime (Ollama models → `llm`, `llm:<model>`,
`embed:<model>`; `nvidia-smi` → `gpu`; installed speech models → `stt:<lang>`, `diarize`) and
re-detects every minute. The job runner uses them directly. It also appends a `device.info` event
with the detected list when it changes, so peers see what this device can run.

But `core.device()` (`SyncNode::device`) still holds the static `CoreConfig::capabilities`. If
`library` re-announces `device.info` from `core.device()` (for `last_active`), it overwrites the
detected list.

**Workaround in this branch:** none, beyond announcing again on the next change.

**Request:** when building a `DeviceInfo` to announce, `library` takes its capabilities from
`core.processing.capabilities().capabilities`. Alternatively, `Shared` gets one mutable "current
device info" that both modules update.

## P2. `methods::MODELS_SLOTS`

`models.slots` is in docs/API.md but `almond_types::api::methods` has no constant for it. Add
`pub const MODELS_SLOTS: &str = "models.slots";`. The UI type (`api/types.ts`) was added in this
branch.

## P3. Speech slots in `models.slots`

`models.slots` returns the user's overrides plus defaults for `llm_summary`, `llm_verify` and
`embed`. The defaults for `stt` and `stt_live` come from `almond_stt::routing`, which `capture`
owns.

**Request:** `capture` exposes `pub fn speech_slots(core: &Core) -> Vec<ModelSlot>` (defaults
for this device class), and `processing` merges it.

## P4. `models.download` result type

`models.download` returns a `Job`. A download isn't a pipeline job, so the returned job is local
bookkeeping only: processor `models.download`, empty `meeting_id`, `params.id`. It isn't
written to the log. Progress goes out as `job.progress` (the UI's Settings screen listens for
this) and, from `capture`, as `models.downloadProgress`.

**Request (later, low priority):** return `ModelDownloadProgress` instead.

## P5. Device identity of cores opened in tests (Windows)

`Core::open` uses `default_secret_store(data_dir)`. On Windows and macOS this is the OS keyring
(service `almond`), with the file store only as a fallback. So every core opened on this PC gets
the **same device key**: `open_core` in tests, two cores in one process, and the real app.

- Two cores can't sync with each other: "Connecting to ourself is not supported".
- Test cores author events under the real app's device id.

**Workaround in this branch:** the offload test (`tests/processing.rs`) runs the PC side as a bare
`SyncNode` with a `FileSecretStore` and a `JobRunner`.

**Request:**
- `CoreConfig` gets `secrets: Option<Arc<dyn SecretStore>>`, passed to `NodeConfig::secrets`;
- `tests/common/mod.rs::open_core` uses a `FileSecretStore` in the temp dir (or does so whenever
  `network == false`).
