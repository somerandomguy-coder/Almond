# STT evaluation — results and default models (WS-B)

Measured 2026-11-09/11 on the dev PC: Windows 11, RTX 5060 Ti 16 GB, 12 logical CPUs, 31 GB RAM.
Everything here is reproducible with the scripts in this directory (see "Reproduce" at the end).
Datasets are downloaded at run time into `~/.almond/eval/stt/` and are **not** committed.

## TL;DR — recommended defaults

| Device class | Vietnamese (offline / post-meeting) | Vietnamese (live captions) | English (offline) | English (live) | Lang-ID | Diarization |
|---|---|---|---|---|---|---|
| **PC, 16 GB GPU** | `sherpa:zipformer-vi-30m-int8` *(PhoWhisper via whisper.cpp is broken, see GPU runs)* | `sherpa:zipformer-vi-30m-int8` partials, finals re-decoded per language (`live::LiveRouter`) | `whisper:large-v3-turbo` | `whisper:large-v3-turbo` (greedy partials, beam-5 finals) | `whisper:large-v3-turbo` + `langid::LidPolicy` (vi prior, short utterances follow context) | 3D-Speaker window embeddings + clustering (`diarize::FastDiarizer`, see Phase 2) |
| **Fedora laptop, CPU** | `sherpa:zipformer-vi-30m-int8` | `sherpa:zipformer-vi-30m-int8` | `whisper:small` *(or offload to PC)* | `whisper:small` | `whisper:small` | same (RTF ≈ 0.15–0.25 on CPU) |
| **Samsung A26** | `sherpa:zipformer-vi-30m-int8` | `sherpa:zipformer-vi-30m-int8` | offload to PC | — | — | offload to PC |

Why:
- **Zipformer-vi-30M (int8, 26 MB) is the surprise winner on CPU**: best VIVOS WER of everything
  we ran (4.5 %), best FLEURS-vi WER on sentences without digits (4.3 %), and ~40× faster than
  real time on 4 CPU threads (RTF 0.023–0.027). It is also small enough for the phone. Its
  weaknesses: no punctuation/casing (all caps output) and digits are spelled out, which costs it
  on FLEURS sentences that contain numbers (22.7 % WER on those vs 11 % for Whisper-turbo).
- **PhoWhisper-medium** matches Zipformer-30M on VIVOS (4.9 %) and adds punctuation and casing,
  which matters for summaries; it is the quality default on the GPU PC. PhoWhisper-large could not
  be measured (see "Not measured"), so the routing table uses the measured `phowhisper-medium`
  instead of the `phowhisper-large` named in ARCHITECTURE §10; revisit once large is run.
- **whisper-large-v3-turbo is not a Vietnamese model**: 20.7 % WER on VIVOS (conversational,
  regional accents) vs 4.5–6.3 % for the Vietnamese-tuned models — but it is the best on read
  FLEURS-vi sentences with numbers, and the right model for English (5.4 % FLEURS-en) and for
  language ID.
- On a CPU-only machine Whisper "large" class models are too slow (see CPU table): use Zipformer
  for Vietnamese and `whisper:small` (or offload to the PC) for English.

`crates/almond-stt/src/routing.rs` encodes these defaults (`RoutingTable::default_for`).

## Accuracy and speed

WER/CER: normalized with `almond_stt::metrics` (Unicode NFC, lowercase, punctuation → space;
**Vietnamese diacritics kept** — a tone error is an error). CER excludes spaces. RTF = processing
time / audio duration (lower is faster), batch size 1, sequential utterances, after one warm-up.

### VIVOS test (760 utt, 19 speakers, 44.8 min, read Vietnamese, southern accents)

