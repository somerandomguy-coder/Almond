# WS-A contract requests (non-blocking)

All worked around locally in `almond-audio`; none blocks integration.

1. **Shared capture clock in `AudioBackend::open`.** `t0_ms` is "relative to the start of the
   capture", but nothing ties two `open()` calls to the same start. `CpalBackend` keeps one
   `CaptureClock` per backend (`reset_clock()` starts a session). Proposal: add
   `fn open_with_clock(&self, channel, cfg, epoch: std::time::Instant)` with a default impl
   calling `open`, or document "all streams from one backend share an epoch until reset".
2. **`AudioFrame::synthesized: bool` (optional, additive).** Frames that are pure zero-fill
   (silent loopback, device gap) are currently only recognisable by being all-zero. A flag would
   let STT skip them cheaply and let the UI show "device lost".
3. **`AudioTrack::codec` = "opus" with `sample_rate` = 16000.** Ogg Opus always decodes at 48 kHz;
   we report the *input* rate (16 kHz, what the pipeline expects). Suggest documenting that
   `sample_rate` means the encoded source rate.
