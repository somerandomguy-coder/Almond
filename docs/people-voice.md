# Voice prints: calibration of the match threshold

Feature `people` recognises speakers across meetings by comparing voice prints
(`crates/almond-core/src/capture/voice.rs`): 3D-Speaker ERes2Net-base embeddings
(`sherpa:3dspeaker-eres2net-base`, the diarization model), the mean of the L2-normalised
embeddings of a speaker's segments, normalised again. A speaker is labelled as the person
whose prints have the highest cosine similarity, if it is **≥ 0.5** (setting
`people.voice_threshold`) and **≥ 0.05 ahead** of the runner-up.

## Measurement (2026-11-12, Windows PC)

`crates/almond-core/tests/people_voice_real.rs` (ignored): each clip's speech is cut into ~3 s
VAD windows, split into two halves, and each half becomes one print exactly as the app makes
them. Clips (not in the repo):

- **nam**: the mic tracks of four of the user's own test recordings (Vietnamese/English, laptop
  mic), 11–131 s of speech each;
- **sysA/B/C**: the system (remote) tracks of three of those recordings (unknown speakers);
- **david / zira / huihui**: two clips each of the Windows SAPI voices (English, 16 kHz), as
  extra distinct speakers.

Prints are 5–74 s of speech (shorter than real meeting prints, which use up to 90 s).

| Pairs | n | min | mean | max |
|---|---|---|---|---|
| same speaker | 49 | 0.474 | 0.773 | 0.995 |
| different speakers | 276 | −0.026 | 0.176 | 0.591 |

Details:
- Same real speaker across different recordings (nam × nam): 0.47–0.85, mean ≈ 0.66; the
  pairs under 0.55 all involve a print of only 5–7 s. Longer prints agree more (74 s vs 57 s:
  0.85).
- Real speakers vs other real speakers across recordings: ≤ 0.42. The highest "different"
  real-voice values (0.54–0.55) are a mic track vs the system track **of the same recording**:
  the remote voice leaks into the laptop mic, so these prints are mixed.
- Synthetic voices: same voice 0.92–0.99; two female SAPI voices of the same engine
  (zira vs huihui) 0.49–0.59 — the worst different-speaker case; male vs female ≤ 0.24.

## Choice

**Threshold 0.5, margin 0.05.** It accepts the bulk of same-speaker pairs (mean 0.77) and
rejects every cross-recording pair of different real speakers seen (≤ 0.42). Risks:
- prints from very short speech (< 8 s) of the same person can fall just under 0.5 → no
  suggestion (the user labels once more; each label adds a print, and the best print counts);
- very similar voices can reach 0.5–0.6 → a wrong *suggestion*. Suggestions are shown dimmed
  with a confirm tick and never override the user's labels; the margin rule drops ambiguous
  ones when both people are known.

To re-run: see the doc comment of `people_voice_real.rs` (`ALMOND_VOICE_CLIPS=speaker=path;…`).