| Model | Runtime | WER % | CER % | RTF |
|---|---|---:|---:|---:|
| sherpa `zipformer-vi-30m-int8` (26 MB) | sherpa-onnx CPU, 4 threads | **4.52** | **2.50** | **0.020–0.027** |
| PhoWhisper-medium (769 M) | HF transformers fp16, CUDA, beam 5 | 4.86 | 2.65 | 0.90 ¹ |
| sherpa `zipformer-vi-int8` (2025-04-20, 60 MB) | sherpa-onnx CPU, 4 threads | 6.27 | 3.82 | 0.027 |
| PhoWhisper-small (244 M) | HF transformers fp16, CUDA, beam 5 | 6.27 | 3.27 | 0.25 ¹ |
| whisper-large-v3-turbo (809 M) | HF transformers fp16, CUDA, beam 5 | 20.66 | 16.22 | 0.17 ¹ |

### FLEURS vi_vn test (200 random utt, 41.3 min, read Wikipedia sentences)

| Model | Runtime | WER % | CER % | WER % (40 utt with digits) | WER % (160 utt without digits) | RTF |
|---|---|---:|---:|---:|---:|---:|
| sherpa `zipformer-vi-30m-int8` | CPU, 4 thr | **8.09** | 6.14 | 22.7 | **4.3** | 0.019 |
| whisper-large-v3-turbo | HF fp16 CUDA, beam 5 | 8.30 | **5.45** | **11.1** | 7.6 | 0.11 ¹ |
| sherpa `zipformer-vi-int8` | CPU, 4 thr | 9.29 | 7.89 | 23.4 | 5.7 | 0.023 |
| PhoWhisper-medium | HF fp16 CUDA, beam 5 | 9.89 | 7.64 | 23.2 | 6.4 | 0.32 ¹ |
| PhoWhisper-small | HF fp16 CUDA, beam 5 | 11.39 | 8.26 | 25.5 | 7.6 | 0.23 ¹ |

FLEURS references keep digits ("10.000 năm"); Zipformer and PhoWhisper write numbers as words,
which the normalizer does not reconcile. The digit/no-digit split shows that this, not
recognition, is most of their gap. A number normalizer for Vietnamese is an open item.

### FLEURS en_us test (200 random utt, 33.5 min)

| Model | Runtime | WER % | CER % | RTF |
|---|---|---:|---:|---:|
| whisper-large-v3-turbo | HF fp16 CUDA, beam 5 | 5.44 | 2.45 | 0.065 ¹ |

¹ **GPU RTFs are pessimistic and only comparable to each other**: (a) they are from Hugging Face
`generate()` at batch 1 with beam 5, which is far slower than whisper.cpp/faster-whisper; (b) the
GPU was shared with Ollama (`llama-server`, ~13 GB VRAM, up to 97 % utilization) from the parallel
summarization workstream during all GPU runs. PhoWhisper-medium in particular (RTF 0.90) was
measured while the GPU was busiest. Expect whisper.cpp CUDA to be several times faster; it could
not be built on this PC yet (see "Not measured").

### whisper.cpp on CPU (this crate's engine) — the Fedora-laptop proxy

Run in WSL2 Ubuntu 24.04 on the same 12-thread CPU (no GPU), `--features whisper,sherpa`.
Accuracy of the whisper.cpp engine is covered by the meeting run below (English via
`whisper:small`: 9.2 % WER); a clean per-utterance whisper.cpp WER table is **missing** — the WSL
VM stopped responding (`HCS_E_CONNECTION_TIMEOUT`) before the final CPU sweep and was not
restarted to avoid disturbing other workstreams. Speed observations:

| Model | Measurement | RTF |
|---|---|---:|
| `whisper:large-v3-turbo-q5_0` | 20 FLEURS-en utt (193 s), 12 threads, beam 5, language given | **3.2** (too slow for a CPU laptop) |
| `whisper:small` + LID `whisper:small` | system channel of the meeting (≈ 22 min speech, 60 EN turns via Whisper, 200 VI turns via Zipformer, LID on all 260) | 0.76 for the whole channel |
| `sherpa:zipformer-vi-30m-int8` + LID `whisper:small` | mic channel of the meeting (40 VI turns) | 0.09 |

(The turbo WER from that run is not reported: it predates the VAD fix and dropped quiet audio.)

### Live captions (pseudo-streaming, LocalAgreement-2)

