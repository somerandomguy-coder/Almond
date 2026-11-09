# Summary eval — results (2026-11-10)

**Setup.** 10 synthetic meetings (`data/`: 5 VI with VI–EN code-switching, 5 EN; 13–19 segments
each, 7–9 reference key points, 3–5 reference action items). Windows Ollama 0.40.13, RTX 5060 Ti
16 GB. `think: false`, temperature 0.2, one run per meeting (n = 5 per row, so expect roughly ±0.1
noise on coverage). Judge = `gemma4:12b` (so its own judge scores may be slightly favourable).
Raw rows incl. every generated summary: `results/rows.json` (native templates, + cross-language)
and `results/rows-tplen.json` (English instruction templates).

Models: the models pulled on the PC at eval time were `qwen3.5:4b`, `gemma4:12b`, `llama3.1:8b`
(`qwen3.5:9b` / `gemma4:e4b` were not available on the working Ollama, and nothing new was pulled).

Columns: **coverage** = share of reference key points present (keyword match / LLM judge);
**cite validity** = cited indices that exist in the transcript (before our validator drops the rest);
**grounded** = bullets with ≥1 valid cite; **cite hit** = best-matching bullet cites a gold segment
(±1); **lang ok** = bullets detected in the requested output language; **AI recall** = reference
action items found by `action_items`; latency = summary call only (model already loaded).

## A. Final prompts, English instructions (`template_lang = en`, the new default)

| model | set | coverage (kw) | coverage (judge) | cite validity | grounded | cite hit | lang ok | AI recall | latency (s) | tok/s |
|---|---|---|---|---|---|---|---|---|---|---|
| qwen3.5:4b | vi→vi | 0.72 | 0.85 | 1.00 | 1.00 | 0.97 | 1.00 | 0.91 | 8.0 | 57 |
| qwen3.5:4b | en→en | 0.86 | 0.94 | 1.00 | 1.00 | 0.77 | 0.97 | 0.89 | 7.6 | 62 |
| **gemma4:12b** | vi→vi | **1.00** | **0.97** | 1.00 | 1.00 | 0.90 | 0.98 | 0.95 | 19.5 | 32 |
| **gemma4:12b** | en→en | **1.00** | **1.00** | 1.00 | 1.00 | 0.86 | 1.00 | 1.00 | 18.3 | 32 |
| llama3.1:8b | vi→vi | 0.97 | 0.97 | 1.00 | 1.00 | 0.82 | 0.99 | 0.86 | 15.6 | 44 |
| llama3.1:8b | en→en | 0.94 | 0.97 | 1.00 | 1.00 | 0.97 | 1.00 | 0.96 | 11.2 | 54 |

(One judge call for gemma4:12b / vi-02 returned unusable JSON; that row is excluded from this
table and the harness exited non-zero, as designed.)

## B. Final prompts, native-language instructions (vi template for vi output) + cross-language

| model | set | coverage (kw) | coverage (judge) | cite validity | grounded | cite hit | lang ok | AI recall | latency (s) | tok/s |
|---|---|---|---|---|---|---|---|---|---|---|
| qwen3.5:4b | vi→vi | 0.58 | 0.55 | 1.00 | 1.00 | 0.90 | 1.00 | 1.00 | 4.8 | 63 |
| qwen3.5:4b | en→en | 0.86 | 0.94 | 1.00 | 1.00 | 0.89 | 0.98 | 0.85 | 7.3 | 69 |
| qwen3.5:4b | cross (vi↔en) | – | – | 1.00 | 1.00 | – | 1.00 | – | 7.8 | 69 |
| gemma4:12b | vi→vi | 1.00 | 1.00 | 1.00 | 1.00 | 0.88 | 0.98 | 0.95 | 16.5 | 39 |
| gemma4:12b | en→en | 1.00 | 1.00 | 1.00 | 1.00 | 0.86 | 1.00 | 1.00 | 16.3 | 35 |
| gemma4:12b | cross (vi↔en) | – | – | 1.00 | 1.00 | – | 1.00 | – | 16.8 | 38 |
| llama3.1:8b | vi→vi | 0.39 | 0.47 | 1.00 | 1.00 | 1.00 | 1.00 | 0.91 | 2.6 | 52 |
| llama3.1:8b | en→en | 0.94 | 0.91 | 0.97 | 0.98 | 0.90 | 1.00 | 0.92 | 8.3 | 69 |
| llama3.1:8b | cross (vi↔en) | – | – | 0.99 | 0.99 | – | 0.99 | – | 7.8 | 65 |

