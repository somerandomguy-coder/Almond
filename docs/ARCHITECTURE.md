# Almond — Architecture

A local-first, Granola-style meeting notepad. Records meetings, transcribes them on-device
(Vietnamese + English, Chinese later), lets you jot rough notes, then uses a **local** LLM (Ollama)
to enhance notes and write summaries. Knows your calendars (several Google accounts + phone
calendar), reminds you before meetings, and syncs everything between your own devices over the LAN.
No cloud required.

Research behind these choices: [RESEARCH.md](RESEARCH.md). Work breakdown: [PLAN.md](PLAN.md).

## 1. Goals / non-goals

**Goals**
- Windows PC, Linux box, Android phone — one codebase, all peers, all syncing over LAN.
- Vietnamese + English first-class (including code-switching); Chinese is a config addition, not a rewrite.
- Every AI output is **grounded** (cites transcript spans) and **versioned with provenance**, so a
  verification layer (incl. future neuro-symbolic claim checking) can plug in without schema changes.
- Heavy compute can be **offloaded** to whichever paired device has the GPU.

**Non-goals (v1)**: cloud accounts, multi-user/team sharing, iOS/macOS, bots joining calls,
recording phone calls on Android (blocked by the platform).

## 2. Big picture

```
 ┌────────────── Windows PC (GPU) ─────────────┐     ┌──────── Linux box ────────┐
 │ Tauri UI (React/TS)                          │     │ almondd (headless) or UI  │
 │   │ Tauri commands / events                  │     │ Ollama, STT models        │
 │ almond-core ─ audio ─ stt ─ pipeline ─ llm ──┼──┐  └─────────────┬─────────────┘
 │      │          verify   calendar   mail     │  │                │
 │   store (SQLite + blobs)   sync (iroh) ──────┼──┼──── LAN QUIC ──┤ (mDNS / NSD discovery)
 │ Ollama :11434 (localhost only)               │  │                │
 └──────────────────────────────────────────────┘  │  ┌─────────────┴─────────────┐
                                                   └──│ Android: Tauri UI + Kotlin │
                                                      │ plugin (mic svc, calendar, │
                                                      │ NSD) + sherpa-onnx ASR     │
                                                      └────────────────────────────┘
```

- **Rust core, everywhere.** No Python runtime on devices. Same crates compile for Windows, Linux,
  Android (aarch64). Platform specifics live behind traits + `cfg`.
- **Tauri 2** shell for desktop and Android; one React + TypeScript UI.
- **almondd**: the same core as a headless daemon — lets the Linux box act as an always-on compute +
  sync node.
- Ollama is only ever reached via localhost by the core on that machine. Other devices request work
  through the sync layer (jobs), never by hitting Ollama directly.

## 3. Repo layout (monorepo, Cargo workspace + pnpm)

```
crates/
  almond-types/      # domain model, IDs, artifact kinds, traits — THE contract (owned by Phase 0)
  almond-store/      # SQLite (rusqlite) + migrations, content-addressed blob store (BLAKE3)
  almond-audio/      # capture backends (wasapi, pipewire, android-bridge, file), VAD, resample, opus
  almond-stt/        # SttEngine trait impls: whisper-rs, sherpa-onnx; diarization; model manager
  almond-llm/        # LlmProvider trait impls: ollama, remote-peer; prompt templates (vi/en/zh)
  almond-pipeline/   # processor registry + DAG runner + job queue (local and offloaded)
  almond-verify/     # claim extraction + Verifier trait + baseline verifiers + external plugin host
  almond-calendar/   # CalendarSource impls: google (multi-account), ics, phone; reminder scheduler
  almond-mail/       # gmail: thread context for attendees, follow-up drafts
  almond-sync/       # device identity, pairing, discovery, transport, log replication, blob transfer
  almond-core/       # facade wiring everything; exposes Rust API + local JSON-RPC/WebSocket API
apps/
  desktop/           # Tauri 2 app (Windows, Linux, Android targets), UI in apps/desktop/ui
  almondd/           # headless daemon binary
plugins/
  tauri-plugin-almond-android/   # Kotlin: foreground recording service, CalendarContract, NSD, notifications
eval/                # STT WER + summary quality harness, small vi/en datasets (no private audio in git)
docs/
```

## 4. Data model (event-sourced, artifact-centric)

Everything the AI produces is an **Artifact**; artifacts are immutable. Corrections create a new
artifact that `supersedes` the old one. This makes sync trivial (no conflicts on immutable data),
makes verification possible (we always know exactly which inputs produced which output), and leaves
room for new kinds of outputs without migrations.