3-minute excerpt of the synthetic meeting (mixed channel), replayed in real time through
`SttEngine::start_stream` (`examples/live.rs`), Zipformer-30M, Windows CPU:

| Setting | first shown (p50 / p90) | final (p50 / p90) | partial updates | finals |
|---|---:|---:|---:|---:|
| max segment 15 s, pause 800 ms | — | 7.9 s / 15.6 s | 354 | 21 |
| **max segment 8 s, pause 500 ms (default)** | **0.42 s / 1.6 s** | 4.8 s / 8.9 s | — | 34 |

"First shown" = time from the end of a word's audio until it first appears (as a grey partial);
"final" = until its segment is committed. Decoding cost on CPU: RTF ≈ 0.02 per pass × 2 passes/s.

## End-to-end: 30-minute VI/EN meeting with speakers ("Done when")

`make_meeting.py` builds a 29.7-min two-channel "online meeting" from the eval sets: left = mic
(one VIVOS speaker = "me", 40 utterances), right = system loopback (5 other VIVOS speakers, 200
utterances, plus 60 English FLEURS utterances), turn-taking with 0.25–2 s gaps, light noise,
each utterance level-normalized to -26 dBFS (conference AGC; raw FLEURS-en is 30 dB quieter than
VIVOS). `examples/transcribe.rs --stereo-mic-system` produces a `transcript` artifact body;
`score_meeting.py` scores it per reference utterance (hypothesis words assigned by time).

Final run — the **CPU / Fedora-laptop configuration**, entirely through this crate (WSL2, CPU only):
vi → `sherpa:zipformer-vi-30m-int8`, en → `whisper:small`, language ID `whisper:small` per
utterance, diarization sherpa (threshold 1.0) on the system channel, mic = "me":

| Metric | Result |
|---|---|
| WER Vietnamese (2 569 ref words) | **6.70 %** (CER 4.32 %) |
| WER English (1 334 ref words) | **9.15 %** (CER 4.46 %) |
| WER overall | **7.76 %** |
| Language ID per utterance | **100 %** vi (240/240), **100 %** en (60/60) |
| Mic utterances labelled "me" | **100 %** (40/40) |
| Remote VI utterances with the right speaker (transcript segments) | **91.5 %** (200), 7 labels for 5 speakers |
| Output | 276 segments, 17 speaker ids (5 VI + "me" + 11 for the many FLEURS voices) |
| Time (CPU, 12 threads, partly shared with other jobs) | mic 160 s, system 1 680 s, diarize 411 s → **RTF 1.26** |

On CPU the cost is dominated by Whisper on the English turns and per-utterance language ID on
every system-channel turn (≈ 260 encoder passes). On the GPU PC both are cheap; on the laptop
prefer offloading the post-meeting pass to the PC (ARCHITECTURE §5 jobs) and keep Zipformer for
live captions.

### GPU runs (Windows, whisper.cpp CUDA, added 2026-11-10 after LLVM/libclang was installed)

Built with `scripts/cargo-cuda.cmd build -p almond-stt --release --features cuda,sherpa --examples`.
Same meeting, same scorer; GPU otherwise idle.

| Config | WER vi | WER en | WER all | Speaker (remote VI) | Time | RTF |
|---|---:|---:|---:|---:|---|---:|
| **vi Zipformer-30M (CPU), en + LID `large-v3-turbo` (GPU)** — new PC default | **6.54 %** | **8.47 %** | **7.69 %** | 92.0 % | mic+system 66 s, diarize 271 s → **5.6 min** | **0.19** |
| vi `phowhisper-medium` (GPU), en + LID turbo (GPU) | 84.7 % ² | 8.47 % | — | 97.5 % | mic+system 162 s, diarize 263 s → 7.1 min | 0.24 |

