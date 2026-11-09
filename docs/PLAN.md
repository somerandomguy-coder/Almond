# Almond — Parallel Build Plan

Design: [ARCHITECTURE.md](ARCHITECTURE.md) · Research: [RESEARCH.md](RESEARCH.md)

## How parallelism works here

1. **Phase 0 freezes the contracts** (types, traits, API, event format, workspace skeleton with a
   stub crate for every workstream). Done once, sequentially, on `main`.
2. **Phase 1 = 8 workstreams in 8 git worktrees**, each owning its own directories. Nobody edits
   another workstream's directories. Shared-contract changes go through a small PR to
   `crates/almond-types` that the others rebase on.
3. **Phase 2 integrates** (end-to-end flows), **Phase 3 hardens + packages.**

Every workstream codes against the **traits + mocks** from Phase 0, so none blocks on another.

### Git conventions
- Branch per workstream: `ws/<letter>-<name>` (e.g. `ws/b-stt`), worktree per branch.
- Small commits; **commit + push at every milestone** listed below; PR → `main` when a milestone is green.
- Tags: `m0-contracts`, `m1-<ws>`, `m2-e2e`, `v0.1.0`.
- CI (GitHub Actions): `cargo fmt --check`, `clippy -D warnings`, `cargo test` on Windows + Linux,
  `pnpm lint/test` for UI, Android build check (Phase 2+).

---

## Phase 0 — Contracts & skeleton (sequential, ~1 agent)

**Owner:** main session. **Branch:** `main`.

- [ ] Cargo workspace with all crates from ARCHITECTURE §3 as compiling stubs; pnpm workspace for UI.
- [ ] `almond-types`: `Id`, `DeviceId`, `Hlc`, `LangTag`, `Meeting`, `Artifact`, `Provenance`,
      `SpanRef`, artifact body structs + JSON Schemas (via `schemars`), `Event` envelope.
- [ ] Traits: `AudioSource`, `SttEngine`, `Diarizer`, `LlmProvider`, `Processor`, `Verifier`,
      `CalendarSource`, `MailSource`, `Transport`, `Store`, `BlobStore`, `SecretStore`.
- [ ] In-memory **mock** impl of every trait (`almond-types::mock` behind a `mock` feature).
- [ ] API spec: `docs/API.md` + Rust request/response types; TS types generated (`ts-rs` or `specta`)
      into `apps/desktop/ui/src/gen/`.
- [ ] Tauri 2 app that boots on Windows + Linux showing "hello" via one real command.
- [ ] CI workflow, `.editorconfig`, `rustfmt.toml`, `CONTRIBUTING.md` with ownership table below.
- **Milestone M0** → tag `m0-contracts`, push. Unblocks Phase 1.

---

## Phase 1 — Workstreams (all parallel)

| WS | Name | Owns (dirs) | Depends on |
|---|---|---|---|
| A | Audio capture | `crates/almond-audio` | M0 |
| B | STT + diarization + eval | `crates/almond-stt`, `eval/stt` | M0 (uses WAV files, not A) |
| C | LLM + pipeline + prompts | `crates/almond-llm`, `crates/almond-pipeline`, `eval/summary` | M0 |
| D | Store + sync | `crates/almond-store`, `crates/almond-sync`, `apps/almondd` | M0 |
| E | Calendar + mail | `crates/almond-calendar`, `crates/almond-mail` | M0 |
| F | Desktop UI | `apps/desktop/ui` | M0 (mock API) |
| G | Android | `plugins/tauri-plugin-almond-android`, `apps/desktop/src-tauri/gen/android` | M0 |
| H | Verification | `crates/almond-verify` | M0 (uses fixture artifacts) |

`almond-core` (the facade) is **not** owned in Phase 1 — it's wired in Phase 2 to avoid 8-way conflicts.

### WS-A Audio capture
Goal: produce synchronized 16 kHz mono PCM frames per channel (`mic`, `system`) + archive Opus files.
- A1 `AudioSource` impls: `cpal` mic (all OS), WASAPI loopback (Windows), PipeWire monitor (Linux, PulseAudio fallback), file/WAV import.
- A2 Resampling (`rubato`), VAD (Silero via ONNX or webrtc-vad), silence handling when loopback is quiet.
- A3 Two-channel recording with shared clock; Opus encoding to blob store interface; crash-safe (chunked files, resume after crash).
- A4 Optional AEC (WebRTC AEC3) when not on headphones; device hot-plug handling.
- Done when: a CLI example records 30 min of a Meet/Zoom call on Windows and Linux with both sides audible, no drift > 100 ms.

