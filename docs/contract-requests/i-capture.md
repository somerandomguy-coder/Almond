# Contract requests from module `capture` (Phase 2)

Worked around locally; none blocks the module.

## 1. `notifications::MODELS_DOWNLOAD_PROGRESS` constant (almond-types)

docs/API.md lists the `models.downloadProgress` notification (payload `ModelDownloadProgress`), but
`almond_types::api::notifications` has no constant for it. `capture::models` uses its own
`pub const MODELS_DOWNLOAD_PROGRESS: &str = "models.downloadProgress"`.

Request: add `pub const MODELS_DOWNLOAD_PROGRESS: &str = "models.downloadProgress";` to
`almond_types::api::notifications` (and switch `capture::models` to it).

## 2. `methods::AUDIO_URL` constant (almond-types)

`audio.url` is in docs/API.md but not in `almond_types::api::methods`. Request:
`pub const AUDIO_URL: &str = "audio.url";`.

## 3. Per-channel device state in `RecordingStatus` (almond-types + UI)

When the PC's HDMI audio endpoint disappears (display asleep), the system channel keeps
recording zero-filled (`AudioFrame.synthesized`) frames and is re-opened when the device comes
back; the microphone is unaffected. Today this is only visible as a 0 level on the system meter
and in the log ("audio device lost").

Request (optional, additive): `RecordingStatus.lost: Vec<AudioChannel>` (`#[serde(default)]`),
channels that have delivered only synthesized audio for ≥ 2 s, so the UI can show "system audio
lost — still recording your mic". `capture::recording::Status` already tracks it
(`synth_ms`); filling the field is a one-line change once it exists.

## 4. `RecordingStatus.levels` scale (docs only)

`levels` are documented as "RMS level per channel in [0,1]". Raw speech RMS is ~0.03–0.1, which
barely moves a linear meter, so capture sends RMS mapped from [-60, 0] dBFS to [0, 1] (fast
attack, slower release). Request: document it as "meter level in [0,1] (RMS, -60..0 dBFS)".