² Not a recognition failure: the Vietnamese *text* is mostly right, but PhoWhisper via whisper.cpp
emits a junk first word per utterance ("doctoral", "hashtag", "like" …) and its word timestamps run
past the utterance (PhoWhisper was fine-tuned without timestamp tokens), so 1 678 words land
outside any reference utterance. Until that is fixed (suppress the first token / use segment-level
times), the PC default for Vietnamese is Zipformer-30M. *(Fixed 2026-11-11: no-timestamp
decoding, see "PhoWhisper through whisper.cpp: junk first token fixed"; Zipformer stays the
default on accuracy.)*

**Diarization is now ~80 % of the time.** It runs after transcription on CPU; running it concurrently
with transcription and/or on the CUDA execution provider is the next speed-up.

### Phase 2: through the core (`transcribe.local`), fast diarization, concurrent pass (2026-11-10)

Same meeting, imported as the Opus tracks a recording produces, run by the `transcribe.local`
processor in `almond-core` (`post_meeting_pass_30min`, ignored test, `--features cuda,sherpa`):

| Config | WER vi | WER en | WER all | LID | "me" | Speaker (remote VI) | Time |
|---|---:|---:|---:|---:|---:|---:|---|
| vi Zipformer-30M, en + LID turbo (GPU), **fast diarizer concurrent** | **6.46 %** | **6.90 %** | **6.74 %** | 100 % | 100 % | **94.5 %** | mic 10 s, system 67 s, diarize 21 s in parallel → **77 s** (82 s incl. decode + model load), RTF 0.046 |
| same + **LID policy, decode checks, hallucination filter** (2026-11-11, see "Vietnamese + English") | **6.27 %** | **6.75 %** | **6.64 %** | 100 % | 100 % | 94.5 % | mic 10 s, system 64 s, diarize 21 s → 74 s (203 s with the CPU busy compiling) |

- Diarization: `almond_stt::diarize::FastDiarizer` (VAD windows of ~3 s, one 3D-Speaker
  embedding each, 4 parallel extractors, average-linkage clustering at cosine distance 0.6, small
  clusters folded) replaces the sherpa pyannote pipeline (which embeds every second ~10 times).
  System channel alone (`examples/diarize.rs`, `score_diarization.py`): **45.8 s vs 270 s**, VI
  utterances attributed 100.0 % (vs 99.5 %), 1 label per true VI speaker (vs 1–2). `--pyannote`
  keeps the old path; it is also used when a speaker count is given.
- It runs concurrently with transcription (`router::transcribe_meeting_concurrent`).
- Timing depends heavily on the shared PC: with other agents compiling (CPU at 100 %) and the GPU
  shared, the same pass took 352–389 s (transcription of the system channel 254 s); on an idle GPU
  82 s. The 4.5-min target holds when the GPU is not busy with Ollama.

What went wrong on the way (all fixed and covered by tests/notes):
- 22/60 English turns were lost when a quiet English turn and a neighbouring Vietnamese turn
  merged into one VAD region, were identified as Vietnamese and sent to Zipformer → the router now
  splits at 200 ms gaps and the VAD threshold never drops below 1.5× the noise floor.
- whisper.cpp skipped later utterances inside packed 28 s windows (early end-of-text) → offline
  Whisper decodes per utterance (`WhisperConfig::pack_utterances = false`).
- An absolute VAD floor dropped very quiet recordings entirely (FLEURS at -54 dBFS).

### Diarization (system channel only; mic = "me" by channel prior)

sherpa-onnx pyannote-segmentation-3.0 + 3D-Speaker ERes2Net-base, fast clustering, scored with
`score_diarization.py` (each hypothesis label mapped to the reference speaker it covers most):

| Cluster threshold | Speakers found | Labels per true VI speaker | VI utterances attributed correctly | RTF (CPU) |
|---|---:|---|---:|---:|
| 0.5 (sherpa default) | 89 | 7–12 | 99.0 % (trivial: over-split) | 0.25 (4 thr) |
| 0.8 | 29 | 2–4 | 99.5 % | 0.16 (10 thr) |
| **1.0 (our default)** | 15 | **1–2** | **99.5 %** | 0.15 (10 thr) |

