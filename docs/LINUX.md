# Almond on Linux (Fedora laptop) + offload to the home PC

The laptop (Intel iGPU, CPU only) runs the whole app and does what it can locally; the home PC
(RTX 5060 Ti) does the heavy work when it is reachable over Tailscale.

## Build and run

```bash
sudo dnf install webkit2gtk4.1-devel gtk3-devel libsoup3-devel javascriptcoregtk4.1-devel \
  opus-devel libappindicator-gtk3-devel librsvg2-devel clang-devel
pnpm -C apps/desktop install
scripts/run-linux.sh            # everyday use: release build (UI embedded) + start
scripts/dev-linux.sh            # editing the app: debug build + Vite dev server (slower)
```

Live caption language: Settings → General → "Live caption language" (setting `captions.language`).
On a CPU-only device live captions don't detect the language; English uses
`sherpa:zipformer-en-small-int8` (install: `cargo run --release -p almond-stt --example models --
get sherpa:zipformer-en-small-int8`, 112 MB), measured 0.3 s median until words show.

Release binary (UI embedded): `pnpm -C apps/desktop tauri build --no-bundle --features whisper,sherpa`
→ `target/release/almond-desktop`. **Not** plain `cargo build`: without Tauri's `custom-protocol`
feature (which `tauri build` turns on) the binary is a dev build that loads the UI from the Vite
dev server at `http://localhost:1420` ("Could not connect to localhost: Connection refused").

Local Ollama on the laptop: `gemma4:e4b` (fallback summaries). Speech models download under
Settings → Models (`~/.almond/models`), or `cargo run --release -p almond-stt --example models --
get sherpa:zipformer-vi-30m-int8 sherpa:pyannote-seg-3.0 sherpa:3dspeaker-eres2net-base whisper:small`
(the CPU routing table, ~560 MB).

### Speech on the laptop CPU (i7-10610U, 4 cores, measured 2026-11-11)

| | Time | RTF |
|---|---|---|
| Zipformer-vi (sherpa), 64 s of Vietnamese | 1.3 s | 0.02 |
| whisper-small (whisper.cpp, AVX2, 4 or 8 threads, beam 1 or 5) | 132 s | 2.06 |
| `transcribe.local`, 15 s stereo (vi+en language ID per utterance + diarization) | 138 s | ~4.6 |
| Diarization alone, 64 s | 1.8 s | 0.03 |

Each whisper call costs a full 30-s encoder pass (~7 s here), so whisper language ID is what is
slow. Consequences on a device without a GPU:
- **Live captions** stay in the primary language (Zipformer, real time) with no language ID; the
  post-meeting transcript still routes vi/en.
- **Post-meeting transcription** defaults to the PC (`processing.offload` unset = `prefer_peer`
  without a GPU). Away from the PC it still runs locally after the wait time, slowly.

## Offload to the home PC

| | What | Needs on the PC |
|---|---|---|
| **AI (summary, notes, action items, title, fact-check)** | Settings → Home PC → address `100.80.179.59`. LLM jobs call the PC's Ollama whenever it answers (probe every 20 s), the laptop's otherwise. Default model there: `gemma4:12b`. | Ollama listening on `0.0.0.0` (it is). The Almond app does **not** need to run. |
| **Transcription and everything else** | Settings → Home PC → Heavy jobs = *Wait for the home PC* (the default without a GPU). Jobs stay unclaimed for a paired peer with a GPU synced in the last 15 min, up to the wait time (default 20 min), then run on the laptop. | Almond open, paired with the laptop, auto-sync on (or press Sync). Firewall: UDP 47321 from `100.64.0.0/10`. |

Settings keys: `llm.remote_ollama_url`, `processing.offload` (`local` | `prefer_peer`),
`processing.offload_wait_min`. `llm.status` shows which Ollama is in use.

Pairing over Tailscale: mDNS doesn't cross it. Use the QR text from Devices, and if a side can't
find the other, `sync.addAddr` with the Tailscale address (`100.101.48.54:47321` = laptop,
`100.80.179.59:47321` = PC).

Verified (2026-11-11): `ALMOND_REMOTE_OLLAMA=100.80.179.59 cargo test -p almond-core --test offload_remote -- --ignored`
→ fixture meeting summarized by the PC's `gemma4:12b` in 18 s; unreachable remote falls back to
the laptop's Ollama. `cargo test -p almond-core --test offload`: two cores over localhost iroh; the
"laptop" leaves a job it could run for the "PC" (which only initiates syncs), falls back after the
wait time, and `local` never waits.

## Agents on two machines

The laptop and PC coding agents leave each other notes on the orphan branch `agents/mailbox`
(`to-pc.md`, `to-laptop.md`; rules and cleanup in its README). Never merge it; delete it when idle.
