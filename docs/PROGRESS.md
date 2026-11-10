# Almond — Progress (updated 2026-11-10)

Plan: [PLAN.md](PLAN.md). **Phase 0** (contracts, tag `m0-contracts`) and **Phase 1** (7 parallel
workstreams, tag `m1-workstreams`) are done and merged into `main`. The batched contract changes are
on `main` too ([contract-requests/RESOLVED.md](contract-requests/RESOLVED.md)).
**Next: Phase 2, integration** — the parts work on their own but are not wired into the app yet;
the desktop UI still runs on its mock API.

## Phase 1 — what each part does (all on `main`)

| WS | Crate(s) | State | Verified |
|---|---|---|---|
| A Audio | `almond-audio` | Mic + WASAPI loopback (Windows), Pulse monitor (Linux), aligned 16 kHz pipeline, VAD, crash-safe chunked Ogg Opus recorder | Real capture on the PC; Linux not run locally. Echo cancellation (AEC3) not done |
| B STT | `almond-stt` | whisper.cpp (CPU / CUDA) + sherpa-onnx engines, model manager, language routing, live captions, diarization | 30-min VI/EN meeting: WER 7.7 %, LID 100 %, speakers 92 %; **5.6 min on the PC GPU** (RTF 0.19). See `eval/stt/RESULTS.md` |
| C LLM / pipeline | `almond-llm`, `almond-pipeline` | Ollama provider, VI/EN prompts, cited summary / enhanced notes / action items / title, replicated job runner (offload phone → PC), external JSON-RPC processors | Real Ollama: `gemma4:12b` coverage ≥ 0.97, citations ≥ 0.97 valid. See `eval/summary/` |
| D Store / sync | `almond-store`, `almond-sync` | SQLite event log + views, FTS5 with Vietnamese folding, BLAKE3 blobs, Loro notes, identity + pairing, iroh LAN transport, on-demand `sync_now` | Real 3-process iroh LAN test passes |
| E Calendar / mail | `almond-calendar`, `almond-mail` | Google OAuth (PKCE, multi-account), Google/ICS/phone calendars, reminders, Gmail context (read-only) | Mocked Google only — **no real sign-in yet** |
| F UI | `apps/desktop/src` | Home, meeting view (citations, verdicts, notes editor), devices & sync, settings, VI/EN | Runs on the mock API (`?mock=1`) |
| G Android | — | Not started | **Blocked**: needs Android NDK, cmdline-tools, JDK |
| H Verify | `almond-verify` | Claim extraction, rule-based verifiers, LLM judge, Datalog consistency, aggregation | Held-out set: P/R 1.00/1.00 with `gemma4:12b` (small, self-made set) |

Tests: 282 Rust + 18 UI pass; clippy `-D warnings` clean.

## Defaults chosen from the evals (PC)

- Summaries + fact-check judge: `gemma4:12b` (Fedora laptop: `qwen3.5:4b`, or leave jobs for the PC).
- Vietnamese STT: `sherpa:zipformer-vi-30m-int8` (PhoWhisper via whisper.cpp emits a junk first word
  and broken timestamps). English + language ID: `whisper:large-v3-turbo` on the GPU.

## Environment notes (dev PC)

- Ollama: the Windows one (0.40.13) on port 11434. The WSL Ollama is stopped and disabled.
- LLVM 23.1.5 and VS 2022 Build Tools installed; CUDA whisper builds with
  `crates\almond-stt\scripts\cargo-cuda.cmd` (target dir `%USERPROFILE%\.almond\t`).
- Models in `%USERPROFILE%\.almond\models`; eval data in `%USERPROFILE%\.almond\eval` (not committed).

## Phase 2 — to do

1. `almond-core` facade: own the store, sync, pipeline, audio, STT, LLM, calendar, verify; JSON-RPC.
2. Tauri commands per docs/API.md; switch the UI from the mock to the real API.
3. End to end on the PC: record a Meet call → live captions → stop → transcript → summary, notes,
   action items, fact-check → shown in the app.
4. Sync PC ↔ Fedora laptop; Google sign-in with the user's own OAuth client.
5. Speed: run diarization concurrently with transcription (it is ~80 % of post-meeting time).
6. Follow-ups listed in [RESOLVED.md](contract-requests/RESOLVED.md#phase-2-integration-work-that-these-unlock).

## Known issues

- All-day Google events are anchored at UTC midnight.
- Echo cancellation not done (needs meson, ninja, clang for webrtc AEC3).
- The HDMI audio endpoint drops out when the display sleeps.
- Linux and Android builds not verified locally.
- UI bundle has a 4.9 MB chunk (base64 fixtures) — split before release.
