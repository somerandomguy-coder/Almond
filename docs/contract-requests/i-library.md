# Phase 2 module `library` — contract requests and notes for the integrator

Everything below is worked around locally; none of it blocks the module.

## 1. Count every API call as user activity (router hook)

**What:** call `crate::library::touch(&core)` once per routed method (in `Router::call` or
`Api::call`), not only for library methods.

**Why:** reminders use `ActiveDevicePolicy` ("only the most recently used device notifies"), and
`device.info { last_active }` heartbeats tell peers which device is in use. Today only
`meetings.*`, `notes.*`, `search.*`, `calendar.*`, `mail.*`, `sync.*` and `storage.*` count;
someone who only records or reads artifacts on a device is not "active" there.
**Workaround:** library methods call `touch` themselves (`library::add`).

## 2. docs/API.md additions (additive)

| Method / key | Addition |
|---|---|
| `sync.pairAccept` | optional `device` (host device id) for the 6 words when mDNS can't see the host but its address was added with `sync.addAddr`; `words` may be an array or one string |
| `calendar.addIcs` | `url` may also be a local file path or `file://` URL (exported `.ics`) |
| `storage.usage` | `audio_bytes` is the audio/video part of `blobs_bytes`; `models_bytes` is the speech models dir only (LLMs live in Ollama) |
| `mail.context` | `[]` when the event has no other attendees or no connected account granted Gmail |
| `calendar.refresh` | fails (with the first failure's code) if any source failed; the other sources are still applied |
| setting `google.oauth_client` | also accepts the endpoint overrides of `almond_calendar::GoogleOAuthConfig` and `calendar_api_base` / `gmail_api_base` (used by the mock-Google tests; not for users) |

## 3. Types in `almond-types`

- `methods::STORAGE_USAGE` and a ts-exported `StorageUsage { db_bytes, blobs_bytes, models_bytes, audio_bytes }`.
  It is defined in `almond_core::library::StorageUsage` for now and hand-typed in
  `apps/desktop/src/api/types.ts`.
- `CalendarSourceInfo::gmail_enabled: Option<bool>` (WS-E request 2, still open) so Settings can
  show which accounts have Gmail on. Re-running `calendar.addGoogle { gmail: true, login_hint }`
  works today.

## 4. "Pairing completed" notification

The host (the device showing the QR / words) learns about the joiner inside the transport
(`IrohTransport::subscribe_paired`). There is no notification for it, so the Devices screen polls
`sync.status` every 3 s while a code is shown. A `sync.paired { device: DeviceInfo }` notification
would remove the polling.

## 5. Tauri shell: URL opener

`library::set_url_opener(&core, Arc<dyn Fn(&str) -> Result<()>>)` replaces how the core opens the
Google consent page. The default spawns `rundll32 url.dll,FileProtocolHandler` / `open` /
`xdg-open`, which is fine on desktop. Android needs the plugin (Custom Tab); until then
`calendar.addGoogle` returns `Unsupported` there. The Android plugin also has to call
`library::push_phone_snapshot(&core, json)` after `calendar.importPhone`.

## Notes for merging

- **Secrets are now scoped per data dir** (`almond_sync::secrets::default_secret_store`): the OS
  keychain service is `almond-<hash of the data dir>`, and data dirs under the system temp dir
  (tests) use only `secrets.json`. Before this, every core on a machine (tests, examples, the app)
  shared one device key in the keychain service `almond`, so two cores in one test process had
  the same device id. A stale Phase 1 entry is still in this PC's Credential Manager
  (`almond/device_key.almond`); nothing reads it any more and it can be deleted by hand.
- `SyncNode::open` no longer re-announces `device.info` at every start because of `last_active`.
- The UI's own 2-minute auto-sync (`components/Shell.tsx`) now runs only with `?mock=1`; the core
  runs it for real (`sync.auto_while_open`).
- Notes text is not in the full-text index: `search.query` covers meeting titles and artifacts
  (transcripts, summaries, …). Indexing notes needs a hook on remote `notes.update` events too.
- New `almond-core` dependencies: `base64`, `iana-time-zone`; dev: `loro`, `chrono`, `wiremock`,
  `url`.
