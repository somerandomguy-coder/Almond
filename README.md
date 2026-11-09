# Almond 🌰

A local-first, Granola-style AI meeting notepad. Records and transcribes meetings on your own
devices (Vietnamese + English, Chinese later), enhances your rough notes and writes cited summaries
with a local LLM (Ollama), knows your calendars (multiple Google accounts + phone calendar), and
syncs everything between your PC, Linux box and Android phone over the LAN. No cloud.

**Status:** planning. See:

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — design, data model, sync protocol, API, verification hooks
- [docs/PLAN.md](docs/PLAN.md) — phased, parallel work plan (workstreams A–H)
- [docs/RESEARCH.md](docs/RESEARCH.md) — research notes and sources

**Stack:** Rust core · Tauri 2 (Windows, Linux, Android) · React + TypeScript · whisper.cpp
(PhoWhisper) + sherpa-onnx · Ollama · SQLite · Loro · iroh.
