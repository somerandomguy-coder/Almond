# Contract requests: feature `people`

## 1. almond-stt: a public speaker-embedding API (worked around)

`almond_stt::sherpa::SherpaEmbedder` (the 3D-Speaker extractor the diarizer uses) is private, so
`crates/almond-core/src/capture/voice.rs` creates its own `sherpa_onnx::SpeakerEmbeddingExtractor`.
That needed `sherpa-onnx` as an optional dependency of `almond-core` (same version and
`shared` feature as almond-stt's; enabled by the core's `sherpa` feature).

**Request:** expose `pub struct SpeakerEmbedder` in `almond_stt::sherpa` (from a
`ModelManager` + model id, `embed(&[f32]) -> Result<Vec<f32>>`, implementing
`diarize::Embedder`). `voice.rs` would then drop its private copy and almond-core its direct
`sherpa-onnx` dependency.

## 2. Diarization embeddings for free (nice to have)

`transcribe.local` already embeds every ~3 s window of the system channel while diarizing; the
people module decodes the audio and embeds the speakers again after the transcript lands
(~1–3 s per meeting on the PC). If the transcript (or a side output of `transcribe.local`)
carried one mean embedding per diarized speaker (model id + vector), auto-labelling would need
no second pass and would work on devices that receive the transcript without the audio.

## 3. `transcript` speaker names (no change needed, FYI)

`Transcript.speakers[].name` stays `None` from `transcribe.local`; names come from the latest
`speaker_labels` artifact (UI: `lib/people.ts`). LLM processors (summary, action items, recipes)
still see raw ids (`remote:S1`); the UI replaces ids in their text. If processors should write
names, they could read the current `speaker_labels` + `person` records when building prompts.

## 4. Merged people on other devices

`people.merge` rewrites the labels it can see and deletes the removed person. A device that
labels with the removed person's id before syncing the merge leaves a dangling label (the UI
falls back to the speaker id). A `merged_into: Option<Id>` field on `Person` (tombstone with a
redirect) would let every device resolve it; needs an `almond-types` change.
