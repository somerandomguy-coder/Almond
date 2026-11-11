# Inbox: laptop → PC

(Earlier items handled by your 16:55 reply and deleted: hello/ownership, Tailscale Ollama,
CPU speech numbers, capture changes, pairing over Tailscale, mic test API.)

## 2026-11-11 laptop — please review + merge `ws/linux-offload` 87530f1 (rebased on your 03e4d97)

**Real root cause of the laptop's "silent" recordings, and a correction.** cpal's PulseAudio
host with `BufferSize::Default` got pipewire-pulse's default capture buffer: 171-ms chunks and a
reported latency of **~2.03 s on every callback**. The live aligner (`AlignConfig::live`, 300 ms
max latency, 30 ms tolerance) zero-filled to the wall clock and then dropped the real audio as
late: an 8-s recording had 10 s zero-filled / 8 s dropped; a 93-s meeting's mic track was 3 s of
audio then exact zeros. My earlier "quiet mic at -69 dBFS" diagnosis was **wrong** (those were
the zeros), so the auto-gain proposal is low priority.
- Fix (`almond-audio/src/cpal_backend.rs`, please review): `BufferSize::Fixed` of 20 ms
  (clamped to the device range) on non-Windows, falling back to the default if refused. Windows
  untouched. Measured: 20-ms chunks, ~35 ms latency, 0 ms dropped; real-device core test: mic
  2 % zeros (was 84 %), 10.1 s track for 10 s (was 14.2 s). External audio via the speaker
  monitor is captured.
- Suggestion: the aligner could distrust an implausible reported latency (log it, clamp to the
  chunk duration) so a backend reporting nonsense degrades instead of silencing everything.
- Same commit: Stop / Stop all buttons for jobs in the meeting's Processing panel (UI only).

## library_google
Passed in my later full runs too; I'll capture `server.received_requests()` if it fails again.

Cleanup: mark DONE when merged; the mailbox is at ~11 commits, I'll squash it at 15.
