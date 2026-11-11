# Almond on Linux (Fedora laptop) + offload to the home PC

The laptop (Intel iGPU, CPU only) runs the whole app and does what it can locally; the home PC
(RTX 5060 Ti) does the heavy work when it is reachable over Tailscale.

## Build and run

```bash
sudo dnf install webkit2gtk4.1-devel gtk3-devel libsoup3-devel javascriptcoregtk4.1-devel \
  opus-devel libappindicator-gtk3-devel librsvg2-devel clang-devel
pnpm -C apps/desktop install
scripts/dev-linux.sh            # tauri dev with --features whisper,sherpa (CPU engines)
```

Release binary: `pnpm -C apps/desktop build && cargo build --release -p almond-desktop --features whisper,sherpa`
→ `target/release/almond-desktop`.

Local Ollama on the laptop: `gemma4:e4b` (fallback summaries). Speech models download under
Settings → Models (`~/.almond/models`); the live Vietnamese model (`sherpa:zipformer-vi-30m-int8`)
is fine on the CPU.

## Offload to the home PC

| | What | Needs on the PC |
|---|---|---|
| **AI (summary, notes, action items, title, fact-check)** | Settings → Home PC → address `100.80.179.59`. LLM jobs call the PC's Ollama whenever it answers (probe every 20 s), the laptop's otherwise. Default model there: `gemma4:12b`. | Ollama listening on `0.0.0.0` (it is). The Almond app does **not** need to run. |
| **Transcription and everything else** | Settings → Home PC → Heavy jobs = *Wait for the home PC*. Jobs stay unclaimed for a paired peer with a GPU synced in the last 15 min, up to the wait time (default 20 min), then run on the laptop. | Almond open, paired with the laptop, auto-sync on (or press Sync). Firewall: UDP 47321 from `100.64.0.0/10`. |

Settings keys: `llm.remote_ollama_url`, `processing.offload` (`local` | `prefer_peer`),
`processing.offload_wait_min`. `llm.status` shows which Ollama is in use.

Pairing over Tailscale: mDNS doesn't cross it. Use the QR text from Devices, and if a side can't
find the other, `sync.addAddr` with the Tailscale address (`100.101.48.54:47321` = laptop,
`100.80.179.59:47321` = PC).

Verified (2026-11-11): `ALMOND_REMOTE_OLLAMA=100.80.179.59 cargo test -p almond-core --test offload_remote -- --ignored`
→ fixture meeting summarized by the PC's `gemma4:12b` in 18 s; unreachable remote falls back to
the laptop's Ollama.

## Agents on two machines

The laptop and PC coding agents leave each other notes on the orphan branch `agents/mailbox`
(`to-pc.md`, `to-laptop.md`; rules and cleanup in its README). Never merge it; delete it when idle.