(Sweep on the first version of the meeting, before level normalization. The 60 English utterances
come from many different FLEURS voices, so >5 speakers is expected.)
Channel prior: mic utterances labelled "me": **100 %**; mic echo of system audio (speakers instead
of headphones) is removed by time-overlap + text-similarity (`transcript::drop_echo`, unit-tested;
not exercised by this synthetic set, which has no echo).

## Vietnamese + English: language routing, hallucinations, live captions, code-switching (2026-11-11)

Why: the user's first real test (speaking Vietnamese and English into a mic, sometimes in one
sentence) routed most Vietnamese utterances to English, where Whisper hallucinated ("No, no, no,
no, no.", "A40.", "Hmm."), and live captions were Vietnamese-only (English came out as garbled
Vietnamese). Tools: `examples/lid_dump.rs` (Whisper LID probabilities and vi/en decode scores per
clip, at crop lengths 0.5–3 s), `examples/lang_eval.rs` (test sets built from VIVOS + FLEURS-en,
see its docs). All runs: RTX 5060 Ti, `--features cuda,sherpa`, GPU partly shared.

### What Whisper LID does on real mic audio

The two recordings from the user's test (47 mic utterances, 0.5–4 s, -40 to -55 dBFS, casual
northern Vietnamese with some English) were run through `lid_dump --wav`: `whisper:large-v3-turbo`
gives **p(vi) < 0.1 to 32 of the 47 utterances** (< 0.5 to 40), including clearly Vietnamese 1–3 s ones
("MỌI NGƯỜI THẤY NÓ CÁI NÀY KHÔNG" p(vi) 0.01, "XONG RỒI XONG RỒI CHẮC LÀ…" 0.01, Zipformer's
transcripts). Level normalization to -26 dBFS does not change this. On read speech it is fine:

| LID accuracy per crop length (1 035 vi rows: VIVOS + FLEURS-vi; 599 en rows: FLEURS-en) | 0.5 s | 1 s | 1.5 s | 2 s | 3 s | whole |
|---|---:|---:|---:|---:|---:|---:|
| vi, raw argmax (old) | 58.3 | 89.8 | 98.9 | 99.4 | 100 | 100 |
| vi, **policy** (new default) | **100** | **95.2** | **99.4** | 100 | 100 | 100 |
| en, raw argmax (old) | 100 | 100 | 100 | 100 | 100 | 100 |
| en, **policy** | 0 ¹ | 100 | 100 | 100 | 100 | 100 |

¹ By design: below 400 ms an utterance's language comes from its neighbours / the prior.

So the fix cannot rely on better LID probabilities; it has to (a) distrust them for short
utterances and favour the meeting's primary language, and (b) check the result of decoding.

### Language routing policy (`langid::LidPolicy`, `router::RoutedEngine`)

* Viterbi over the meeting's utterances: prior 0.75 for the primary language (first of
  `allowed_languages` = meeting language, else vi), LID log-probabilities weighted by duration
  (0 below 0.4 s, full at 2 s), probabilities floored at 0.02, switching language costs 1 nat
  decaying with the pause (e^-gap/3 s).
* After decoding, each utterance of a secondary-language window is re-decoded with the primary
  engine when Whisper kept nothing for it (all hallucination-filtered), when a segment's mean word
  probability is < 0.55, or when it got fewer than 0.7 words per second of speech (Whisper told
  "en" answered 7.7 s of Vietnamese with "oh"). The 0.55 is a trade-off: on the user's recordings
  the misrouted Vietnamese that survived the policy decoded at 0.50–0.54 and their real English
  at 0.60–0.75; on read FLEURS-en, 0.55 sends about 40 % of 1 s clips and 10 % of 1.5 s clips to
  Zipformer (0.6: 47 % / 14 %), and none of the full sentences.
Results:

| Set | Old (argmax) | New (policy + checks) |
|---|---|---|
| User's recordings, mic, 47 utterances: routed to English | 15, ~10 of them Vietnamese judging by Zipformer / Whisper-vi output, decoded as "I'm going to go to the next one", "I'm going to go home", "Yes, my", "Bye bye", "in the retomary guide", "I'm not good. I'm not good.", "oh" (for 7.7 s of speech) | 10: the English "So what if I do with English right now?", "English, English.", "I have to use description." (probably "…captions / transcription"), the code-switched last sentence, fillers ("Hmm.", "Yeah.", "Hello.") and ~3 still Vietnamese ("All the time.", "I love you again.", "I'm going to use it, you know?") || Synthetic conversation, 60 utt (42 vi / 18 en, 20 of them 1–3-word clips 0.3–1.6 s) — LID long vi / long en | 28/28, 12/12 | 28/28, 12/12 |
| same — LID short vi / short en | 8/14, 5/6 | 11/14, 2/6 |
| same — WER vi / en | 10.8 % / 8.7 % | 11.9 % / 10.6 % |
| `turbo-auto` (Whisper LID among all languages, decode with Whisper) — LID short vi | 5/14, WER vi 32 % | |

On clean read speech the old argmax was already right, so the policy costs a little there
(short English is now mostly sent to Zipformer); it is the real mic recordings where it matters.
The synthetic set cannot reproduce Whisper's failure on real casual speech — the main limitation
of these numbers.

### Hallucination filter (`hallucination::reason`, whisper.cpp thresholds set explicitly)

Drops a segment when: known outro phrase; no-speech > 0.6 with low confidence; ≥ 4 words with
≤ 1/3 distinct ("No, no, no, no, no."); < 15 % of its audio voiced; ≤ 3 words with mean token
log-prob < -1 ("A40.", "C40") or a filler ("hmm", "thank you", "cảm ơn" …) with log-prob < -0.5 /
no-speech > 0.2. Word loops inside long segments are cut to two repetitions.

| Whisper decodes | kept by old filter | kept by new filter |
|---|---:|---:|
| User's mic utterances decoded as English (the misrouting case), 47 | 47 | 30 |
| same decoded as Vietnamese, 47 | 33 | 28 |
| VIVOS, Vietnamese, 127 (correct speech) | 127 | 127 |
| FLEURS-en, English, 100 (correct speech) | 100 | 100 |
| VIVOS decoded as English, 127 | 127 | 124 |

(Python port of the rules without the voiced-audio check; the Rust filter can only drop more.)
What remains are confident *translations* (Vietnamese audio, English text) — the routing policy,
not the filter, is the fix for those.

### Live captions (`live::LiveRouter`)

Partials still come from the fast model (Zipformer for Vietnamese; Whisper greedy after an English
final), but every **final is re-decoded**: the segment's audio is split at 200 ms pauses, each part
gets a language from the online policy (continuing from earlier finals), and each language run is
decoded by that language's offline model (Zipformer / Whisper-turbo beam 5). Same `SttUpdate`
protocol: the refined final replaces the partials of its index. Audio already covered by the
previous final is not decoded again, and a repeated overlap with the previous final is dropped.

The 60-utterance conversation (3:47 min) replayed in real time, GPU otherwise idle:

| Config | first shown p50 / p90 | final p50 / p90 | WER vi | WER en | LID long vi / en ² |
|---|---:|---:|---:|---:|---:|
| Zipformer only (old default) | 0.31 / 1.49 s | 2.70 / 5.71 s | 17.9 % | 92.1 % | 28/28, 0/12 |
| routed, LID on every partial | 0.45 / 3.34 s | 2.18 / 6.38 s | 17.9 % | 38.9 % | 25/28, 10/12 |
| **routed, partials in the previous final's language (default)** | 0.51 / 2.86 s | 2.13 / 8.99 s | **13.4 %** | **37.4 %** | 28/28, 10/12 |

² A final carries one language; a final that contains both languages counts its minority part as
a LID miss, so live LID is understated.

English live WER is still high (37 %): English partials are Zipformer gibberish whose word timing
is useless for LocalAgreement's segmentation, so English finals are cut in odd places and lose
words at the cuts; the offline pass afterwards gets English right (10.6 %). LID on every partial
fixed some of that but broke Vietnamese segmentation, so it is off by default
(`LiveRouter::lid_partials`).