### WS-B STT + diarization + eval
- B1 `SttEngine` via `whisper-rs` (whisper.cpp; CUDA/Vulkan features); chunked pseudo-streaming with overlap → partial + final segments.
- B2 `SttEngine` via `sherpa-onnx` (Zipformer-vi streaming; also used on Android).
- B3 Model manager: registry file, download with SHA-256 check, GGML conversion notes/scripts for PhoWhisper; per-language routing table; language ID per segment.
- B4 Diarization via sherpa-onnx (pyannote-seg + 3D-Speaker embeddings); use `mic` vs `system` channel as a strong prior ("Me" vs others).
- B5 `eval/stt`: WER/CER harness on VIVOS / Common Voice vi + a few EN + user's own (git-ignored) recordings; report table in `eval/stt/RESULTS.md`. Compare PhoWhisper-{medium,large}, whisper-large-v3-turbo, Zipformer-vi, code-switching LoRA.
- Done when: transcript artifact for a 30-min VI/EN file with speakers, and RESULTS.md picks defaults per device class.

### WS-C LLM + pipeline + prompts
- C1 `LlmProvider` for Ollama (`/api/chat`, streaming, JSON-schema `format`, model digest in provenance, health/model list).
- C2 Prompt templates (vi/en) for: summary (with mandatory `cites`), enhance_notes (keep user text verbatim, add AI blocks), action_items, title. Map-reduce over long transcripts with token budgeting.
- C3 `almond-pipeline`: processor registry, DAG runner, job records (requested/claimed/lease/done/failed), capability matching, retries, rerun-with-supersede.
- C4 External processor host: JSON-RPC 2.0 over stdio + HTTP (`processor.describe`, `processor.run`) with a Python example processor.
- C5 `eval/summary`: ~10 VI + 10 EN transcripts with reference notes; metrics: citation validity (cited span exists & overlaps), coverage judged by rubric, language correctness; compare `qwen3:4b/8b/14b`, `gemma3:12b`.
- Done when: fixture transcript → summary/enhanced_notes/action_items artifacts with valid cites, in VI and EN.

### WS-D Store + sync
- D1 `almond-store`: SQLite schema + migrations, event log, materialized views, FTS5 with Vietnamese diacritic folding, blob store (BLAKE3).
- D2 HLC, version vectors, event apply (idempotent), Loro doc storage.
- D3 `almond-sync`: device identity (keychain), pairing (QR payload + 6-word code), iroh transport (pinned version) behind `Transport` trait, LAN-only mode, discovery (mDNS + manual address + hook for Android NSD).
- D4 Replication protocol, lazy blob fetch with per-device policy, backpressure, resume.
- D5 `apps/almondd`: headless daemon (systemd unit) using store + sync (+ pipeline later).
- Done when: 3 processes (Win, Linux, and a third on either) pair, converge after offline edits, and an integration test with random partitions passes.

### WS-E Calendar + mail
- E1 Google OAuth installed-app flow (loopback + PKCE), multi-account, tokens in `SecretStore`; setup guide `docs/google-setup.md` (own GCP project, "In production" status to avoid 7-day tokens, scopes).
- E2 `GoogleCalendar` source: calendar list, `events.list` + `syncToken`, poll, normalize (video link detection: Meet/Zoom/Teams), emit events into the log.
- E3 `IcsSubscription` source; `PhoneCalendar` source *interface* (data provided by WS-G plugin).
- E4 Reminder scheduler (configurable lead time; dedupe across devices so only the active device notifies).
- E5 Gmail: attendee thread context (readonly), follow-up draft creation; strictly optional per account.
- Done when: two Google accounts + an ICS feed show merged upcoming events with reminders.

