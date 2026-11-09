# Almond — Progress (paused 2026-11-09)

Plan: [PLAN.md](PLAN.md). Phase 0 is done (tag `m0-contracts`). Phase 1 was running as 7 parallel
agents; they were **stopped** at the user's request. Every workstream's work — finished milestones
*and* in-progress files — is pushed to its `ws/*` branch. Commits titled `wip(...)` were saved
mid-task and may not build or pass tests yet.

## Status by workstream

| WS | Branch | Done (committed by the agent) | Unfinished (in the `wip` commit) | Verified |
|---|---|---|---|---|
| A Audio | `ws/a-audio` | A1 cpal / WASAPI loopback / Pulse-monitor capture, push + WAV sources; A2 clock-aligned resampling, VAD; A3 crash-safe chunked Ogg Opus / WAV recorder, merge + recovery, multi-channel `CaptureSession` | A4 echo cancellation (`echo.rs`), `record` example | Not checked by the integrator; no real-hardware run confirmed |
| B STT | `ws/b-stt` | — (nothing committed before the stop) | whisper + sherpa engines, model registry (`models.json`), language routing, streaming, metrics, `eval/stt` harness | Not checked; no WER numbers yet |
| C LLM / pipeline | `ws/c-llm` | C1 Ollama provider; C2 vi/en prompt templates + cited tasks; C3 registry, DAG planner, replicated job runner; C4 external JSON-RPC processors (stdio + HTTP) | C5 `eval/summary` dataset (partial), no RESULTS.md yet | Not checked by the integrator |
| D Store / sync | `ws/d-sync` | D1+D2 SQLite event log, LWW views, FTS5 Vietnamese folding, BLAKE3 blobs, Loro notes; D3–D5 identity, pairing, iroh LAN transport, replication protocol, `sync_now` | Agent had a long test/build still running (likely the real-iroh multi-process test) — unknown result | Not checked by the integrator |
| E Calendar / mail | `ws/e-calendar` | **Complete** E1–E5 + `docs/google-setup.md` (EN + VI). PR [#1](https://github.com/somerandomguy-coder/Almond/pull/1) open | — | ✅ Scope check clean (only its own dirs); 38 tests pass (2 need a real Google account, `#[ignore]`). All Google traffic is mocked; no real sign-in yet |
| F UI | `ws/f-ui` | — (nothing committed before the stop) | App shell, mock API, screens (home, meeting, sync, settings), citations, notes editor, i18n, vitest tests | Not checked; agent was about to start the dev server |
| G Android | — | Not started | — | Blocked: no Java / Android SDK / NDK on the PC |
| H Verify | `ws/h-verify` | H1–H3 verifier registry, processor adapters, aggregation, claim extraction, baseline verifiers | H4 parser fixes, hallucination fixtures (`tests/hallucinations.rs`, `eval/`), examples; H5 ADR not written | Not checked by the integrator |

## Open contract requests (to batch into `almond-types` before integration)

- `docs/contract-requests/ws-e.md` (on `ws/e-calendar`): `keys::GOOGLE_ACCOUNTS`; `calendar.addGoogle { gmail?, login_hint? }`;
  `CalendarDelta::reset_calendars`; settings entry for the user's Google OAuth client id/secret;
  `DeviceInfo::last_active`.
- Other workstreams may have added `docs/contract-requests/ws-*.md` on their branches — check when resuming.

## Known issues to fix during integration

- WS-E: all-day Google events are anchored at UTC midnight; deleted events can linger after a 410 full
  resync until `reset_calendars` exists.
- Mock `MemStore` advances the version vector without checking for gaps (the real store must not).

## How to resume

1. For each `wip` branch: check out the worktree (or the branch), run that crate's fmt/clippy/tests, fix,
   then finish the remaining milestones listed in PLAN.md. Each agent can be restarted with the same
   brief plus: "continue from branch `ws/<x>`; the last commit is WIP from a paused session".
2. When each workstream is green, open its PR. Review it for ownership (only its own dirs) and run its tests.
3. Batch the contract requests into one `almond-types` PR on `main`, then rebase the `ws/*` branches.
4. Phase 2 integration (`almond-core` + Tauri commands), per PLAN.md.
5. Install Android Studio (SDK + NDK, JDK) to unblock WS-G.

Local worktrees live in `.claude/worktrees/agent-*` (one per workstream) and can be reused or removed —
everything is on GitHub.
