# Almond — Research Notes (2026-11-09)

Findings that drive the decisions in [ARCHITECTURE.md](ARCHITECTURE.md). Anything marked
**VERIFY** must be benchmarked or confirmed by the workstream that owns it before we lock it in.

## 1. Prior art (local Granola-likes)

| Project | Stack | Takeaway for us |
|---|---|---|
| Meetily (Zackriya) | Tauri + Rust, Whisper, Ollama | Closest to what we want. Per-OS capture: WASAPI loopback (Win), ScreenCaptureKit (mac), PulseAudio monitor (Linux). Good reference for audio code. No multi-device sync. |
| Anarlog (ex-Hyprnote, MIT) | Tauri | "Enhance my rough notes with transcript" UX — the core Granola idea. On-device STT only on Apple Silicon. |
| Minutes, Amical, Prismical | — | Other OSS alternatives; none do LAN multi-device sync + Vietnamese. |

**Conclusion:** Tauri 2 + Rust core is the proven path. Our differentiators: Vietnamese-first,
LAN sync across Win/Linux/Android, compute offload (phone → PC), and a verification layer.

## 2. Speech-to-text

- **PhoWhisper** (VinAI, 2024): Whisper fine-tuned on 844 h of regional-accent Vietnamese; tiny→large.
  SOTA on Common Voice / VIVOS / VLSP 2020 at publication. Default for Vietnamese on desktop.
  Needs conversion to ggml for whisper.cpp or CTranslate2 for faster-whisper. **VERIFY** license + conversion.
- **Vietnamese–English code-switching** is a real problem (tech meetings). A LoRA on PhoWhisper-large
  exists for VI-EN code-switching (self-reported numbers, small test set); TSPC (phoneme-centric) is
  a research architecture, not a drop-in. → We keep the STT engine pluggable and run our own eval.
- **whisper.cpp vs faster-whisper:** whisper.cpp = single native lib, CPU/Vulkan/CUDA, mobile-friendly,
  easy from Rust (`whisper-rs`). faster-whisper/WhisperX = Python, best on NVIDIA. Neither streams
  natively; real-time needs chunking + overlap (whisper-streaming / WhisperLive approach).
  → Rust-native whisper.cpp to avoid a Python runtime on every device.
- **Diarization:** neither Whisper engine does it. pyannote is the standard; **sherpa-onnx** ships
  ONNX exports of pyannote segmentation + 3D-Speaker embeddings, usable from Rust and Android.
- **Android on-device ASR:** sherpa-onnx has a Vietnamese Zipformer RNN-T (~75 MB INT8, trained on
  ~70k h, ~7.7% WER on GigaSpeech2-vi clean as reported by a third party). Fast enough for live
  captions on a phone. **VERIFY** on our audio.
- Chinese later: Whisper large-v3(-turbo) handles zh; sherpa-onnx has zh models (Paraformer/SenseVoice).

## 3. Local LLM (Ollama)

- Qwen3 family is the most consistently recommended open model family for Vietnamese; small models
  (≤2B) are weak at Vietnamese summarization. Gemma 3 is the other candidate (a Vietnamese Gemma
  fine-tune, GemSUra, exists). No reliable public benchmark for VI meeting summarization →
  **we build a tiny eval set and pick by score** (WS-C).
