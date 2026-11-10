# Contract requests — resolution (2026-11-10, after merging Phase 1)

The per-workstream files (`ws-a.md` … `ws-h.md`) are kept as the record of what was asked.
This file says what happened to each request.

## Done in `almond-types` (with every caller updated)

| Req | Change |
|---|---|
| A1 | Doc: all streams from one `AudioBackend` share a capture epoch until the backend's clock is reset |
| A2 | `AudioFrame::synthesized` (audio pipeline sets it for all-zero frames) |
| A3 | Doc: `AudioTrack::sample_rate` is the encoded source rate |
| B3 | Doc: `SttUpdate` partial/final protocol |
| B4 | `SttModelInfo::role` (`ModelRole`), `ModelDownloadProgress` + notification `models.downloadProgress` |
| C1 | `Store::job_records` → `JobRecord { job, status, leases, outcomes }`; `JobStatus::derive` (any `Done` wins, then latest outcome, then live claim, then pending) used by MemStore and SqliteStore; `almond-pipeline::job_states` no longer scans the log |
| D1 | `Transport::add_addr(device: DeviceId, addr)` — device id required; `sync.addAddr { device, addr }` |
| D2 | `Store::known_blobs` → `BlobInfo` (moved from almond-store into almond-types) |
| D4 | Store semantics documented on the `Store` trait (tombstones, job status, notes snapshots, gaps) |
| D5 | `SyncReport::status: SyncOutcome` (`ok` / `offline` / `failed`) |
| E1 | `secrets::keys::GOOGLE_ACCOUNTS` (almond-calendar's `ACCOUNT_INDEX_KEY` now points at it) |
| E3 | `CalendarDelta::reset_calendars` (field added; Google 410 path still to fill it, see below) |
| E5 | `DeviceInfo::last_active` |
| F6 | Doc: claim `source.segment` = flat bullet index across summary sections |
| H1 | `VerifyContext::params` |
| H2 | `Meeting::tz` (IANA zone) |
| H4 | `Verdicts::errors: Vec<VerifierError>` |
| H5 | `almond_types::params_hash(template_id, &params)` |

## Done in docs / CI

| Req | Change |
|---|---|
| B1 | CI runs `--all-features` everywhere except almond-stt, which is checked with default features |
| B2 | Documented in `crates/almond-stt/scripts/`; LLVM is now installed on the dev PC and `cargo-cuda.cmd` uses it |
| C2 | `processor.cancel` documented in docs/API.md |
| C4, F3 | `enhance_notes.llm` gets the notes' plain text as `params.user_notes`, filled in by the core |
| E2 | `calendar.addGoogle { gmail?, login_hint? }` documented |
| E4 | Settings key `google.oauth_client` documented |
| F1 | Tauri conventions documented at the top of docs/API.md |
| F2, F4, F8, F9 | `audio.url`, `models.slots`, `storage.usage`, `app.info` added to the method table |
| F5 | Setting `sync.auto_while_open`; the core runs the periodic sync |
| F7 | Settings keys listed in docs/API.md |

## Phase 2 (integration) work that these unlock

- Core: emit `blob.available` for recordings (D3); fill `VerifyContext.calendar_event` and `params.tz` from the meeting (H3, H1); read the notes doc for `enhance_notes.llm` (C4).
- almond-calendar: set `reset_calendars` on a Google 410 full resync, and have the core drop those calendars' stored events before applying the delta (E3).
- almond-verify: report failing verifiers in `Verdicts::errors` instead of dropping them (H4); read `tz` from `ctx.params` / `meeting.tz` (H1).
- STT: skip `synthesized` frames (A2).
- Optional: make `SyncEngine` generic over `dyn Store` now that `known_blobs` is on the trait (D2).

## Not done

- C3 `talk_time` artifact kind: unknown kinds already sync as opaque JSON; add a type when the UI shows it.