```rust
// almond-types (sketch — Phase 0 finalizes)
pub struct Id(Ulid);                       // time-sortable, generated on any device
pub struct DeviceId(PublicKey);            // ed25519, = iroh node id

pub struct Meeting {
    id: Id, title: String, started_at, ended_at: Option<_>,
    calendar_event: Option<EventRef>,      // link to a synced calendar event
    languages: Vec<LangTag>,               // ["vi","en"], BCP-47
    audio: Vec<BlobRef>,                   // per-channel opus files (mic, system)
    notes_doc: DocId,                      // Loro CRDT doc with the user's own notes
}

pub struct Artifact {
    id: Id,
    meeting_id: Id,
    kind: ArtifactKind,                    // "transcript", "summary", "claims", "verdicts", "enhanced_notes",
                                           // "action_items", ... — open set, string-typed
    schema: u32,                           // body schema version for that kind
    body: serde_json::Value,               // validated against JSON Schema for (kind, schema)
    provenance: Provenance,
    supersedes: Option<Id>,
    created_at: Hlc, device: DeviceId,
}

pub struct Provenance {
    processor: String, processor_version: String,   // "summarize.ollama", "0.3.1"
    model: Option<ModelRef>,                         // "ollama:qwen3:8b@<digest>", "whisper:phowhisper-large@<sha>"
    params_hash: Blake3,                             // prompt template id + params
    inputs: Vec<Id>,                                 // artifact ids consumed
}

pub struct SpanRef {                                 // the grounding primitive
    artifact: Id, segment: u32,
    char_range: Option<(u32,u32)>, time_ms: Option<(u64,u64)>,
}
```

Key artifact bodies (v1):

- `transcript`: `segments: [{ idx, t0_ms, t1_ms, speaker: Option<SpeakerId>, channel, lang, text, words?, confidence }]`
- `summary`: `sections: [{ heading, bullets: [{ text, cites: [SpanRef] }] }]` — **every bullet cites**
- `enhanced_notes`: user notes merged with AI additions; each block tagged `origin: user|ai` + `cites`
- `action_items`: `[{ text, owner?, due?, cites }]`
- `claims` (from WS-H): atomic, checkable statements extracted from a summary: `{ id, text, source: SpanRef(summary bullet), logical_form?: Json, evidence: [SpanRef] }`
- `verdicts`: `[{ claim_id, verifier, label: supported|contradicted|unverifiable|needs_review, confidence, evidence: [SpanRef], explanation, trace?: Json }]`

**Unknown kinds/schemas are preserved and synced as opaque JSON** — an old phone won't break when
the PC starts producing `verdicts.v2` from a new neuro-symbolic verifier.

## 5. Pipeline & processors (the extension point)

```rust
#[async_trait]
pub trait Processor: Send + Sync {
    fn descriptor(&self) -> ProcessorDescriptor;   // name, version, consumes: [kind], produces: [kind],
                                                   // requires: [Capability] (e.g. Gpu, Llm("qwen3"), Stt("vi"))
    async fn run(&self, ctx: &JobCtx, inputs: Vec<Artifact>) -> Result<Vec<ArtifactDraft>>;
}
```

Default DAG for a finished meeting:

```
audio ─► transcribe ─► diarize ─► transcript
                                      ├─► summarize ─► summary ─► extract_claims ─► claims ─► verify(*) ─► verdicts
                                      ├─► enhance_notes (+ user notes doc) ─► enhanced_notes
                                      └─► action_items
```

- **Jobs** are records in the synced log: `job.requested`, `job.claimed{lease}`, `job.done`, `job.failed`.
  Any paired device whose advertised **capabilities** match can claim a job. Result: the phone
  records + live-transcribes with a small model, then the PC (GPU) re-transcribes with
  PhoWhisper-large and summarizes; results sync back automatically.
- **External processors** (research hook): a processor can live out-of-process and speak JSON-RPC
  2.0 over stdio or localhost HTTP (`processor.describe`, `processor.run`). That lets you prototype a
  verifier in Python (Z3, Prolog, Soufflé, a fine-tuned NLI model) without touching Rust.
- Re-running with a better model/verifier = new artifacts with `supersedes`, old ones kept for comparison.

### Verification layer (designed now, grown later)

`almond-verify` ships with:
1. **Claim extraction** — LLM splits summary bullets into atomic claims (structured output).
2. **Grounding verifier** — does the cited span actually support the claim? (LLM-as-judge / NLI).
3. **Symbolic sanity rules** — deterministic checks: numbers/dates/names in the claim appear in
   the evidence; dates agree with the calendar event; speakers/owners are actual attendees.
