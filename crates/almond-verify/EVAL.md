# almond-verify — hallucination eval

Reproduce:

```sh
cargo run -p almond-verify --example scorecard -- --rows                         # A: no LLM
cargo run -p almond-verify --example scorecard -- --ollama --model qwen3.5:4b --rows  # A–D
cargo test -p almond-verify --test hallucinations   # regression gate (A must keep R ≥ 0.8, P ≥ 0.9)
```

## Data

- **Dev set** `eval/fixtures.json`: 4 meetings (VI planning, EN launch sync, VI/EN code-switched
  standup, VI customer call), **38 summary bullets: 21 hallucinated, 17 correct**. Injected errors:
  wrong number (5), wrong owner (4), wrong date (4), invented decision (4), unsupported / no or
  irrelevant citation (3), wrong meeting time (1). Each meeting has a calendar event (attendees,
  RSVPs, start time) and named speakers.
- **Held-out set** `eval/heldout.json`: 2 meetings, **14 bullets (7/7)**, written after tuning on the
  dev set; includes **cross-lingual** bullets (English summary of a Vietnamese meeting: "2.3 billion
  VND" vs "hai tỷ ba", "300 million" vs "300tr") and a wrong-attendee claim.
- A bullet is **flagged** by a verifier if any claim extracted from it gets a non-`supported`
  verdict from that verifier. "aggregate ≠ supported" is the UI's "not green" signal;
  "contradicted / needs_review" is the stricter "red or amber" alarm. Positive = hallucinated.
- Synthetic, small, written by the same agent that wrote the verifiers → numbers are optimistic
  upper bounds for this kind of error, not a benchmark. See "Caveats".

## Headline

| set | configuration | P | R | F1 |
|---|---|---|---|---|
| dev | A. symbolic only, rule extraction (no LLM) | 1.00 | 0.86 | 0.92 |
| dev | B. symbolic + `verify.nli` (qwen3.5:4b) | **1.00** | **1.00** | **1.00** |
| dev | C. B + LLM claim extraction (qwen3.5:4b) | 1.00 | 1.00 | 1.00 |
| dev | D. `verify.nli` alone (qwen3.5:4b) | 1.00 | 0.95 | 0.98 |
| held-out (blind, first run) | A. symbolic only | 0.75 | 0.86 | 0.80 |
| held-out (after parser fixes) | A. symbolic only | 1.00 | 1.00 | 1.00 |
| held-out (after parser fixes) | B. symbolic + nli (qwen3.5:4b) | 0.88 | 1.00 | 0.93 |
| held-out | D. nli alone (qwen3.5:4b) | 0.86 | 0.86 | 0.86 |

(aggregate, flagged = ≠ supported). PLAN.md bar "≥ 80% recall at reasonable precision" is met by
the symbolic baseline alone on both sets and by every configuration with the LLM judge.

**Model note.** The brief named `qwen3.5:9b`, but it is not pulled on this PC (`ollama list`:
`qwen3.5:4b`, `gemma4:12b`, `llama3.1:8b`, ...). Real-LLM numbers use **qwen3.5:4b** (RTX 5060 Ti
16 GB, temperature 0, `think: false`, JSON-schema `format`); a gemma4:12b run is appended below if
it completed. Rerun with `--model qwen3.5:9b` once pulled; the harness refuses to run with a missing
model (the first attempt silently produced "judge failed" verdicts — now counted as errors).

**Held-out honesty.** The first held-out run (blind) exposed three generic bugs: VI elliptical numbers
("hai tỷ ba" parsed as 2 000 000 003 instead of 2.3 tỷ), "at 45k" treated as a time, and any past
date treated as a missed deadline ("October revenue" → `due_before_meeting`). Fixed in
`numbers.rs`, `verifiers/dates.rs`, `triples.rs` (+ multiset number matching), with unit tests.
After that the held-out set is no longer blind; the blind row is the honest generalization number.

## Per-verifier (dev set, configuration B, qwen3.5:4b)

| system | P | R | F1 | TP | FP | FN | coverage |
|---|---|---|---|---|---|---|---|
| verify.calendar | 1.00 | 0.05 | 0.09 | 1 | 0 | 20 | 2/38 |
| verify.citation_exists | 1.00 | 0.10 | 0.17 | 2 | 0 | 19 | 38/38 |
| verify.consistency (ascent) | 1.00 | 0.33 | 0.50 | 7 | 0 | 14 | 7/38 |
| verify.dates | 1.00 | 0.19 | 0.32 | 4 | 0 | 17 | 15/38 |
| verify.entities | 1.00 | 0.19 | 0.32 | 4 | 0 | 17 | 16/38 |
| verify.numbers | 1.00 | 0.33 | 0.50 | 7 | 0 | 14 | 15/38 |
| verify.nli (qwen3.5:4b) | 1.00 | 0.95 | 0.98 | 20 | 0 | 1 | 38/38 |
| **aggregate** ≠ supported | **1.00** | **1.00** | **1.00** | 21 | 0 | 0 | 38/38 |
| **aggregate** contradicted / needs_review | 1.00 | 0.95 | 0.98 | 20 | 0 | 1 | 38/38 |

Symbolic verifiers are narrow (low coverage/recall each) but precise; together (A) they reach
R 0.86 at P 1.00. Recall by error type, configuration A → B:

| type | A (symbolic) | B (+ nli) |
|---|---|---|
| wrong_number | 5/5 | 5/5 |
| wrong_date | 4/4 | 4/4 |
| wrong_owner | 4/4 | 4/4 |
| wrong_meeting_time | 1/1 | 1/1 |
| unsupported | 3/3 | 3/3 |
| invented_decision | **1/4** | **4/4** |

Invented decisions ("the team *decided* to move to Postgres" when it was only proposed) are the
class symbolic checks cannot see — there is no wrong atom, only wrong modality. The LLM judge
catches them; the ADR proposes modality-aware triples to make that symbolic too.

The NLI judge alone misses one wrong owner ("An phụ trách sửa bug iOS" vs "Chi lo luôn bản iOS"),
which `verify.entities` + the Datalog blame rule catch: the combination is strictly better than
either part.

## Neural vs symbolic disagreement (held-out, B)

The single false positive on held-out B: "Ad spend was 300 million VND." citing "Chi phí quảng cáo
hết 300tr". `verify.numbers` normalizes `300tr` = 300 000 000 and **matches**; the 4B judge does not
know the slang `tr` = triệu and says **contradicted** (0.9), which wins under precedence rule R1.
A cheap fix is an "atomic veto" (a precise symbolic match on every atom of a numeric claim
down-weights a neural contradiction to needs_review), left for the research track because it trades
recall on modality errors; see ADR roadmap items 1 and 3.

## Claim extraction (qwen3.5:4b)

Dev: 38 units → 38 claims in 8 calls, 0 fallbacks, 0 rejected by the "no new numbers" guard, 38/38
with `triples.v1`. Held-out: 14 → 14, 2 calls, 0 fallbacks. Fixture bullets are already atomic, so
the extractor (correctly) did not split them; extraction did not change any metric (B = C).
The guard and fallbacks are exercised in unit tests with `MockLlm`.

## Latency (qwen3.5:4b, RTX 5060 Ti)

NLI ≈ 1.5 s per claim (38 claims: ~59 s), extraction ≈ 3 s per 8-bullet batch. The symbolic stack
runs the whole dev set in < 0.1 s.

## Caveats

- Small synthetic fixtures; same author for data and verifiers (dev numbers are tuned).
- Every bullet cites exactly the segment the generator would cite; real summaries cite less precisely.
- `verify.entities` uses capitalization heuristics; lowercase or ASR-garbled names will slip.
- No evaluation yet of explanation/trace faithfulness or of confidence calibration.