### WS-F Desktop UI (React + TS + Vite + Tailwind + TipTap/Loro)
- F1 App shell, routing, i18n (vi/en), theming, settings scaffold; runs against the mock API.
- F2 Home: upcoming meetings (calendar), recent meetings, search.
- F3 Meeting view (the Granola core): notes editor during recording, live transcript panel, "Enhance notes" → AI blocks shown in a distinct style; summary with citation hover → jump to transcript/audio time.
- F4 Verdict display (colors/badges on bullets), artifact version switcher (compare reruns).
- F5 Settings: Google accounts (add/remove), phone calendar status, devices & pairing (QR display/scan), models (download/select per language), storage.
- Done when: full click-through on mocks, responsive enough to reuse on Android.

### WS-G Android
- G1 Tauri Android target builds; Kotlin plugin skeleton wired.
- G2 Foreground recording service (mic) with notification controls; feeds PCM to Rust via JNI; survives screen-off.
- G3 `CalendarContract` reader + "Import phone calendars" button (READ_CALENDAR) → `PhoneCalendar` events.
- G4 `NsdManager` discovery + `WifiManager.MulticastLock`; feed peers to `almond-sync`.
- G5 Notifications (meeting reminders with "Start notes" action); QR scanner for pairing.
- G6 Spike: `AudioPlaybackCapture` with Meet/Zoom/Teams — document which allow it.
- Done when: phone records an in-person meeting with live Zipformer-vi captions and pairs with the PC.

### WS-H Verification framework
- H1 `Verifier` trait, verdict schema, registry; runs as pipeline processors.
- H2 Claim extraction processor (LLM structured output) with `logical_form` slot.
- H3 Baseline verifiers: citation-exists, entity/number/date overlap rules, calendar consistency (attendees/dates), LLM/NLI grounding judge.
- H4 Fixtures with deliberately hallucinated summaries; precision/recall report in `crates/almond-verify/EVAL.md`.
- H5 Design note `docs/adr/neuro-symbolic.md`: logical form choice (triples vs Datalog), rule engine options (ascent/crepe vs Z3/Soufflé via external processor), how proofs map to `trace`.
- Done when: hallucinated fixtures get flagged with ≥ 80% recall at reasonable precision.

---

## Phase 2 — Integration (mostly sequential, 1–2 agents)

- I1 `almond-core` facade: wire store, sync, pipeline, audio, stt, llm, calendar, verify; JSON-RPC/WS server.
- I2 Tauri commands → core; swap UI from mock to real API.
- I3 E2E flow: calendar reminder → start notes → record (PC) → live transcript → stop → summary + enhanced notes + verdicts → appears on Linux and phone.
- I4 Offload flow: phone records → job requested → PC claims → artifacts sync back to phone.
- I5 Phone calendar → visible on PC; Google events → visible on phone.
- **Milestone M2** → tag `m2-e2e`.

## Phase 3 — Hardening & packaging (parallel again)

- Installers: Windows MSI/NSIS, Linux AppImage + .deb + `almondd` systemd unit, signed APK.
- Model download UX, first-run wizard (Ollama check, model pick by detected GPU/VRAM).
- Backup/export (Markdown/JSON per meeting), data deletion, encryption-at-rest option.
- Chinese: add zh STT routing + prompts + UI strings.
- MCP server over the local API.
- **Release** `v0.1.0`.

---

## Agent spawning cheat sheet (for Phase 1)

Each agent gets: its WS section above, ARCHITECTURE.md, the ownership rule ("only edit your dirs; if
you need a contract change, write it up in `docs/contract-requests/<ws>.md` and stop"), the milestone
list, and "commit + push to `ws/<x>` at each milestone, open a PR when the WS is done".

Suggested waves if you don't want 8 at once:
- **Wave 1 (core value):** B, C, D, F
- **Wave 2:** A, E, H
- **Wave 3:** G (benefits from B's sherpa-onnx work and D's sync)

## Open questions for you

1. PC GPU model / VRAM, and Linux box specs? (Decides default STT + LLM sizes and who claims jobs.)
2. Is the Linux box always on? (Then it's the natural `almondd` sync/compute hub.)
3. Typical meeting: online (Meet/Zoom/Teams on PC) or in-person (phone on the table)?
4. Android version / phone model? (Playback capture and background limits vary.)
5. Do you want Gmail *drafts*, or only reading context? (Drafts need a broader restricted scope.)
