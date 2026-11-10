# Contract request: `meetings.import` types into `almond-types`

**From:** ws/import (import an audio/video file). **Status:** open, nothing blocked.

`meetings.import` / `meetings.importCancel` / `import.progress` (docs/API.md) are implemented in
`crates/almond-core/src/capture/import.rs`, which also defines their param/payload structs
because `almond-types` was off limits:

| Rust (today, `almond_core::capture::import`) | Suggested home |
|---|---|
| `ImportParams { path, title?, language? }` | `almond_types::api::ImportParams` |
| `ImportCancelParams { meeting_id }` | `almond_types::api::ImportCancelParams` |
| `ImportProgress { meeting_id, path, file_name, stage, progress, processed_ms, duration_ms, decoder, error }` | `almond_types::api::ImportProgress` |
| `ImportStage` (`decoding`, `storing`, `done`, `failed`, `cancelled`) | `almond_types::api::ImportStage` |
| `MEETINGS_IMPORT`, `MEETINGS_IMPORT_CANCEL`, `IMPORT_PROGRESS` | `almond_types::api::methods` / `notifications` |

Why move them: the UI currently mirrors these by hand in `apps/desktop/src/api/types.ts`
(`ImportParams`, `ImportProgress`, `ImportStage`); in `almond-types` they would get `#[derive(TS)]`
and be generated into `apps/desktop/src/gen/` like the other API types.

Suggested shape: keep the field names and serde casing as they are (snake_case stages), add
`#[ts(export)]`, `#[ts(type = "number")]` on the `u64` fields. `language` could become
`Option<LangTag>` validated to `vi`/`en` in the handler. After the move, `import.rs` re-exports or
imports them, and `types.ts` imports from `../gen/`.