### Code-switching inside one utterance

40 utterances, each a VIVOS clip and a FLEURS-en clip joined with a 0–150 ms gap (tight trims),
alternating order; WER on the joined reference:

| Config | WER all | first part | second part |
|---|---:|---:|---:|
| `zip` (Zipformer only) | 69.6 % | 70.6 % | 71.1 % |
| `turbo-vi` (Whisper, language vi) | 30.1 % | 39.2 % | 27.9 % |
| `turbo-auto` (Whisper LID per utterance) | 28.9 % | 47.0 % | 22.6 % |
| `turbo-en` (Whisper, language en) | **18.2 %** | 26.8 % | 17.2 % |
| **`routed` (default)** | 21.4 % | 31.4 % | 16.6 % |
| `pho` (PhoWhisper-medium, no-timestamp mode) | 77.5 % | 85.2 % | 88.8 % |

* Whisper-turbo keeps each language in its own script whatever language token it is given
  ("Khi nhân viên cứu hộ… Giancarlo Fisicella lost control…"), so a code-switched sentence decoded
  whole by Whisper reads well — but its Vietnamese is poor (20.7 % WER on VIVOS), which dominates
  in a Vietnamese-first meeting.
* The routed default splits at pauses ≥ 200 ms (most joins here); within one breath group it
  sends everything to one engine, so a short English phrase inside Vietnamese comes out phonetic
  from Zipformer ("LIVE STRANCIFT"). **Nothing measured is good at true intra-sentence switching**;
  routed is the best default for Vietnamese-primary meetings, Whisper-turbo (any language token)
  would be the choice for English-heavy mixing.
* PhoWhisper-medium translates the English half into Vietnamese — unusable for mixed speech. With
  `<|notimestamps|>` decoding (the fix for its junk first token, see below) its Vietnamese is
  19.0 % WER on the conversation vs 11.9 % routed.

### PhoWhisper through whisper.cpp: junk first token fixed

PhoWhisper was fine-tuned without timestamp tokens; decoding it in timestamp mode produced the
junk first word and the out-of-range word times measured in "GPU runs" above. `WhisperModel` now
decodes PhoWhisper with `<|notimestamps|>` + one segment per VAD utterance and spreads word times
over the utterance. VIVOS, first 150 utterances, same scorer:

| Model | WER | CER | insertions | RTF |
|---|---:|---:|---:|---:|
| `whisper:phowhisper-medium`, whisper.cpp CUDA, beam 5, no-timestamps | 8.15 % | 4.77 % | 0 | 0.094 |
| `sherpa:zipformer-vi-30m-int8`, CPU 4 threads | **7.15 %** | **3.80 %** | 1 | 0.020 |

The junk word is gone (0 insertions), and PhoWhisper adds punctuation and casing, but it is still
not more accurate than Zipformer-30M and it translates English into Vietnamese, so the routing
default for Vietnamese stays Zipformer-30M.

## Not measured / blocked (exactly what is missing)

1. **whisper.cpp with CUDA on Windows** — whisper.cpp + ggml-cuda itself compiles (needs MSVC
   v143 host compiler, Ninja, short target dir: `crates/almond-stt/scripts/cargo-cuda.cmd`), but
   `whisper-rs-sys` then needs **libclang** for bindgen: its bundled `bindings.rs` was generated on
   Linux (`int64_t` → `c_long`, 32-bit on Windows) and fails layout asserts. Installing LLVM was
   declined on this PC, so all whisper.cpp numbers are CPU (WSL). With LLVM installed
   (`winget install LLVM.LLVM`), run `cargo-cuda.cmd run --release -p almond-stt --features cuda,sherpa --example stt_eval ...`.
2. **PhoWhisper-large** — conversion script works (small and medium converted), but the 6.2 GB
   fp32 checkpoint could not be downloaded: the disk ran out (other workstreams share it; 2 GB free
   at the time). Run `python eval/stt/convert_phowhisper.py large` + `hf_eval.py --model vinai/PhoWhisper-large`.
