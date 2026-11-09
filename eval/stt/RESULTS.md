# STT evaluation — results and default models (WS-B)

Measured 2026-11-09/10 on the dev PC: Windows 11, RTX 5060 Ti 16 GB, 12 logical CPUs, 31 GB RAM.
Everything here is reproducible with the scripts in this directory (see "Reproduce" at the end).
Datasets are downloaded at run time into `~/.almond/eval/stt/` and are **not** committed.

## TL;DR — recommended defaults

| Device class | Vietnamese (offline / post-meeting) | Vietnamese (live captions) | English (offline) | English (live) | Lang-ID | Diarization |
|---|---|---|---|---|---|---|
| **PC, 16 GB GPU** | `whisper:phowhisper-medium` *(re-check vs `-large` once measured)* | `sherpa:zipformer-vi-30m-int8` | `whisper:large-v3-turbo` | `whisper:large-v3-turbo` (greedy) | `whisper:large-v3-turbo` | sherpa pyannote-3.0 + 3D-Speaker, threshold 1.0 |
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
| sherpa `zipformer-vi-30m-int8` (26 MB) | sherpa-onnx CPU, 4 threads | **4.53** | **2.52** | **0.027** |
| PhoWhisper-medium (769 M) | HF transformers fp16, CUDA, beam 5 | 4.86 | 2.65 | 0.90 ¹ |
| sherpa `zipformer-vi-int8` (2025-04-20, 60 MB) | sherpa-onnx CPU, 4 threads | 6.27 | 3.82 | 0.034 |
| PhoWhisper-small (244 M) | HF transformers fp16, CUDA, beam 5 | 6.27 | 3.27 | 0.25 ¹ |
| whisper-large-v3-turbo (809 M) | HF transformers fp16, CUDA, beam 5 | 20.66 | 16.22 | 0.17 ¹ |

### FLEURS vi_vn test (200 random utt, 41.3 min, read Wikipedia sentences)

| Model | Runtime | WER % | CER % | WER % (40 utt with digits) | WER % (160 utt without digits) | RTF |
|---|---|---:|---:|---:|---:|---:|
| sherpa `zipformer-vi-30m-int8` | CPU, 4 thr | **8.06** | 6.12 | 22.7 | **4.3** | 0.023 |
| whisper-large-v3-turbo | HF fp16 CUDA, beam 5 | 8.30 | **5.45** | **11.1** | 7.6 | 0.11 ¹ |
| sherpa `zipformer-vi-int8` | CPU, 4 thr | 9.29 | 7.89 | 23.4 | 5.7 | 0.029 |
| PhoWhisper-medium | HF fp16 CUDA, beam 5 | 9.89 | 7.64 | 23.2 | 6.4 | 0.32 ¹ |
| PhoWhisper-small | HF fp16 CUDA, beam 5 | 11.39 | 8.26 | 25.5 | 7.6 | 0.23 ¹ |

FLEURS references keep digits ("10.000 năm"); Zipformer and PhoWhisper write numbers as words,
which the normalizer does not reconcile. The digit/no-digit split shows that this, not
recognition, is most of their gap. A number normalizer for Vietnamese is an open item.

### FLEURS en_us test (200 random utt, 33.5 min)

| Model | Runtime | WER % | CER % | RTF |
|---|---|---:|---:|---:|
| whisper-large-v3-turbo | HF fp16 CUDA, beam 5 | 5.44 | 2.45 | 0.065 ¹ |
| whisper `large-v3-turbo-q5_0` | **whisper.cpp (this crate)**, CPU, WSL, 12 thr | see CPU table | | |

¹ **GPU RTFs are pessimistic and only comparable to each other**: (a) they are from Hugging Face
`generate()` at batch 1 with beam 5, which is far slower than whisper.cpp/faster-whisper; (b) the
GPU was shared with Ollama (`llama-server`, ~13 GB VRAM, up to 97 % utilization) from the parallel
summarization workstream during all GPU runs. PhoWhisper-medium in particular (RTF 0.90) was
measured while the GPU was busiest. Expect whisper.cpp CUDA to be several times faster; it could
not be built on this PC yet (see "Not measured").

### whisper.cpp on CPU (this crate's engine) — the Fedora-laptop proxy

Run in WSL2 Ubuntu 24.04 on the same 12-thread CPU (no GPU), `--features whisper,sherpa`.

| Model | Set | n | WER % | RTF (12 threads, beam 5) |
|---|---|---:|---:|---:|
| _filled in below by the final run_ | | | | |

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
utterances, plus 60 English FLEURS utterances), turn-taking with 0.25–2 s gaps, light noise.
`examples/transcribe.rs --stereo-mic-system` produces a `transcript` artifact body;
`score_meeting.py` scores it per reference utterance (hypothesis words assigned by time).

_Results filled in below by the final run._

### Diarization (system channel only; mic = "me" by channel prior)

sherpa-onnx pyannote-segmentation-3.0 + 3D-Speaker ERes2Net-base, fast clustering, scored with
`score_diarization.py` (each hypothesis label mapped to the reference speaker it covers most):

| Cluster threshold | Speakers found | Labels per true VI speaker | VI utterances attributed correctly | RTF (CPU) |
|---|---:|---|---:|---:|
| 0.5 (sherpa default) | 89 | 7–12 | 99.0 % (trivial: over-split) | 0.25 (4 thr) |
| 0.8 | 29 | 2–4 | 99.5 % | 0.16 (10 thr) |
| **1.0 (our default)** | 15 | **1–2** | **99.5 %** | 0.15 (10 thr) |

(The 60 English utterances come from many different FLEURS voices, so >5 speakers is expected.)
Channel prior: mic utterances labelled "me": **100 %**; mic echo of system audio (speakers instead
of headphones) is removed by time-overlap + text-similarity (`transcript::drop_echo`, unit-tested;
not exercised by this synthetic set, which has no echo).

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
5. **Real meetings / user recordings** — none available; `eval/stt/private/` (git-ignored) is
   where they go; `prepare_data.py`-style manifest + `stt_eval` works on them.
6. Phone (Samsung A26) speed — not measured (no Android toolchain yet); Zipformer-30M int8 is
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

# GPU reference (transformers)
python eval/stt/hf_eval.py --model vinai/PhoWhisper-medium --set vivos-test
python eval/stt/summarize.py      # markdown table of all runs
```