- Ollama supports JSON-schema-constrained output (`format`) → use it for structured, citeable summaries.
- Starting defaults (subject to eval and the PC's GPU): `qwen3:8b` (8–12 GB VRAM), `qwen3:14b` /
  `gemma3:12b` if it fits, `qwen3:4b` for CPU-only. Never assume a model name is fixed — config.
- Phone does **not** run the summarizer by default; it offloads to a paired PC over LAN (see §5).

## 4. Audio capture

- **Windows:** WASAPI loopback (system audio) + mic, mixed or kept as two channels (better:
  two channels = free "me vs them" diarization hint). Loopback delivers nothing when nothing plays —
  handle silence/gaps. Echo cancellation (WebRTC AEC3) when speakers, not headphones, are used.
- **Linux:** PipeWire default-sink monitor (`stream.capture.sink=true`), PulseAudio monitor fallback.
- **Android:** mic recording works (in-person meetings) via a foreground service.
  `AudioPlaybackCapture` (Android 10+) needs user consent each session and apps can opt out
  (`allowAudioPlaybackCapture=false`); third-party *phone call* recording is blocked by Play policy
  since 2022. → **Phone = in-person/mic recorder + viewer; online meetings are recorded on PC.**
  **VERIFY** whether Meet/Zoom/Teams Android allow playback capture.

## 5. LAN sync

- **CRDTs:** Automerge and Loro are the two mainstream Rust cores. Loro is Rust-native, fast, has a
  ProseMirror binding (fits a TipTap editor) and a movable tree. → Loro for user-editable notes only.
- Most of our data is **immutable** (transcripts, AI outputs, audio) → doesn't need a CRDT; an
  append-only, content-addressed event log replicated with version vectors is simpler and more robust.
- **Transport:** iroh (QUIC, ed25519 node IDs, optional mDNS local discovery, hole-punching/relays
  for later off-LAN sync). Has a documented Automerge-sync example. On Android, Rust mDNS may be
  unreliable (multicast lock, OEM quirks) → use Android NSD (`NsdManager`) via our Kotlin plugin as
  a fallback discovery source and feed addresses into iroh. **VERIFY** current iroh version/API (it
  moves fast — pin it); keep transport behind a trait (fallback: `mdns-sd` + `quinn`).

## 6. Google (Calendar + Gmail, multiple accounts)

- Desktop OAuth: installed-app flow with **loopback redirect + PKCE**; tokens per account in the OS
  keychain (`keyring` crate; Android Keystore on phone).
- Each user brings their **own Google Cloud OAuth client** (it's a personal app). Gotchas:
  - In *Testing* publishing status, refresh tokens expire after **7 days** → set the consent screen to
    *In production* (unverified is OK for personal use, you click through the warning; 100-user cap).
  - `calendar.readonly` is a *sensitive* scope; Gmail read/compose scopes are *restricted*. Fine for
    personal use, never fine for public distribution without CASA audit. Keep Gmail optional and
    scopes minimal. **VERIFY** exact scope classes in the console.
- Calendar: use `events.list` with `syncToken` for incremental sync; poll every few minutes (push
  webhooks need a public HTTPS endpoint — not available on a LAN app).
- **Phone calendar "easy button":** Android `CalendarContract` (READ_CALENDAR) reads *every* calendar
  on the phone (Google, Samsung, Exchange, local) in one tap; synced to desktops as a calendar source.
  Plus `.ics` URL subscription as a generic source.

## Sources

- PhoWhisper: https://arxiv.org/pdf/2406.02555
- VI-EN code-switching LoRA: https://huggingface.co/rinhoooo/phowhisper-large-vien-cs-asr
- TSPC review: https://www.themoonlight.io/en/review/tspc-a-two-stage-phoneme-centric-architecture-for-code-switching-vietnamese-english-speech-recognition
- whisper.cpp vs faster-whisper vs WhisperX: https://dev.to/jovan_chan_9500711396d4e6/faster-whisper-vs-whispercpp-vs-whisperx-2026-o29
- Self-hosted STT 2026: https://smallest.ai/blog/open-source-speech-to-text-in-2026-what-to-self-host-vs-use-as-an-api
- Meetily audio capture: https://instagit.com/Zackriya-Solutions/meetily/meetily-platform-specific-audio-capture-apis/
- Tauri system audio plugin: https://docs.rs/tauri-plugin-system-audio
- flexaudio (PipeWire/WASAPI): https://docs.rs/flexaudio
- WASAPI loopback: https://msdn.microsoft.com/en-us/library/vs/alm/dd316551
- Vietnamese LLM guide (vendor): https://www.siliconflow.com/articles/best-open-source-llm-for-vietnamese
- Vietnamese LLM eval (Truong et al.): https://www.themoonlight.io/fr/review/crossing-linguistic-horizons-finetuning-and-comprehensive-evaluation-of-vietnamese-large-language-models
- iroh + Automerge: https://docs.iroh.computer/protocols/automerge
- iroh local discovery: https://docs.iroh.computer/connecting/local-discovery
- iroh mDNS notes: https://iroh.computer/blog/iroh-0-92-0-mdns-improvements
- iroh-http-discovery (mobile → native NSD): https://docs.rs/iroh-http-discovery
- Local-first CRDT engines 2026: https://blog.codercops.com/blog/local-first-crdt-sync-engines-2026
- Automerge: https://automerge.org/docs/hello/
- Android mDNS: https://blog.esper.io/android-dessert-bites-26-mdns-local-47912385
- sherpa-onnx Vietnamese (phostt): https://docs.rs/crate/phostt/latest
- viet-asr: https://huggingface.co/coflar-ai/viet-asr
- sherpa-onnx RN wrapper w/ diarization: https://app.unpkg.com/react-native-sherpa-onnx-offline-stt@0.1.0/files/README.md
- Android playback capture: https://developer.android.com/media/platform/av-capture
- Play call-recording ban: https://www.theregister.com/2022/04/22/google_banning_thirdparty_callrecording_apps/
- Google unverified apps: https://support.google.com/cloud/answer/7454865
- Google security review / CASA: https://nango.dev/docs/api-integrations/google-shared/google-security-review
- Open-source Granola alternatives: https://openalternative.co/alternatives/granola
- Meetily: https://dev.to/zackriya/meetily-the-best-free-open-source-self-hosted-ai-meeting-note-taker-a-granola-alternative-for-2c8e
