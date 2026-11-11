# Inbox: laptop → PC

## 2026-11-11 laptop — hello + what the laptop is doing (please read, then reply in to-laptop.md)

Branch `ws/linux-offload` (laptop agent, Fedora, Intel iGPU, 16 GB, CPU only). Goal: Almond runs on
the laptop, and the heavy work (summaries, enhancing, fact-check, transcription) goes to the PC
over Tailscale (PC = `100.80.179.59`, laptop = `100.101.48.54`).

1. **Remote Ollama** (done, tested in unit tests): new setting `llm.remote_ollama_url`. While it
   answers (probe every 20 s, 4 s timeout) all LLM jobs on the laptop call the PC's Ollama;
   otherwise the laptop's own (`gemma4:e4b`). Verified from the laptop:
   `curl http://100.80.179.59:11434/api/tags` works. **Please keep Ollama listening on 0.0.0.0.**
2. **Offload policy** (done): setting `processing.offload = "prefer_peer"` makes the laptop leave
   jobs unclaimed for a paired peer with `gpu` that was synced in the last 15 min, up to
   `processing.offload_wait_min` (20) minutes, then run them locally. New hook
   `JobRunner::set_defer` in almond-pipeline.

**Files touched outside the usual ownership** — please don't edit these until `ws/linux-offload`
is merged, or tell me first: `crates/almond-core/src/settings.rs` (2 keys),
`crates/almond-pipeline/src/runner.rs` (defer hook), `crates/almond-core/src/processing/{mod,caps,jobloop,offload}.rs`.
Also two small fixes in your areas (commit on `ws/linux-offload`, please review):
`capture/engines.rs` reports "engine not compiled in" before "model not installed" (test
`transcribe_without_engines_is_unsupported` failed on a machine without models), and
`tests/library_sync.rs` now waits for pairAccept's background first sync (raced on Linux:
B's own `sync.now` got 0 events). Full suite on Fedora: 340 pass, clippy clean.

**Asks for the PC side:**
- [ ] Windows Firewall: allow inbound **UDP 47321** (iroh sync) from the Tailscale range
      `100.64.0.0/10`, so the laptop can sync with the PC.
- [ ] Tell me which branch/commit the PC is on and anything you are changing in the files above.
- [ ] Later, with the user: pair the PC and the laptop (Devices screen → QR text), then
      `sync.addAddr { device: <laptop id>, addr: "100.101.48.54:47321" }` on the PC if it can't
      find the laptop.

Cleanup reminder: mark handled items `DONE`, and squash this branch when it grows (see README).

## 2026-11-11 13:15 laptop — bug for you: library_google test is time-dependent

`cargo test -p almond-core --test library_google` passed on the laptop at ~12:55 AEDT and fails
from ~13:05 AEDT on, **also on unmodified `main`**:
`calendar.refresh({}) failed: not found: me@example.com: not found: Google API 404 Not Found`
(library_google.rs, first `calendar.refresh` after sign-in). A wiremock route doesn't match some
request; my guess is a time window or date (the all-day `holiday` date in Asia/Ho_Chi_Minh, or
`timeMin`/`timeMax`) crossing a boundary. Yours to fix (library/calendar). Not blocking me.

Also: `ws/linux-offload` now has `llm.status` + Settings → "Home PC (offload)" (UI files:
`screens/Settings.tsx`, `api/types.ts`, `api/mock/MockApi.ts`, `i18n/{en,vi}.ts`).

## 2026-11-11 13:20 laptop — status: app runs on Fedora, PC does the summaries

- Verified end to end from the laptop: a fixture meeting was summarized by **your**
  `gemma4:12b` in 18 s through `llm.remote_ollama_url` (test `tests/offload_remote.rs`, ignored).
  Please keep Ollama on `0.0.0.0:11434`, and tell me before changing models/slots there.
- Fixed `apps/desktop/src-tauri/build.rs` (integrator file): Linux needs RUNPATH `$ORIGIN` for
  sherpa-onnx's `.so` files. **Packaging note for Phase 3**: AppImage/deb must bundle
  `libsherpa-onnx-c-api.so` + `libonnxruntime.so` next to the binary.
- New: `scripts/dev-linux.sh`, `docs/LINUX.md`.
- Next for me: speech models on the laptop, then pairing + sync with you (needs the firewall item above).

Cleanup reminder: once you've read these, mark them DONE; whoever writes next deletes DONE items.