## C. Before the "be complete" prompt line (first iteration, native templates)

| model | vi→vi cov (kw / judge) | en→en cov (kw / judge) | latency vi / en (s) |
|---|---|---|---|
| qwen3.5:4b | 0.70 / 0.68 | 0.71 / 0.71 | 8.8 / 7.1 |
| gemma4:12b | 1.00 / 0.97 | 0.91 / 0.89 | 13.6 / 12.1 |
| llama3.1:8b | 0.64 / 0.70 | 0.89 / 0.89 | 4.6 / 7.3 |

Adding "include every decision, number, date, owner and open issue (typically 6–15 bullets)" raised
EN coverage for every model and brought gemma4:12b to 1.00 everywhere.

## Findings

- **Citations are solid across the board.** Structured output (`format` = JSON Schema) + segment
  indices in the prompt gives ≥ 0.97 citation validity and ≥ 0.98 grounded bullets for every
  model; the validator drops the rare bad index. Cite *hit* (pointing at the right line) is
  0.77–0.97.
- **Output language is reliable** (≥ 0.97), including cross-language summaries (VI meeting → EN
  notes and vice versa) and code-switched transcripts.
- **Small models under-summarize Vietnamese with Vietnamese instructions** (they return 3–4
  bullets). English instructions + "write in Vietnamese" fixes most of it (llama3.1:8b 0.39 → 0.97,
  qwen3.5:4b 0.58 → 0.72 kw / 0.85 judge) and doesn't hurt gemma4:12b. → `TaskConfig` now defaults
  to `template_lang = en`; the vi templates remain (set `template_lang` to `vi`/None) for future
  tuning.
- **gemma4:12b is the clear quality winner** (1.00 coverage, best action-item recall) at ~16–20 s
  per summary — fine for post-meeting processing. llama3.1:8b is close on EN but less reliable on
  Vietnamese and is not multilingual-tuned; qwen3.5:4b is the fastest/lightest but misses ~15–30%
  of points.

## Recommended defaults

| device | model | prompts | notes |
|---|---|---|---|
| **PC (RTX 5060 Ti 16 GB)** | **`gemma4:12b`** (`DEFAULT_MODEL`) | `template_lang = en`, output = dominant transcript language | ~8.4 GB VRAM at 8k ctx; leaves room for STT. Re-evaluate `qwen3.5:9b` when it's pulled on the Windows Ollama (`--models qwen3.5:9b`). |
| **Fedora laptop (weak)** | **`qwen3.5:4b`** (`ALMOND_LLM_MODEL=qwen3.5:4b`) | same | Light summaries/titles locally; for full-quality notes keep requesting jobs with `requires: ["llm"]` and let the PC claim them on the next sync. |
| Phone | none | – | Requests jobs only (descriptor-only processors); PC runs them. |

The model is configurable per device (`ALMOND_LLM_MODEL`, `TaskConfig::model`) and per job
(`params.model`). Per-device capabilities should advertise `llm:<model>`.

## Caveats / next steps

- n = 5 per language, single run, synthetic meetings ~2–3 min long: good for regressions and
  relative ranking, not for absolute claims. Long meetings (map-reduce path) are only unit-tested
  with mocks so far — add a 30–60 min transcript once WS-B produces real ones.
- Keyword coverage is strict and the judge is one of the evaluated models; both agree within ~0.1.
- Enhanced notes are not scored here: user text is kept verbatim by construction (unit-tested);
  the quality of AI additions needs a rubric.
