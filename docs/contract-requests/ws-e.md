# WS-E contract requests (calendar + mail)

All additive. WS-E works today without them (local shims noted below).

## 1. Well-known key for the Google account index

**What** — in `almond_types::secrets::keys`:

```rust
/// JSON list of connected Google accounts (email, granted scopes); no secrets.
pub const GOOGLE_ACCOUNTS: &str = "almond/google/accounts";
```

**Why** — `SecretStore` can't list keys, so `GoogleAuth` keeps an index of connected accounts.
Integration/UI code that inspects accounts should not need to import `almond-calendar` for the
key name. **Shim:** `almond_calendar::oauth::ACCOUNT_INDEX_KEY` (same value).

## 2. `calendar.addGoogle` params: optional Gmail

**What** — document params `{ gmail?: boolean, login_hint?: string }` for `calendar.addGoogle`
(docs/API.md), plus a way to toggle Gmail later (re-running `addGoogle` with `login_hint` and
`gmail: true` already works: it replaces the token and scopes).

**Why** — Gmail is strictly optional per account (`gmail.readonly` is a *restricted* scope);
the consent screen must only ask for it when the user ticks the box.
`CalendarSourceInfo` could also carry `gmail_enabled: Option<bool>` so the UI can show the
state; until then the UI can read `GoogleAuth::accounts()` via the integrator.

## 3. Resetting a calendar's events (`CalendarDelta`)

**What** —

```rust
pub struct CalendarDelta {
    pub upserts: Vec<CalendarEvent>,
    pub deletes: Vec<String>,
    pub next_cursor: Option<String>,
    /// Calendar ids (within this source) whose stored events should be dropped *before*
    /// applying `upserts` (full resync after an expired sync token, calendar unsubscribed).
    pub reset_calendars: Vec<String>, // #[derive(Default)] keeps this additive
}
```

**Why** — after Google answers 410 (sync token expired) we do a full windowed resync of that
calendar, but events deleted while the token was stale never come back as "cancelled", so
they linger in the store. Same when a calendar disappears from `calendarList` (unsubscribed).
**Today:** stale events can remain until they scroll out of the window; the ICS and phone
sources don't have this problem (they diff snapshots in the cursor).

## 4. Where the OAuth client lives in settings

**What** — a settings entry (kv or config file, owned by integration) holding
`almond_calendar::GoogleOAuthConfig { client_id, client_secret }` (serde-compatible; endpoint
fields default). Same values on every device; it is *not* a secret in the OAuth sense
(Desktop client), but should not be synced to third parties either — local config is fine.

**Why** — the user brings their own GCP project (docs/google-setup.md, step 6 refers to
"Settings → Calendar & mail → Google").

## 5. Device activity for "which device notifies"

**What** — add to `DeviceInfo` (re-emitted as a `device.info` event while the app is in use):

```rust
#[serde(default)]
#[ts(type = "number | null")]
pub last_active: Option<UnixMs>,
```

**Why** — `ActiveDevicePolicy` lets only the most recently *used* device notify (all devices
notify when none was active in the last 15 min). The integrator feeds
`almond_calendar::reminder::DeviceActivity::record(device, ts)` from local input / app focus
and from peers. `PeerInfo::last_seen` is a network-presence signal, not user activity, so it's
a poor proxy. **Caveat:** sync is on demand (no daemon), so a peer's activity is only as fresh
as the last sync; in practice the policy mostly prevents duplicate notifications between
devices that are open and synced at the same time, which is the case that matters.
