# Contract requests from WS-H (verification)

All additive. Nothing here blocks WS-H; local workarounds are in place.

## 1. `VerifyContext.params`

**What**

```rust
pub struct VerifyContext {
    // ...existing fields...
    /// Job params (tz, thresholds, verifier subset, ...).
    pub params: serde_json::Value,
}
```

**Why** Verifiers need per-job settings, most importantly the meeting's time zone to resolve
"thứ Sáu" / "tomorrow" / meeting start time. Today they get it from `VerifyConfig` at
construction (default: the machine's local zone), so a job offloaded to a PC in another zone would
resolve dates differently. **Compat:** additive; `Default`-able.

## 2. Meeting time zone

**What** `Meeting.tz: Option<String>` (IANA, e.g. "Asia/Ho_Chi_Minh") or `tz_offset_min: Option<i32>`.

**Why** Same as above but at the source of truth; also useful for the UI and calendar matching.
**Compat:** additive, optional.

## 3. Calendar event in pipeline inputs

**What** Either a well-known artifact kind `calendar_snapshot` (body = `CalendarEvent`) that the
pipeline attaches to verify jobs, or the pipeline filling `VerifyContext.calendar_event` from
`Meeting.calendar_event` via the store.

**Why** `JobCtx` has no store access, so `verify.all` currently reads the event from
`params.calendar_event`. **Compat:** additive.

## 4. Verifier failures in `Verdicts`

**What** `Verdicts { verdicts, errors: Vec<VerifierError { verifier: String, message: String }> }`
with `#[serde(default)]`.

**Why** One failing verifier must not sink the others; today its error is dropped (the NLI verifier
already degrades to a `needs_review` verdict with `trace.error`). **Compat:** additive (serde default).

## 5. (Informational) `Provenance.params_hash`

WS-H currently writes the prompt template id (`extract_claims.v1`, `rules.v1`) there because no
BLAKE3 helper exists in `almond-types`. A shared `params_hash(template_id, &params)` helper would keep
all processors consistent.