3. **Common Voice vi** — gated on Hugging Face (needs a token + terms acceptance); used FLEURS-vi
   instead.
4. **VI–EN code-switching LoRA** (PhoWhisper-large based) — not evaluated (depends on 2; also no
   public code-switching test set was found that is small and freely downloadable).
5. **Real meetings / user recordings** — only the two short recordings of the user's first
   test (47 mic utterances, no reference text; used qualitatively above, not committed); `eval/stt/private/` (git-ignored) is
   where they go; `prepare_data.py`-style manifest + `stt_eval` works on them.
6. **Per-utterance whisper.cpp CPU WER table** (PhoWhisper-small/medium, whisper-small, turbo-q5
   via this crate) — WSL VM hung before the run; command:
   `crates/almond-stt/scripts/wsl-cargo.sh run --release -p almond-stt --features whisper,sherpa --example stt_eval -- --model whisper:phowhisper-small --manifest <vivos manifest> --limit 50`.
   The converted PhoWhisper-small ggml was verified to transcribe correctly through whisper.cpp.
7. Phone (Samsung A26) speed — not measured (no Android toolchain yet); Zipformer-30M int8 is
   26 MB and runs at RTF 0.023 on 4 desktop threads, so it should be comfortably real-time.

## Reproduce

```sh
# data (≈ 2.5 GB downloads; writes ~/.almond/eval/stt/<set>/)
python eval/stt/prepare_data.py vivos-test
python eval/stt/prepare_data.py fleurs-vi fleurs-en --limit 200
python eval/stt/make_meeting.py                       # synthetic 30-min meeting

# models (~/.almond/models; resumable, SHA-256 verified)
cargo run -p almond-stt --example models -- get sherpa:zipformer-vi-30m-int8 whisper:large-v3-turbo ...
python eval/stt/convert_phowhisper.py small|medium|large

# Rust engines (Windows: sherpa; Linux/WSL: whisper too, see scripts/wsl-cargo.sh)
cargo run --release -p almond-stt --features sherpa --example stt_eval -- \
    --model sherpa:zipformer-vi-30m-int8 --manifest ~/.almond/eval/stt/vivos-test/manifest.jsonl
crates/almond-stt/scripts/wsl-cargo.sh run --release -p almond-stt --features whisper,sherpa \
    --example transcribe -- meeting_stereo.wav --stereo-mic-system --out transcript.json
python eval/stt/score_meeting.py transcript.json
cargo run --release -p almond-stt --features sherpa --example diarize -- meeting_stereo.wav --channel 1 --out turns.json
python eval/stt/score_diarization.py turns.json
cargo run --release -p almond-stt --features sherpa --example live -- --model sherpa:zipformer-vi-30m-int8 clip.wav

# Vietnamese + English (GPU; Windows: crates\almond-stt\scripts\cargo-cuda.cmd run ...)
cargo run --release -p almond-stt --features cuda,sherpa --example lid_dump -- \
    --manifest ~/.almond/eval/stt/vivos-test/manifest.jsonl --every 6 --crops 500:1000:1500:2000:3000:0 --decode
cargo run --release -p almond-stt --features cuda,sherpa --example lid_dump -- --wav mic.wav   # per VAD region
cargo run --release -p almond-stt --features cuda,sherpa --example lang_eval -- \
    --sets conv:mixed:live --configs routed:routed-raw:zip:turbo-vi:turbo-en:turbo-auto:pho:live-zip:live-routed:live-routed-nolidpartials
cargo run --release -p almond-stt --features cuda,sherpa --example transcribe -- mic.wav --vi sherpa:zipformer-vi-30m-int8 --no-diarize [--raw-lid]

# GPU reference (transformers)
python eval/stt/hf_eval.py --model vinai/PhoWhisper-medium --set vivos-test
python eval/stt/summarize.py      # markdown table of all runs
```
