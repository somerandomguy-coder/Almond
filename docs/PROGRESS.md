# Almond — Progress (updated 2026-11-10)

Plan: [PLAN.md](PLAN.md). **Phase 0** (contracts, tag `m0-contracts`) and **Phase 1** (7 parallel
workstreams, tag `m1-workstreams`) are done and merged into `main`. The batched contract changes are
on `main` too ([contract-requests/RESOLVED.md](contract-requests/RESOLVED.md)).
**Phase 2 (integration)**: the core and its three modules (capture, processing, library) are
merged (tag `m2-modules`), and the desktop app runs on the real back end. Start it with GPU speech
engines via `scripts\dev-gpu.cmd`. **Next:** a real end-to-end check in the app (record a Meet
call), then sync with the Fedora laptop and a real Google sign-in.

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

## Phase 2 — status

Done, each verified through the core with integration tests ([PHASE2.md](PHASE2.md) has the briefs):
- **capture**: recording (mic + loopback, crash recovery, lost-loopback = silence), live captions,
  `transcribe.local` with concurrent fast diarization. The 30-min meeting takes **82 s** on an idle
  GPU (WER 6.7 %, remote speakers 94.5 %).
- **processing**: the job loop plans and runs everything automatically; LLM + fact-check processors;
  offload to a peer over sync is tested. A real `gemma4:12b` run on the 30-min transcript takes
  ~6 min for all jobs.
- **library**: meetings, notes, search, ICS + Google (mocked) calendars, reminders, Gmail context,
  pairing and sync between cores, storage usage.

To do:
1. A real-app check by the user: record a call, captions, Stop, then the artifacts appear.
2. Sync with the Fedora laptop (build the app there); a real Google sign-in with the user's OAuth client.
3. Enhanced notes are thin when the user typed nothing — build them from the summary.
4. Open contract requests: `docs/contract-requests/i-*.md` (activity hook for every method,
   `RecordingStatus.lost`, `sync.paired` notification, typed `StorageUsage`, ...).

## Known issues

- All-day Google events are anchored at UTC midnight.
- Echo cancellation not done (needs meson, ninja, clang for webrtc AEC3).
- The HDMI audio endpoint drops out when the display sleeps.
- Linux and Android builds not verified locally.
- UI bundle has a 4.9 MB chunk (base64 fixtures) — split before release.