4. **Hook for neuro-symbolic** — `claims.logical_form` (e.g. triples / Datalog facts) is optional
   now; a future verifier fills it and runs a rule engine (`ascent`/`crepe` in Rust, or Z3/Soufflé
   externally) for consistency across claims, meetings and calendar facts. The `trace` field in a
   verdict stores the proof/derivation for the UI.

UI surfaces verdicts as underline colors on summary bullets with hover → evidence.

## 6. Storage

- **SQLite** per device (rusqlite, WAL). Tables: `events` (the replicated log), materialized views
  (`meetings`, `artifacts`, `calendar_events`, `jobs`, `devices`), FTS5 index over transcripts +
  summaries (unicode61 tokenizer with diacritic folding so "hop" finds "họp").
- **Blobs** (audio, model files are *not* synced) content-addressed by BLAKE3 under `data/blobs/`.
- **Notes** = Loro docs; Loro updates are stored/replicated as events.
- Secrets (OAuth refresh tokens, device private key) → OS keychain / Android Keystore, never in SQLite
  and never synced. Each device connects to Google itself, or receives synced calendar *events* only.

## 7. Sync protocol

- **Identity:** each device has an ed25519 keypair (= iroh node id). A "vault" = set of paired devices.
- **Pairing:** device A shows a QR (node id + LAN addrs + one-time secret); B scans (phone) or types a
  6-word code (desktop↔desktop). Mutual challenge with the secret → both store each other's key.
  Unpaired peers are rejected at the transport level.
- **Discovery:** iroh local discovery (mDNS) + Android NSD fallback + manual `ip:port`. LAN-only by
  default (relays off); off-LAN via relay is a later toggle.
- **Replication:** each event = `{ device, seq, hlc, type, payload }`. Per-device monotonically
  increasing `seq` → version vector. Sync = exchange vectors, stream missing events, ack. Idempotent,
  resumable, works with any number of peers (no leader).
- **Blobs:** fetched lazily by hash with per-device policy (phone: "metadata + text always, audio
  only on demand"; PC: "everything").
- **Conflicts:** immutable artifacts can't conflict; mutable fields (meeting title, tags) are LWW by
  HLC; notes are Loro (true merge).
- **Forward compat:** unknown event types are stored and relayed untouched.

## 8. Calendar & mail

- `CalendarSource` trait: `GoogleCalendar{account}` (OAuth loopback+PKCE, `syncToken` incremental,
  any number of accounts), `IcsSubscription{url}`, `PhoneCalendar` (Android CalendarContract, one
  "Import phone calendars" button). Events normalized and synced to all devices as events, so the PC
  sees the phone's calendar and vice versa.
- **Reminder scheduler:** N minutes before an event with a video link or attendees → OS notification
  "Meeting with X in 5 min — [Start notes]". Starting from a notification pre-links the event
  (title, attendees, agenda from description).
- **Gmail (optional, per account):** fetch recent threads with the event's attendees as summary
  context; create a follow-up **draft** (never auto-send).

## 9. Local API (for UI, CLI, plugins, future agents)

`almond-core` exposes one API surface, transported three ways: Tauri commands (UI), JSON-RPC over
WebSocket on `127.0.0.1` with a token (CLI, external processors, scripts), and later an **MCP
server** so an AI agent can search/ask over your meetings. Namespaced methods, versioned:

```
meetings.list / get / create / update / delete
recording.start{meeting?, sources} / stop / status        (+ stream: transcript.partial)
artifacts.list{meeting, kind?} / get / rerun{processor, inputs}
notes.get / notes.applyUpdate (Loro)
search.query{text, lang?}
calendar.accounts.add{google|ics|phone} / remove / events.upcoming
mail.context{event} / mail.draftFollowUp{meeting}
pipeline.processors.list / register{external} / jobs.list
verify.run{artifact, verifiers?} / verify.verifiers.list
sync.devices.list / pair.begin / pair.accept / unpair / status
models.list / download / select{task, lang}
```

Rule: **the UI never reaches into storage directly** — it only uses this API, so a CLI or another
front-end gets identical capabilities.

## 10. Languages

- `LangTag` everywhere (BCP-47). STT model routing table per language:
  `vi → phowhisper-large (desktop) / zipformer-vi (android)`, `en → whisper-large-v3-turbo / whisper-small`,
  `zh → (later) whisper-large-v3-turbo / sensevoice`. Mixed meetings: per-segment language ID.
- Prompt templates are files keyed by `(task, output_lang)`; summary output language = user choice
  (default: dominant meeting language).
- UI strings via i18n keys from day one (`vi`, `en`; `zh` later).
