# verify-bench: catching factual errors in Almond meeting summaries (VI + EN)

Run date 2026-11-12, RTX 5060 Ti 16 GB, Windows 11. Everything ran locally: Ollama `gemma4:12b` plus
Hugging Face encoders. **This report has aggregate numbers only.** The few example texts come from
synthetic data: Almond's eval fixtures and SymAI's fictional council minutes.

> **Partial run.** The coordinator asked for a lighter GPU load midway through (the PC was running
> hot). Everything on the synthetic set ran in full. On the personal set, every encoder system and
> **Almond current** ran on all 322 claims. The plain judge and CoVe ran on a **stratified 20-claim
> sample** (10 clean and 10 errors, 5 of each per language). **QA-based, SelfCheck and MiniCheck+MT
> were not run on personal data.** Section 6 lists every cut.

## 1. Headline

| system | synthetic VI R / FA | synthetic EN R / FA | personal VI R / FA | personal EN R / FA | ms per claim |
|---|---|---|---|---|---|
| **Almond current** (rules + `verify.nli`, gemma4:12b) | 89% / 8% | 84% / 20% | 56% / 29% | 72% / 18% | 2 000–2 800 |
| Almond rules only, red/amber | 70% / 0% | 56% / 11% | 11% / 0% | 25% / 0% | 0.4–7 |
| SymAI as published (EN models and lexicon) | 44% / 0% | 95% / 3% | 7% / 7% | 64% / 31% | 20 |
| SymAI-VI (multilingual models + VI lexicon, built outside SymAI) | 63% / 8% | 87% / 4% | 31% / 9% | 46% / 10% | 33 |
| SummaC-ZS (mDeBERTa-v3 XNLI) | 68% / 8% | 83% / 9% | 28% / **0%** | 58% / 4% | **17** |
| MiniCheck-RoBERTa-L | 87% / 33% | 93% / 12% | 97% / **95%** | 76% / 20% | 11 |
| MiniCheck + gemma MT | 82% / 25% | 93% / 10% | not run | not run | 2 200 (VI) |
| QA-based (QAGS-style, 3 calls) | 93% / 25% | 96% / 24% | not run | not run | 3 300 |
| LLM judge, plain (1 call) | 83% / **0%** | 83% / **2%** | 60% / 0% (n=5+5) | 80% / 0% (n=5+5) | 1 300 |
| LLM judge, CoVe (3 calls) | 87% / 8% | 88% / 9% | 60% / 0% (n=5+5) | 80% / 0% (n=5+5) | 4 400 |
| SelfCheck-prompt (4 samples) | 91% / 8% | 85% / 10% | not run | not run | 7 500 |
| **Proposal**: judge + Almond atoms + distortion rules | 87% / **0%** | **93% / 7%** | 80% / 0% (n=5+5) | 80% / 0% (n=5+5) | 1 400 |

R = recall on injected errors. FA = false-alarm rate on clean bullets. "Flagged" means any label other
than *supported*. Synthetic: 170 errors and 115 clean bullets (VI 54/12, EN 116/103). Personal: 215
errors and 107 clean bullets (VI 90/58, EN 125/49). Bootstrap 95% CIs, AUROC and precision are in the
appendix. With n=12 VI clean synthetic bullets and n=5 per cell in the personal sample, those cells are
indicative only.

**Recommendation in one line:** keep Almond's architecture (cheap symbolic atoms plus one LLM judge
call), but (a) fix the `verify.nli` quote and neighbour rules that cause most of its false alarms,
(b) add a VI/EN lexical *distortion* verifier (hedge, quantifier, genericity, negation scope, time
frame) in the SymAI style, and (c) optionally add multilingual NLI (SummaC-style) as a 17 ms
pre-screen and confidence signal. Section 5 has the details.

## 2. Setup

**Transcription (task 1).** The 4 private recordings (about 1, 4, 22 and 64 min) were decoded with
PyAV to 16 kHz mono. They were then transcribed with Almond's own stack:
`crates/almond-stt/examples/transcribe.rs`, CUDA build, Zipformer-30M (VI), whisper large-v3-turbo
(EN and language ID), pyannote-3.0 and 3D-Speaker diarization. This is the routing `almond-core` uses
on a desktop GPU. Results:

- RTF 0.07–0.11, so the 64-min recording took 4.7 min.
- 782 segments in total.
- Language mix by characters: one recording is about 98% Vietnamese, three are mostly English with
  short VI-tagged fragments. Overall, about 75% of transcript characters are VI.
- Mean segment confidence 0.78–0.82.

Quality notes from a hand-check of about 20 segments:

- Zipformer VI output is all caps with no punctuation, and runs on across sentence boundaries. It is
  intelligible.
- Number words are spelled out ("BA MƯƠI PHÚT").
- Short noisy English fragments are sometimes routed to VI and come out as junk ("OHI"-like tokens).
- Diarization over-segments badly: 33 "speakers" in 22 min and 14 in 64 min, for conversations of
  2–4 people.
- There is no reference transcript, so WER was not measured.

**Clean bullets (task 2).** gemma4:12b wrote cited bullets over transcript windows: two passes, with
2 400-char and 1 100-char windows. The prompt asks for near-extractive bullets, speaker attribution,
and preserved hedges, numbers and names. A bullet was kept only if it passed **every** check below:

1. Cites are valid, 1–3 per bullet, inside the window.
2. Length is 5–45 words.
3. ≥ 60% of its content tokens (diacritic-folded) occur in the cited segments.
4. Every number occurs in the source. Spoken numbers are normalised, e.g. "ba mươi" = 30.
5. Every capitalised entity occurs in the source.
6. Every speaker label it names belongs to a cited segment.
7. Scope words (some, many, all, một số, nhiều, tất cả…) occur in the source.
8. A hedged source ("I think", "maybe", "chắc", "có lẽ") keeps a hedge or opinion attribution.
9. Two independent local judges, `qwen3.5:4b` and `llama3.1:8b`, both say "entailed". Neither is a
   benchmarked system, so the clean set is not biased toward gemma.
10. It is not a near-duplicate of an earlier bullet.

Yield was 107 of 308 generated bullets: 58 VI, 49 EN. In a **hand-check of 24 kept bullets**, about 20
were clearly faithful, 2 had subtle errors (a dropped "I think", "many" turned into "some"; checks 7
and 8 were added afterwards because of these) and 2 could not be judged because the ASR was too noisy.
**Clean-label noise of about 5–15% remains**, so personal false-alarm rates are upper bounds.

**Errors (task 2).** These are deterministic, single-edit mutations with exact labels, in the SymAI
style but bilingual (`vb/mutate.py`):

- number or unit
- entity swap
- negation flip (VI không/chưa/chẳng/không phải; EN)
- hedge drop and hedge add (VI có lẽ/chắc/khoảng/cho rằng→khẳng định; EN)
- quantifier (VI tất cả/một số/nhiều/hầu hết; EN)
- temporal (dd/mm, hours, thứ Hai…Chủ nhật, tuần sau↔tuần này, sẽ↔đã; EN equivalents)
- speaker misattribution
- overgeneralisation (episodic→habitual, "S3 said"→"Everyone said")
- fabrication (fixed VI/EN template pools, checked to be absent from the transcript)
- cluster insert (two fabrications together)

That is 11 types. Fabrications cite the neighbouring bullet's segments, as a generator would, so
"no citation" cannot give them away. A hand-check of about 60 mutations (synthetic and personal)
found and fixed several bugs: speaker labels "S3" being treated as numbers, "24 h|ours" parsed as a
time, "quyết định"→"quyết đã", sentence-initial "Do" treated as an entity. A few remaining edits are
ungrammatical but still change the meaning, so their labels hold.

**Subsets.**

- *almond*: `crates/almond-verify/eval` dev and held-out sets. There are 24 ok bullets and 28
  hand-written hallucinations, mapped onto the taxonomy (wrong_owner→speaker,
  invented_decision→hedge_drop, …), plus our mutations of the ok bullets.
- *symai*: SymAI's 8 English minutes notes, mutated by **SymAI's own `seed_cases`**. This subset
  compares directly with SymAI's report, but it is *in-sample* for SymAI's rules.
- *personal*: described above.

Each error is scored in its own *variant*: the clean summary with exactly that one bullet
substituted or inserted (SymAI's protocol). Clean bullets are scored once.

| subset | lang | clean | number | entity | negation | hedge_drop | hedge_add | quantifier | temporal | speaker | fabricate | overgen. | cluster |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| almond | vi | 12 | 9 | 2 | 4 | 4 | 6 | 0 | 9 | 8 | 5 | 1 | 6 |
| almond | en | 12 | 8 | 0 | 7 | 2 | 6 | 0 | 7 | 3 | 5 | 1 | 6 |
| symai | en | 91 | 8 | 5 | 8 | 6 | 0 | 6 | 8 | 5 | 8 | 1 | 16 |
| personal | vi | 58 | 10 | 3 | 10 | 10 | 10 | 10 | 10 | 10 | 5 | 10 | 2 |
| personal | en | 49 | 9 | 9 | 19 | 10 | 15 | 4 | 1 | 19 | 15 | 18 | 6 |

**Systems (task 3).** All systems get the same claim. All except SymAI get the same evidence: the
cited segments ±1 neighbour, rendered `[n] SPEAKER-NAME: text`, as `verify.nli` does. SymAI does its
own retrieval over the whole transcript, which is how it is designed.

- **a. Almond current**: new runner `crates/almond-verify/examples/bench` with the same set as the
  app's `verify.all` (rule extraction, `VerifierRegistry::with_llm`, aggregate `precedence.v1`) and
  judge gemma4:12b. It is reported in two views: "≠ supported" and "red/amber only"
  (contradicted / needs_review).
- **b. SymAI**: SymAI is imported read-only from its checkout. It uses the minutes profile, hybrid
  alignment, clingo rules and no Claude escalation. Models run on the GPU and predictions are
  memoised. A bullet counts as flagged if any factual rule (ACC-*, MIN-*, BIAS-OPF-01) fires on its
  sentence. **SymAI-VI** keeps the same rules and changes only things outside SymAI's repo:
  - retrieval with `qwen3-embedding:0.6b` (Ollama)
  - mDeBERTa XNLI for NLI
  - `vb/symai_vi_lexicon.yaml` merged into the lexicon
  - Unicode-aware negation-scope tokenisation, because SymAI's `[a-z]` regexes split Vietnamese words
- **c. SummaC-ZS**: max over source units (sentences, segments, window) of P(entail) − P(contradict),
  using `MoritzLaurer/mDeBERTa-v3-base-xnli-multilingual-nli-2mil7`.
- **d. MiniCheck-RoBERTa-L**: run as-is, and also with gemma VI→EN translation of claim and evidence.
- **e. QA-based**: gemma generates 3 QA pairs from the claim and answers them from the evidence. An
  LLM answer comparator replaces token F1, because F1 scored "200 triệu" against "hai trăm triệu" as
  a mismatch.
- **f. Judges**: plain (1 call), CoVe (plan → answer without seeing the claim → verdict, 3 calls), and
  SelfCheck-prompt (4 stochastic re-summaries of the evidence at T = 0.8, then a yes/no support check
  against each).
- **g. Hybrids**: post-hoc combinations of the per-claim outputs. Latency is the sum of the parts.

## 3. Results

### 3.1 Synthetic (fully run)

| system | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim | s/meeting |
|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 71% [64–78] | 76% | 33% [24–42] | 0.77 | 0.4 | 0.0 |
| Almond rules only (red/amber) | 61% [52–68] | 90% | 10% [4–16] | 0.77 | 0.4 | 0.0 |
| **Almond current** | 86% [81–91] | 87% | 19% [12–27] | 0.85 | 2 037 | 16.7 |
| SymAI | 79% [72–85] | 98% | 3% [0–6] | 0.97 | 21 | 0.9 |
| SymAI-VI | 79% [73–85] | 96% | 4% [1–9] | 0.95 | 33 | 2.4 |
| SummaC-ZS | 78% [72–84] | 93% | 9% [4–14] | 0.94 | 17 | 0.2 |
| MiniCheck | 91% [86–95] | 91% | 14% [8–20] | 0.94 | 11–50 | 0.4 |
| MiniCheck + MT | 89% [85–94] | 92% | 11% [6–17] | 0.96 | 11 (EN) / 2 200 (VI) | 2.4 |
| QA-based | 95% [91–98] | 85% | 24% [16–32] | 0.91 | 3 308 | 29.0 |
| Judge (plain) | 83% [77–89] | 99% | 2% [0–4] | 0.91 | 1 257 | 10.1 |
| Judge (CoVe) | 88% [82–92] | 94% | 9% [4–14] | 0.94 | 4 352 | 37.4 |
| SelfCheck-prompt | 87% [82–92] | 93% | 10% [4–15] | 0.91 | 7 514 | 62.4 |
| Hybrid: Almond rules + judge | 84% [79–89] | 97% | 4% [1–7] | 0.91 | 1 257 | 10.1 |
| Hybrid: Almond current OR SymAI-VI | 95% [91–98] | 86% | 23% [16–30] | 0.95 | 2 070 | 19.1 |
| **Proposal** | **91% [86–95]** | **96%** | **6% [2–10]** | 0.93 | 1 356 | 12.5 |

By subset:

- On *almond*, which is out-of-sample for SymAI, the proposal scores 87% R / 8% FA against Almond
  current's 88% / 4%. **No gain there**: the extra rules add 2 clean false alarms on 24 bullets.
- On *symai* the proposal scores 97% / 6% against Almond current's 83% / 23%. Part of this gain is
  in-sample for SymAI's rules.

### 3.2 Recall by error type, synthetic (all languages)

| system | number 25 | entity 7 | negation 19 | hedge_drop 12 | hedge_add 12 | quantifier 6 | temporal 24 | speaker 16 | fabricate 18 | overgen. 3 | cluster 28 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Almond rules only (≠ sup.) | 100% | 100% | 16% | 25% | 0% | 0% | 83% | 100% | 100% | 0% | 100% |
| Almond current | 100% | 100% | 100% | 42% | 8% | 50% | 92% | 100% | 100% | 67% | 100% |
| SymAI | 80% | 86% | 90% | 75% | 42% | 100% | 71% | 69% | 83% | 67% | 93% |
| SymAI-VI | 96% | 86% | 74% | 92% | 17% | 100% | 79% | 50% | 89% | 67% | 96% |
| SummaC-ZS | 92% | 71% | 74% | 58% | 33% | 50% | 83% | 50% | 100% | 100% | 100% |
| MiniCheck | 100% | 100% | 100% | 50% | 75% | 67% | 96% | 81% | 100% | 100% | 100% |
| QA-based | 100% | 100% | 95% | 83% | 83% | 67% | 100% | 94% | 100% | 67% | 100% |
| Judge (plain) | 100% | 100% | 84% | 42% | 0% | 67% | 92% | 88% | 100% | 67% | 100% |
| Judge (CoVe) | 100% | 86% | 95% | 67% | 8% | 67% | 96% | 94% | 100% | 100% | 100% |
| **Proposal** | 100% | 100% | 89% | **92%** | 17% | **100%** | 96% | 94% | 100% | 100% | 100% |

### 3.3 Personal (real ASR transcripts, gemma bullets; partial, see banner)

Full set, 215 errors and 107 clean bullets:

| system | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim | s/meeting |
|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 81% [75–86] | 68% | 77% [68–85] | 0.59 | 7.5 | 0.2 |
| Almond rules only (red/amber) | 19% [14–24] | 100% | 0% | 0.59 | 7.5 | 0.2 |
| **Almond current** | 65% [59–71] | 84% | 24% [17–33] | 0.71 | 2 787 | 74.5 |
| SymAI | 40% [34–46] | 82% | 18% [11–25] | 0.66 | 21 | 2.3 |
| SymAI-VI | 40% [34–47] | 90% | 9% [5–15] | 0.73 | 33 | 5.1 |
| SummaC-ZS | 45% [38–52] | 98% | 2% [0–5] | **0.82** | 17 | 0.6 |
| MiniCheck | 85% [80–89] | 74% | 61% [51–70] | 0.74 | 12 | 0.3 |
| Hybrid: Almond current OR SymAI-VI | 72% [66–78] | 83% | 30% [22–38] | 0.77 | 2 820 | 79.7 |

The same 20-claim stratified sample, with every system scored on it:

| system | recall | false alarms | AUROC |
|---|---|---|---|
| Almond current | 6/10 | 2/10 | 0.69 |
| SymAI-VI | 4/10 | 0/10 | 0.85 |
| SummaC-ZS | 4/10 | 0/10 | 0.76 |
| MiniCheck | 9/10 | 6/10 | 0.68 |
| Judge (plain) | 7/10 | 0/10 | 0.85 |
| Judge (CoVe) | 7/10 | 0/10 | 0.89 |
| **Proposal** | 8/10 | 0/10 | 0.90 |

Recall by type, personal full set:

| system | number 19 | entity 12 | negation 29 | hedge_drop 20 | hedge_add 25 | quantifier 14 | temporal 11 | speaker 29 | fabricate 20 | overgen. 28 | cluster 8 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Almond current | 84% | 67% | 76% | **20%** | 36% | **14%** | 46% | 79% | 100% | 82% | 100% |
| SymAI | 42% | 67% | 55% | 50% | 28% | 36% | 18% | 17% | 65% | 21% | 75% |
| SymAI-VI | 53% | 33% | 55% | 30% | 0% | 43% | 46% | 7% | 100% | 32% | 100% |
| SummaC-ZS | 47% | 50% | 72% | 5% | 4% | 14% | 0% | 41% | 100% | 61% | 100% |
| MiniCheck | 90% | 92% | 93% | 40% | 76% | 79% | 91% | 100% | 100% | 79% | 100% |

Synthetic and personal results differ a lot. Real ASR evidence (all-caps Vietnamese, run-on segments,
over-split speakers) cuts every system's recall by 20–50 points. Synthetic fixtures with clean
evidence overstate what Almond will see.

## 4. Failure analysis per type and system (examples are synthetic)

- **Almond rules only.** Precise on atoms: number, entity, date and speaker reach 100% recall on
  synthetic. They are blind to modality: negation 16%, hedges 0–25%, quantifier 0%, overgeneralisation
  0%. Example: *"Priya did not attend the meeting." → "Priya did attend the meeting."* passes, because
  no atom changed. On real ASR, VI number words in the transcript and unnamed speakers leave the
  number and entity verifiers almost nothing to check, so most clean bullets end up *unverifiable*
  (77% "≠ supported" on clean bullets).
- **Almond current (`verify.nli`).**
  - Recall is good on synthetic. It is weak on hedge_drop (42% synthetic, 20% personal), hedge_add
    (8% / 36%) and quantifier (50% / 14%).
  - The judge accepts *"Chi phí quảng cáo Facebook 50 triệu đồng mỗi tháng"* for a source that says
    "khoảng 50 triệu".
  - **Its false alarms are almost all `needs_review` from the judge wrapper, not from wrong
    judgements.** On personal, 26 of 26 clean false alarms come from `verify.nli`: 25 are
    `needs_review` (a judge quote not found in the evidence, or support only in an uncited
    neighbour), 1 is contradicted. On ALL-CAPS ASR text the judge paraphrases or normalises its
    quote, so the verbatim check fails.
  - On synthetic, the remaining false alarms come from `verify.entities` and `verify.consistency`:
    - *"Cr Okafor declared a non-pecuniary interest…"*: the cited turn says "I declare…" and the name
      is only the speaker label.
    - *"Staff will bring a traffic management plan to the December meeting."*: `owner_not_attendee`
      fires on "Staff".
  - The same model as a plain judge without these wrapper rules has 2% FA (synthetic) and 0/10
    (personal sample).
- **SymAI.**
  - On its own notes it is excellent: 100% / 0% FA, matching its README.
  - With English lexicons and English-only MiniLM/DeBERTa it collapses on Vietnamese: 7% recall on
    personal VI, 44% on synthetic VI. Its `[a-z]` tokenisation splits Vietnamese words, and its
    hedge, quantifier, negation and number lexicons, reporter regexes ("Cr X said…") and
    title-based person detection are all English.
  - The VI adaptation lifts VI recall to 31% (personal) and 63% (synthetic) at ≤ 9% FA.
  - Its distortion rules are what add value: hedge_drop 92%, quantifier 100% on synthetic.
  - Its support and fabrication path depends on retrieval over noisy turns, so on ASR transcripts
    fabricated-but-plausible text is aligned to something.
  - Speaker misattribution with diarization labels (S1/S2) is invisible to it, because it expects
    "Cr Name" titles.
- **SummaC-ZS (mDeBERTa).** The most conservative system: 0–4% FA on personal and the best AUROC on
  personal (0.82). Recall is low on hedges (5%), time (0%) and quantifiers (14%), because XNLI models
  treat these as compatible. Speaker swaps were caught 50% (synthetic) and 41% (personal).
- **MiniCheck (English-only).** On Vietnamese evidence it flags almost everything: 95% FA on personal
  VI. Translating with gemma fixes much of that on synthetic (VI FA 33% → 25%, n=12) at ~2 s per
  claim. On English it is strong (personal EN 76% / 20%, AUROC 0.87).
- **QA-based.** Highest synthetic recall (95%), but 24% FA. Question generation also asks about
  details the evidence never addresses, which reads as "unanswerable". It costs 3 calls per claim.
- **CoVe.** +5 pp recall over the plain judge on synthetic (hedge_drop 42 → 67%), at +7 pp FA and
  3.5× the latency.
- **SelfCheck-prompt.** No gain over the plain judge (87% / 10% vs 83% / 2%) at 6× the cost.
  Re-summaries drop details, so the yes/no checks become noisy.
- **Nobody catches hedge_add well** (0–17% for LLM judges). Judges treat "possibly X" as weaker than,
  and therefore implied by, "X". It is arguably a lower-severity error class.

## 5. Recommendation for `crates/almond-verify`

Three changes are worth making. Each targets a measured failure; expected gains are from this
benchmark.

1. **Fix `verify.nli`'s wrapper rules, not the judge.** Most of Almond current's false alarms
   (25 of 26 on personal) are `needs_review` produced after the judge answered.
   - Match quotes fuzzily: diacritic-folded token overlap ≥ 0.8 and case-insensitive, which matters
     for ALL-CAPS Zipformer output.
   - Accept support from an uncited ±1 neighbour as `supported` with confidence capped at about 0.6,
     instead of `needs_review`.
   - **Expected:** personal FA 24% → about 5–10%, the level of the same model run as a plain judge
     (2% synthetic, 0/10 personal sample), with little recall loss.
2. **Add `verify.distortion`, a deterministic VI+EN modality verifier.** Port SymAI's idea, not its
   code:
   - Compare the claim's lexical features with its cited segments: hedges (có lẽ, chắc, khoảng, hình
     như, maybe, I think), quantifier class (tất cả/một số/nhiều/hầu hết, all/some/most), genericity
     (hằng ngày, lần nào cũng, always) and time frame (sẽ/đã/đang, tuần sau/tuần này).
   - Use clause-scoped negation (không/chưa/chẳng/không phải).
   - Emit `contradicted` when the class differs, and `needs_review` when the claim has a marker the
     source lacks.
   - Put the lexicons in a data file, as `vb/symai_vi_lexicon.yaml` does, and keep the rules in
     `ascent`.
   - **Expected** (proposal hybrid vs Almond current, synthetic): hedge_drop 42% → 92%, quantifier
     50% → 100%, overall recall 86% → 91% with FA 19% → 6% (combined with change 1). The almond
     subset alone shows no gain (87/8 vs 88/4), so treat the size of the gain as uncertain until
     re-measured on more real data.
3. **Smaller fixes.**
   - `verify.entities`: treat speaker names of cited segments as present, so "Cr Okafor declared…"
     no longer needs the name in the text.
   - `verify.consistency`: skip `owner_not_attendee` for group owners (staff, team, nhóm, mọi người).
   - Both were synthetic false-alarm sources.
4. **Optional: SummaC-mDeBERTa as a pre-screen.** It needs 17 ms per claim, a 560 MB model, ≤ 4% FA
   and AUROC 0.82 on real data. Use it to show a provisional colour before the judge returns, to skip
   the judge on high-entailment claims (live mode), or as a confidence feature. It is not a sole
   detector: recall is 45% on personal. It would run as an external processor (Python, JSON-RPC),
   per the ADR.

Not recommended:

- MiniCheck: English-only; 95% FA on VI without MT, and MT costs as much as a judge call.
- QA-based: 24% FA at 3 calls.
- SelfCheck: 6× cost, no gain.
- CoVe as default: 3.5× cost for +5 pp recall. It could serve as an on-demand "re-check" button.

Cost of the proposal: one judge call per claim, about 1.3 s on gemma4:12b with this GPU (about
10–13 s for a 10-bullet summary), plus under 50 ms of symbolic and NLI work.

## 6. What was cut, hard or unreliable

- **Cut (GPU load):**
  - Personal-set QA-based, SelfCheck and MiniCheck+MT were not run.
  - Personal judge and CoVe ran on 20 claims only.
  - The proposal and judge hybrids on personal data are therefore n=20 and only indicative. A
    targeted re-run is `.\run_all.ps1 -Stage llm -Datasets personal`, with `VB_SLEEP=2`.
- **Label quality.**
  - Synthetic labels are exact by construction.
  - Personal clean bullets carry an estimated 5–15% residual noise from the hand-check, so personal
    FA is an upper bound.
  - Personal mutations were hand-checked; a few are ungrammatical.
  - Two mutation types are thin in places: entity VI n=3+2, temporal EN personal n=1, overgen.
    synthetic n=3.
- **Vietnamese coverage.**
  - There is only one predominantly Vietnamese recording, and it is a conversation rather than a
    business meeting.
  - Fabrication templates are generic business claims, so they are off-topic for personal
    conversations and easier to catch than realistic on-topic fabrications. Every system reaches
    100% on cluster inserts.
  - Diarization over-segmentation makes speaker misattribution (S3 → S11) a "different label of
    possibly the same person". Some of those positives may be unfair.
- **In-sample effects.** SymAI's rules were tuned on the *symai* subset. The Almond verifiers were
  tuned on the *almond* dev set; the held-out set was blind only on its first run.
- **One bug found and fixed mid-run.** At first, evidence for the Python systems rendered speaker
  ids (P3, S1) instead of names. This inflated their synthetic false alarms (judge FA 37% → 2% after
  the fix). Every affected system was re-run.
- **Latency** is the time of each request when first made (responses are cached). Almond current's
  per-claim time is the run total divided by judge calls. SymAI and encoder timings exclude model
  loading.

## 7. Downloads, disk, reproduction

- **Hugging Face models** (in `~/.cache/huggingface`):
  - `sentence-transformers/all-MiniLM-L6-v2`: 87 MB
  - `cross-encoder/nli-deberta-v3-base`: 714 MB
  - `MoritzLaurer/mDeBERTa-v3-base-xnli-multilingual-nli-2mil7`: 551 MB
  - `lytang/MiniCheck-RoBERTa-L`: 1.36 GB
  - Kept total: about 2.7 GB. transformers also auto-fetched a 1.36 GB safetensors conversion of
    MiniCheck (`refs/pr/1`). That temporary copy was deleted, but it means **about 4.1 GB was
    transferred, over the 3 GB budget**.
- **Ollama**: no pulls. Used gemma4:12b, qwen3.5:4b, llama3.1:8b and qwen3-embedding:0.6b.
- **Disk**: ≥ 300 GB free at the end.
- **Private data**: everything derived from the recordings (wav, transcripts, summaries, mutated
  sets, per-claim outputs, LLM caches) lives only under `personal_record/bench/`, which is gitignored.

```powershell
cd eval\verify-bench
uv sync                                   # Python 3.12, torch cu128, SymAI deps (SymAI itself via SYMAI_SRC)
.\run_all.ps1 -Stage data                 # models, transcribe (almond-stt CUDA), summaries, filter, assemble
.\run_all.ps1 -Stage encoders             # Almond rules (Rust runner), SummaC, MiniCheck, SymAI, SymAI-VI
.\run_all.ps1 -Stage llm                  # Almond current (verify.nli), judge, CoVe, QA, SelfCheck, MiniCheck+MT
.\run_all.ps1 -Stage metrics              # -> out/metrics.json, out/tables.md (aggregates only)
# Almond runner alone:
cargo run --release -p almond-verify --example bench -- --in out\synthetic.json --out out\results\almond_full.jsonl --config full --model gemma4:12b --cache out\cache\almond-judge
```

Without the private recordings, the synthetic half still runs: `assemble` skips the personal subset.

## Appendix: full aggregate tables (`vb/metrics.py` output)

## synthetic


### synthetic · all

| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |
|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 170 / 115 | 71% [64–78] | 76% | 33% [24–42] | 0.77 | 0.4 | 0.0 |
| Almond rules only (red/amber only) | 170 / 115 | 61% [52–68] | 90% | 10% [4–16] | 0.77 | 0.4 | 0.0 |
| Almond current (rules + verify.nli, ≠ supported) | 170 / 115 | 86% [81–91] | 87% | 19% [12–27] | 0.85 | 2036.8 | 16.7 |
| Almond current (red/amber only) | 170 / 115 | 79% [74–85] | 86% | 19% [12–27] | 0.85 | 2036.8 | 16.7 |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 170 / 115 | 79% [72–85] | 98% | 3% [0–6] | 0.97 | 21.2 | 0.9 |
| SymAI-VI (multilingual + VI lexicon) | 170 / 115 | 79% [73–85] | 96% | 4% [1–9] | 0.95 | 32.8 | 2.4 |
| SummaC-ZS (mDeBERTa XNLI) | 170 / 115 | 78% [72–84] | 93% | 9% [4–14] | 0.94 | 17.0 | 0.2 |
| MiniCheck-RoBERTa-L | 170 / 115 | 91% [86–95] | 91% | 14% [8–20] | 0.94 | 50.2 | 0.4 |
| MiniCheck + gemma MT | 170 / 115 | 89% [85–94] | 92% | 11% [6–17] | 0.96 | 11.1 | 2.4 |
| QA-based (QAGS-style) | 170 / 115 | 95% [91–98] | 85% | 24% [16–32] | 0.91 | 3308.2 | 29.0 |
| LLM judge (plain) | 170 / 115 | 83% [77–89] | 99% | 2% [0–4] | 0.91 | 1257.0 | 10.1 |
| LLM judge (CoVe) | 170 / 115 | 88% [82–92] | 94% | 9% [4–14] | 0.94 | 4352.4 | 37.4 |
| SelfCheck-prompt (4 samples) | 170 / 115 | 87% [82–92] | 93% | 10% [4–15] | 0.91 | 7513.5 | 62.4 |
| Hybrid: Almond rules + judge | 170 / 115 | 84% [79–89] | 97% | 4% [1–7] | 0.91 | 1257.4 | 10.1 |
| Hybrid: rules + judge + NLI veto | 170 / 115 | 84% [79–89] | 97% | 4% [1–7] | 0.97 | 1287.0 | 10.3 |
| Hybrid: Almond current OR SymAI-VI | 170 / 115 | 95% [91–98] | 86% | 23% [16–30] | 0.95 | 2069.7 | 19.1 |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 170 / 115 | 91% [86–95] | 96% | 6% [2–10] | 0.93 | 1355.7 | 12.5 |

### synthetic · lang=vi

| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |
|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 54 / 12 | 72% [59–83] | 100% | 0% [0–0] | 0.86 | 0.4 | 0.0 |
| Almond rules only (red/amber only) | 54 / 12 | 70% [57–82] | 100% | 0% [0–0] | 0.86 | 0.4 | 0.0 |
| Almond current (rules + verify.nli, ≠ supported) | 54 / 12 | 89% [80–96] | 98% | 8% [0–25] | 0.92 | 2036.8 | 16.7 |
| Almond current (red/amber only) | 54 / 12 | 89% [80–96] | 98% | 8% [0–25] | 0.92 | 2036.8 | 16.7 |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 54 / 12 | 44% [32–59] | 100% | 0% [0–0] | 0.85 | 18.8 | 0.9 |
| SymAI-VI (multilingual + VI lexicon) | 54 / 12 | 63% [50–76] | 97% | 8% [0–25] | 0.83 | 31.0 | 2.4 |
| SummaC-ZS (mDeBERTa XNLI) | 54 / 12 | 68% [56–82] | 97% | 8% [0–25] | 0.86 | 17.8 | 0.2 |
| MiniCheck-RoBERTa-L | 54 / 12 | 87% [78–96] | 92% | 33% [8–58] | 0.83 | 48.0 | 0.4 |
| MiniCheck + gemma MT | 54 / 12 | 82% [72–91] | 94% | 25% [0–50] | 0.90 | 2233.4 | 2.4 |
| QA-based (QAGS-style) | 54 / 12 | 93% [83–98] | 94% | 25% [0–50] | 0.89 | 3252.0 | 29.0 |
| LLM judge (plain) | 54 / 12 | 83% [72–93] | 100% | 0% [0–0] | 0.92 | 1338.1 | 10.1 |
| LLM judge (CoVe) | 54 / 12 | 87% [76–96] | 98% | 8% [0–25] | 0.94 | 4431.8 | 37.4 |
| SelfCheck-prompt (4 samples) | 54 / 12 | 91% [83–98] | 98% | 8% [0–25] | 0.91 | 8409.9 | 62.4 |
| Hybrid: Almond rules + judge | 54 / 12 | 85% [74–94] | 100% | 0% [0–0] | 0.93 | 1338.5 | 10.1 |
| Hybrid: rules + judge + NLI veto | 54 / 12 | 85% [74–94] | 100% | 0% [0–0] | 0.94 | 1374.7 | 10.3 |
| Hybrid: Almond current OR SymAI-VI | 54 / 12 | 91% [82–98] | 96% | 17% [0–42] | 0.90 | 2067.8 | 19.1 |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 54 / 12 | 87% [76–96] | 100% | 0% [0–0] | 0.93 | 1413.9 | 12.5 |

### synthetic · lang=en

| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |
|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 116 / 103 | 70% [61–78] | 68% | 37% [27–47] | 0.75 | 0.4 | 0.0 |
| Almond rules only (red/amber only) | 116 / 103 | 56% [47–65] | 86% | 11% [5–16] | 0.75 | 0.4 | 0.0 |
| Almond current (rules + verify.nli, ≠ supported) | 116 / 103 | 84% [77–90] | 82% | 20% [13–29] | 0.83 | 2036.8 | 16.7 |
| Almond current (red/amber only) | 116 / 103 | 75% [66–83] | 81% | 20% [13–29] | 0.83 | 2036.8 | 16.7 |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 116 / 103 | 95% [90–98] | 97% | 3% [0–7] | 0.98 | 26.3 | 0.9 |
| SymAI-VI (multilingual + VI lexicon) | 116 / 103 | 87% [81–93] | 96% | 4% [1–8] | 0.97 | 39.2 | 2.4 |
| SummaC-ZS (mDeBERTa XNLI) | 116 / 103 | 83% [75–90] | 91% | 9% [3–15] | 0.95 | 16.8 | 0.2 |
| MiniCheck-RoBERTa-L | 116 / 103 | 93% [88–97] | 90% | 12% [6–18] | 0.96 | 50.5 | 0.4 |
| MiniCheck + gemma MT | 116 / 103 | 93% [88–97] | 92% | 10% [4–16] | 0.98 | 11.0 | 2.4 |
| QA-based (QAGS-style) | 116 / 103 | 96% [91–99] | 82% | 24% [16–32] | 0.92 | 3334.9 | 29.0 |
| LLM judge (plain) | 116 / 103 | 83% [76–90] | 98% | 2% [0–5] | 0.90 | 1249.6 | 10.1 |
| LLM judge (CoVe) | 116 / 103 | 88% [81–93] | 92% | 9% [4–15] | 0.94 | 4309.1 | 37.4 |
| SelfCheck-prompt (4 samples) | 116 / 103 | 85% [78–91] | 91% | 10% [5–16] | 0.90 | 7402.7 | 62.4 |
| Hybrid: Almond rules + judge | 116 / 103 | 84% [77–90] | 96% | 4% [1–8] | 0.90 | 1250.0 | 10.1 |
| Hybrid: rules + judge + NLI veto | 116 / 103 | 84% [77–90] | 96% | 4% [1–8] | 0.98 | 1266.6 | 10.3 |
| Hybrid: Almond current OR SymAI-VI | 116 / 103 | 97% [93–99] | 82% | 23% [16–32] | 0.97 | 2076.0 | 19.1 |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 116 / 103 | 93% [88–97] | 94% | 7% [3–12] | 0.94 | 1321.3 | 12.5 |

### synthetic · sample20

| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |
|---|---|---|---|---|---|---|---|

### synthetic · subset=almond

| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |
|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 99 / 24 | 71% [62–80] | 100% | 0% [0–0] | 0.85 | 0.4 | 0.0 |
| Almond rules only (red/amber only) | 99 / 24 | 68% [59–77] | 100% | 0% [0–0] | 0.85 | 0.4 | 0.0 |
| Almond current (rules + verify.nli, ≠ supported) | 99 / 24 | 88% [81–94] | 99% | 4% [0–12] | 0.93 | 2036.8 | 16.7 |
| Almond current (red/amber only) | 99 / 24 | 86% [79–93] | 99% | 4% [0–12] | 0.93 | 2036.8 | 16.7 |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 99 / 24 | 64% [55–73] | 96% | 12% [0–25] | 0.84 | 19.3 | 0.9 |
| SymAI-VI (multilingual + VI lexicon) | 99 / 24 | 67% [57–75] | 94% | 17% [4–33] | 0.82 | 31.3 | 2.4 |
| SummaC-ZS (mDeBERTa XNLI) | 99 / 24 | 77% [68–85] | 97% | 8% [0–21] | 0.90 | 17.8 | 0.2 |
| MiniCheck-RoBERTa-L | 99 / 24 | 92% [86–97] | 92% | 33% [17–54] | 0.82 | 48.2 | 0.4 |
| MiniCheck + gemma MT | 99 / 24 | 89% [82–95] | 95% | 21% [4–38] | 0.94 | 1778.0 | 2.4 |
| QA-based (QAGS-style) | 99 / 24 | 96% [92–99] | 94% | 25% [8–42] | 0.92 | 3099.4 | 29.0 |
| LLM judge (plain) | 99 / 24 | 85% [77–92] | 100% | 0% [0–0] | 0.93 | 1238.5 | 10.1 |
| LLM judge (CoVe) | 99 / 24 | 87% [80–93] | 98% | 8% [0–21] | 0.94 | 4144.8 | 37.4 |
| SelfCheck-prompt (4 samples) | 99 / 24 | 88% [81–94] | 99% | 4% [0–12] | 0.91 | 7988.4 | 62.4 |
| Hybrid: Almond rules + judge | 99 / 24 | 86% [78–93] | 100% | 0% [0–0] | 0.93 | 1238.8 | 10.1 |
| Hybrid: rules + judge + NLI veto | 99 / 24 | 86% [78–93] | 100% | 0% [0–0] | 0.96 | 1257.3 | 10.3 |
| Hybrid: Almond current OR SymAI-VI | 99 / 24 | 91% [85–96] | 95% | 21% [4–38] | 0.86 | 2068.1 | 19.1 |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 99 / 24 | 87% [80–93] | 98% | 8% [0–21] | 0.92 | 1321.3 | 12.5 |

### synthetic · subset=symai

| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |
|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 71 / 91 | 70% [58–80] | 57% | 42% [32–52] | 0.71 | 0.4 | 0.0 |
| Almond rules only (red/amber only) | 71 / 91 | 51% [39–62] | 77% | 12% [6–20] | 0.71 | 0.4 | 0.0 |
| Almond current (rules + verify.nli, ≠ supported) | 71 / 91 | 83% [75–92] | 74% | 23% [14–32] | 0.79 | 2036.8 | 16.7 |
| Almond current (red/amber only) | 71 / 91 | 70% [59–82] | 70% | 23% [14–32] | 0.79 | 2036.8 | 16.7 |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 71 / 91 | 100% [100–100] | 100% | 0% [0–0] | 1.00 | 53.0 | 0.9 |
| SymAI-VI (multilingual + VI lexicon) | 71 / 91 | 97% [93–100] | 99% | 1% [0–3] | 0.99 | 82.1 | 2.4 |
| SummaC-ZS (mDeBERTa XNLI) | 71 / 91 | 80% [70–89] | 88% | 9% [3–14] | 0.95 | 16.7 | 0.2 |
| MiniCheck-RoBERTa-L | 71 / 91 | 90% [83–97] | 89% | 9% [3–15] | 0.97 | 51.7 | 0.4 |
| MiniCheck + gemma MT | 71 / 91 | 90% [83–97] | 89% | 9% [3–15] | 0.97 | 11.0 | 2.4 |
| QA-based (QAGS-style) | 71 / 91 | 93% [86–99] | 75% | 24% [15–33] | 0.89 | 3427.4 | 29.0 |
| LLM judge (plain) | 71 / 91 | 80% [70–89] | 97% | 2% [0–6] | 0.89 | 1294.7 | 10.1 |
| LLM judge (CoVe) | 71 / 91 | 89% [82–96] | 89% | 9% [3–15] | 0.93 | 4549.4 | 37.4 |
| SelfCheck-prompt (4 samples) | 71 / 91 | 86% [78–94] | 86% | 11% [4–18] | 0.91 | 7440.3 | 62.4 |
| Hybrid: Almond rules + judge | 71 / 91 | 82% [73–90] | 94% | 4% [1–10] | 0.89 | 1295.1 | 10.1 |
| Hybrid: rules + judge + NLI veto | 71 / 91 | 82% [73–90] | 94% | 4% [1–10] | 0.97 | 1311.5 | 10.3 |
| Hybrid: Almond current OR SymAI-VI | 71 / 91 | 100% [100–100] | 77% | 23% [14–32] | 0.99 | 2118.9 | 19.1 |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 71 / 91 | 97% [93–100] | 93% | 6% [1–11] | 0.96 | 1362.0 | 12.5 |

### synthetic · recall by error type (all languages; n in header)

| system | number (25) | entity (7) | negation (19) | hedge_drop (12) | hedge_add (12) | quantifier (6) | temporal (24) | speaker (16) | fabricate (18) | overgeneralise (3) | cluster_insert (28) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 100% | 100% | 16% | 25% | 0% | 0% | 83% | 100% | 100% | 0% | 100% |
| Almond rules only (red/amber only) | 100% | 86% | 10% | 17% | 0% | 0% | 75% | 100% | 67% | 0% | 79% |
| Almond current (rules + verify.nli, ≠ supported) | 100% | 100% | 100% | 42% | 8% | 50% | 92% | 100% | 100% | 67% | 100% |
| Almond current (red/amber only) | 100% | 100% | 100% | 42% | 8% | 50% | 92% | 100% | 72% | 67% | 79% |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 80% | 86% | 90% | 75% | 42% | 100% | 71% | 69% | 83% | 67% | 93% |
| SymAI-VI (multilingual + VI lexicon) | 96% | 86% | 74% | 92% | 17% | 100% | 79% | 50% | 89% | 67% | 96% |
| SummaC-ZS (mDeBERTa XNLI) | 92% | 71% | 74% | 58% | 33% | 50% | 83% | 50% | 100% | 100% | 100% |
| MiniCheck-RoBERTa-L | 100% | 100% | 100% | 50% | 75% | 67% | 96% | 81% | 100% | 100% | 100% |
| MiniCheck + gemma MT | 100% | 100% | 100% | 58% | 50% | 67% | 96% | 75% | 100% | 100% | 100% |
| QA-based (QAGS-style) | 100% | 100% | 95% | 83% | 83% | 67% | 100% | 94% | 100% | 67% | 100% |
| LLM judge (plain) | 100% | 100% | 84% | 42% | 0% | 67% | 92% | 88% | 100% | 67% | 100% |
| LLM judge (CoVe) | 100% | 86% | 95% | 67% | 8% | 67% | 96% | 94% | 100% | 100% | 100% |
| SelfCheck-prompt (4 samples) | 100% | 100% | 95% | 50% | 17% | 33% | 100% | 100% | 100% | 67% | 100% |
| Hybrid: Almond rules + judge | 100% | 100% | 90% | 42% | 0% | 67% | 92% | 94% | 100% | 67% | 100% |
| Hybrid: rules + judge + NLI veto | 100% | 100% | 90% | 42% | 0% | 67% | 92% | 94% | 100% | 67% | 100% |
| Hybrid: Almond current OR SymAI-VI | 100% | 100% | 100% | 100% | 25% | 100% | 100% | 100% | 100% | 100% | 100% |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 100% | 100% | 90% | 100% | 0% | 100% | 100% | 94% | 100% | 100% | 100% |

### synthetic · recall by error type, lang=vi

| system | number (9) | entity (2) | negation (4) | hedge_drop (4) | hedge_add (6) | temporal (9) | speaker (8) | fabricate (5) | overgeneralise (1) | cluster_insert (6) |
|---|---|---|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 100% | 100% | 0% | 25% | 0% | 89% | 100% | 100% | 0% | 100% |
| Almond rules only (red/amber only) | 100% | 100% | 0% | 25% | 0% | 78% | 100% | 100% | 0% | 100% |
| Almond current (rules + verify.nli, ≠ supported) | 100% | 100% | 100% | 75% | 17% | 100% | 100% | 100% | 100% | 100% |
| Almond current (red/amber only) | 100% | 100% | 100% | 75% | 17% | 100% | 100% | 100% | 100% | 100% |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 56% | 50% | 100% | 25% | 0% | 33% | 50% | 40% | 0% | 67% |
| SymAI-VI (multilingual + VI lexicon) | 100% | 50% | 75% | 75% | 0% | 67% | 25% | 80% | 0% | 100% |
| SummaC-ZS (mDeBERTa XNLI) | 100% | 50% | 75% | 75% | 0% | 78% | 25% | 100% | 100% | 100% |
| MiniCheck-RoBERTa-L | 100% | 100% | 100% | 75% | 67% | 89% | 62% | 100% | 100% | 100% |
| MiniCheck + gemma MT | 100% | 100% | 100% | 100% | 17% | 89% | 50% | 100% | 100% | 100% |
| QA-based (QAGS-style) | 100% | 100% | 100% | 75% | 67% | 100% | 88% | 100% | 100% | 100% |
| LLM judge (plain) | 100% | 100% | 100% | 75% | 0% | 100% | 75% | 100% | 100% | 100% |
| LLM judge (CoVe) | 100% | 50% | 100% | 100% | 17% | 100% | 88% | 100% | 100% | 100% |
| SelfCheck-prompt (4 samples) | 100% | 100% | 100% | 75% | 33% | 100% | 100% | 100% | 100% | 100% |
| Hybrid: Almond rules + judge | 100% | 100% | 100% | 75% | 0% | 100% | 88% | 100% | 100% | 100% |
| Hybrid: rules + judge + NLI veto | 100% | 100% | 100% | 75% | 0% | 100% | 88% | 100% | 100% | 100% |
| Hybrid: Almond current OR SymAI-VI | 100% | 100% | 100% | 100% | 17% | 100% | 100% | 100% | 100% | 100% |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 100% | 100% | 100% | 100% | 0% | 100% | 88% | 100% | 100% | 100% |

### synthetic · recall by error type, lang=en

| system | number (16) | entity (5) | negation (15) | hedge_drop (8) | hedge_add (6) | quantifier (6) | temporal (15) | speaker (8) | fabricate (13) | overgeneralise (2) | cluster_insert (22) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 100% | 100% | 20% | 25% | 0% | 0% | 80% | 100% | 100% | 0% | 100% |
| Almond rules only (red/amber only) | 100% | 80% | 13% | 12% | 0% | 0% | 73% | 100% | 54% | 0% | 73% |
| Almond current (rules + verify.nli, ≠ supported) | 100% | 100% | 100% | 25% | 0% | 50% | 87% | 100% | 100% | 50% | 100% |
| Almond current (red/amber only) | 100% | 100% | 100% | 25% | 0% | 50% | 87% | 100% | 62% | 50% | 73% |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 94% | 100% | 87% | 100% | 83% | 100% | 93% | 88% | 100% | 100% | 100% |
| SymAI-VI (multilingual + VI lexicon) | 94% | 100% | 73% | 100% | 33% | 100% | 87% | 75% | 92% | 100% | 96% |
| SummaC-ZS (mDeBERTa XNLI) | 88% | 80% | 73% | 50% | 67% | 50% | 87% | 75% | 100% | 100% | 100% |
| MiniCheck-RoBERTa-L | 100% | 100% | 100% | 38% | 83% | 67% | 100% | 100% | 100% | 100% | 100% |
| MiniCheck + gemma MT | 100% | 100% | 100% | 38% | 83% | 67% | 100% | 100% | 100% | 100% | 100% |
| QA-based (QAGS-style) | 100% | 100% | 93% | 88% | 100% | 67% | 100% | 100% | 100% | 50% | 100% |
| LLM judge (plain) | 100% | 100% | 80% | 25% | 0% | 67% | 87% | 100% | 100% | 50% | 100% |
| LLM judge (CoVe) | 100% | 100% | 93% | 50% | 0% | 67% | 93% | 100% | 100% | 100% | 100% |
| SelfCheck-prompt (4 samples) | 100% | 100% | 93% | 38% | 0% | 33% | 100% | 100% | 100% | 50% | 100% |
| Hybrid: Almond rules + judge | 100% | 100% | 87% | 25% | 0% | 67% | 87% | 100% | 100% | 50% | 100% |
| Hybrid: rules + judge + NLI veto | 100% | 100% | 87% | 25% | 0% | 67% | 87% | 100% | 100% | 50% | 100% |
| Hybrid: Almond current OR SymAI-VI | 100% | 100% | 100% | 100% | 33% | 100% | 100% | 100% | 100% | 100% | 100% |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 100% | 100% | 87% | 100% | 0% | 100% | 100% | 100% | 100% | 100% | 100% |

## personal


### personal · all

| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |
|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 215 / 107 | 81% [75–86] | 68% | 77% [68–85] | 0.59 | 7.5 | 0.2 |
| Almond rules only (red/amber only) | 215 / 107 | 19% [14–24] | 100% | 0% [0–0] | 0.59 | 7.5 | 0.2 |
| Almond current (rules + verify.nli, ≠ supported) | 215 / 107 | 65% [59–71] | 84% | 24% [17–33] | 0.71 | 2786.8 | 74.5 |
| Almond current (red/amber only) | 215 / 107 | 65% [59–71] | 84% | 24% [17–33] | 0.71 | 2786.8 | 74.5 |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 215 / 107 | 40% [34–46] | 82% | 18% [11–25] | 0.66 | 20.6 | 2.3 |
| SymAI-VI (multilingual + VI lexicon) | 215 / 107 | 40% [34–47] | 90% | 9% [5–15] | 0.73 | 33.1 | 5.1 |
| SummaC-ZS (mDeBERTa XNLI) | 215 / 107 | 45% [38–52] | 98% | 2% [0–5] | 0.82 | 17.0 | 0.6 |
| MiniCheck-RoBERTa-L | 215 / 107 | 85% [80–89] | 74% | 61% [51–70] | 0.74 | 11.6 | 0.3 |
| LLM judge (plain) | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.85 | 1511.2 | 7.9 |
| LLM judge (CoVe) | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.89 | 5704.3 | 69.6 |
| Hybrid: Almond rules + judge | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.85 | 1518.7 | 7.9 |
| Hybrid: rules + judge + NLI veto | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.84 | 1537.6 | 8.1 |
| Hybrid: Almond current OR SymAI-VI | 215 / 107 | 72% [66–78] | 83% | 30% [22–38] | 0.77 | 2819.8 | 79.7 |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 10 / 10 | 80% [50–100] | 100% | 0% [0–0] | 0.90 | 1639.2 | 8.8 |

### personal · lang=vi

| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |
|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 90 / 58 | 88% [81–94] | 62% | 84% [74–93] | 0.56 | 7.5 | 0.2 |
| Almond rules only (red/amber only) | 90 / 58 | 11% [6–18] | 100% | 0% [0–0] | 0.56 | 7.5 | 0.2 |
| Almond current (rules + verify.nli, ≠ supported) | 90 / 58 | 56% [46–66] | 75% | 29% [19–41] | 0.63 | 2786.8 | 74.5 |
| Almond current (red/amber only) | 90 / 58 | 56% [46–66] | 75% | 29% [19–41] | 0.63 | 2786.8 | 74.5 |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 90 / 58 | 7% [2–12] | 60% | 7% [2–14] | 0.55 | 21.6 | 2.3 |
| SymAI-VI (multilingual + VI lexicon) | 90 / 58 | 31% [22–40] | 85% | 9% [2–17] | 0.65 | 32.9 | 5.1 |
| SummaC-ZS (mDeBERTa XNLI) | 90 / 58 | 28% [19–37] | 100% | 0% [0–0] | 0.73 | 21.2 | 0.6 |
| MiniCheck-RoBERTa-L | 90 / 58 | 97% [92–100] | 61% | 95% [88–100] | 0.62 | 14.7 | 0.3 |
| LLM judge (plain) | 5 / 5 | 60% [20–100] | 100% | 0% [0–0] | 0.80 | 1611.4 | 7.9 |
| LLM judge (CoVe) | 5 / 5 | 60% [20–100] | 100% | 0% [0–0] | 0.86 | 6432.8 | 69.6 |
| Hybrid: Almond rules + judge | 5 / 5 | 60% [20–100] | 100% | 0% [0–0] | 0.80 | 1618.8 | 7.9 |
| Hybrid: rules + judge + NLI veto | 5 / 5 | 60% [20–100] | 100% | 0% [0–0] | 0.72 | 1642.6 | 8.1 |
| Hybrid: Almond current OR SymAI-VI | 90 / 58 | 64% [54–73] | 73% | 36% [24–50] | 0.69 | 2819.6 | 79.7 |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 5 / 5 | 80% [40–100] | 100% | 0% [0–0] | 0.90 | 1727.1 | 8.8 |

### personal · lang=en

| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |
|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 125 / 49 | 76% [68–83] | 74% | 67% [53–80] | 0.63 | 7.5 | 0.2 |
| Almond rules only (red/amber only) | 125 / 49 | 25% [17–33] | 100% | 0% [0–0] | 0.63 | 7.5 | 0.2 |
| Almond current (rules + verify.nli, ≠ supported) | 125 / 49 | 72% [64–80] | 91% | 18% [8–31] | 0.78 | 2786.8 | 74.5 |
| Almond current (red/amber only) | 125 / 49 | 72% [64–80] | 91% | 18% [8–31] | 0.78 | 2786.8 | 74.5 |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 125 / 49 | 64% [56–72] | 84% | 31% [18–45] | 0.71 | 19.8 | 2.3 |
| SymAI-VI (multilingual + VI lexicon) | 125 / 49 | 46% [38–54] | 92% | 10% [2–18] | 0.78 | 33.3 | 5.1 |
| SummaC-ZS (mDeBERTa XNLI) | 125 / 49 | 58% [49–66] | 97% | 4% [0–10] | 0.89 | 16.3 | 0.6 |
| MiniCheck-RoBERTa-L | 125 / 49 | 76% [68–83] | 90% | 20% [10–33] | 0.87 | 10.5 | 0.3 |
| LLM judge (plain) | 5 / 5 | 80% [40–100] | 100% | 0% [0–0] | 0.90 | 1499.0 | 7.9 |
| LLM judge (CoVe) | 5 / 5 | 80% [40–100] | 100% | 0% [0–0] | 0.90 | 4952.2 | 69.6 |
| Hybrid: Almond rules + judge | 5 / 5 | 80% [40–100] | 100% | 0% [0–0] | 0.90 | 1506.4 | 7.9 |
| Hybrid: rules + judge + NLI veto | 5 / 5 | 80% [40–100] | 100% | 0% [0–0] | 0.88 | 1525.3 | 8.1 |
| Hybrid: Almond current OR SymAI-VI | 125 / 49 | 78% [70–85] | 90% | 22% [12–35] | 0.83 | 2820.0 | 79.7 |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 5 / 5 | 80% [40–100] | 100% | 0% [0–0] | 0.90 | 1567.8 | 8.8 |

### personal · sample20

| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |
|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 10 / 10 | 90% [70–100] | 50% | 90% [70–100] | 0.59 | 7.5 | 0.2 |
| Almond rules only (red/amber only) | 10 / 10 | 20% [0–50] | 100% | 0% [0–0] | 0.59 | 7.5 | 0.2 |
| Almond current (rules + verify.nli, ≠ supported) | 10 / 10 | 60% [30–90] | 75% | 20% [0–50] | 0.69 | 2786.8 | 74.5 |
| Almond current (red/amber only) | 10 / 10 | 60% [30–90] | 75% | 20% [0–50] | 0.69 | 2786.8 | 74.5 |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 10 / 10 | 20% [0–50] | 40% | 30% [0–60] | 0.60 | 50.3 | 2.3 |
| SymAI-VI (multilingual + VI lexicon) | 10 / 10 | 40% [10–70] | 100% | 0% [0–0] | 0.85 | 104.5 | 5.1 |
| SummaC-ZS (mDeBERTa XNLI) | 10 / 10 | 40% [10–70] | 100% | 0% [0–0] | 0.76 | 16.2 | 0.6 |
| MiniCheck-RoBERTa-L | 10 / 10 | 90% [70–100] | 60% | 60% [30–90] | 0.68 | 11.6 | 0.3 |
| LLM judge (plain) | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.85 | 1511.2 | 7.9 |
| LLM judge (CoVe) | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.89 | 5704.3 | 69.6 |
| Hybrid: Almond rules + judge | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.85 | 1518.7 | 7.9 |
| Hybrid: rules + judge + NLI veto | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.84 | 1537.6 | 8.1 |
| Hybrid: Almond current OR SymAI-VI | 10 / 10 | 70% [40–100] | 78% | 20% [0–50] | 0.85 | 2891.3 | 79.7 |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 10 / 10 | 80% [50–100] | 100% | 0% [0–0] | 0.90 | 1639.2 | 8.8 |

### personal · subset=personal

| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |
|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 215 / 107 | 81% [75–86] | 68% | 77% [68–85] | 0.59 | 7.5 | 0.2 |
| Almond rules only (red/amber only) | 215 / 107 | 19% [14–24] | 100% | 0% [0–0] | 0.59 | 7.5 | 0.2 |
| Almond current (rules + verify.nli, ≠ supported) | 215 / 107 | 65% [59–71] | 84% | 24% [17–33] | 0.71 | 2786.8 | 74.5 |
| Almond current (red/amber only) | 215 / 107 | 65% [59–71] | 84% | 24% [17–33] | 0.71 | 2786.8 | 74.5 |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 215 / 107 | 40% [34–46] | 82% | 18% [11–25] | 0.66 | 20.6 | 2.3 |
| SymAI-VI (multilingual + VI lexicon) | 215 / 107 | 40% [34–47] | 90% | 9% [5–15] | 0.73 | 33.1 | 5.1 |
| SummaC-ZS (mDeBERTa XNLI) | 215 / 107 | 45% [38–52] | 98% | 2% [0–5] | 0.82 | 17.0 | 0.6 |
| MiniCheck-RoBERTa-L | 215 / 107 | 85% [80–89] | 74% | 61% [51–70] | 0.74 | 11.6 | 0.3 |
| LLM judge (plain) | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.85 | 1511.2 | 7.9 |
| LLM judge (CoVe) | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.89 | 5704.3 | 69.6 |
| Hybrid: Almond rules + judge | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.85 | 1518.7 | 7.9 |
| Hybrid: rules + judge + NLI veto | 10 / 10 | 70% [40–90] | 100% | 0% [0–0] | 0.84 | 1537.6 | 8.1 |
| Hybrid: Almond current OR SymAI-VI | 215 / 107 | 72% [66–78] | 83% | 30% [22–38] | 0.77 | 2819.8 | 79.7 |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 10 / 10 | 80% [50–100] | 100% | 0% [0–0] | 0.90 | 1639.2 | 8.8 |

### personal · recall by error type (all languages; n in header)

| system | number (19) | entity (12) | negation (29) | hedge_drop (20) | hedge_add (25) | quantifier (14) | temporal (11) | speaker (29) | fabricate (20) | overgeneralise (28) | cluster_insert (8) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 74% | 58% | 76% | 80% | 76% | 79% | 91% | 83% | 100% | 82% | 100% |
| Almond rules only (red/amber only) | 47% | 25% | 0% | 0% | 0% | 0% | 9% | 0% | 100% | 0% | 100% |
| Almond current (rules + verify.nli, ≠ supported) | 84% | 67% | 76% | 20% | 36% | 14% | 46% | 79% | 100% | 82% | 100% |
| Almond current (red/amber only) | 84% | 67% | 76% | 20% | 36% | 14% | 46% | 79% | 100% | 82% | 100% |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 42% | 67% | 55% | 50% | 28% | 36% | 18% | 17% | 65% | 21% | 75% |
| SymAI-VI (multilingual + VI lexicon) | 53% | 33% | 55% | 30% | 0% | 43% | 46% | 7% | 100% | 32% | 100% |
| SummaC-ZS (mDeBERTa XNLI) | 47% | 50% | 72% | 5% | 4% | 14% | 0% | 41% | 100% | 61% | 100% |
| MiniCheck-RoBERTa-L | 90% | 92% | 93% | 40% | 76% | 79% | 91% | 100% | 100% | 79% | 100% |
| LLM judge (plain) | 100% | 100% | 100% | – | 0% | – | 0% | 100% | 100% | 100% | – |
| LLM judge (CoVe) | 100% | 100% | 100% | – | 0% | – | 0% | 100% | 100% | 100% | – |
| Hybrid: Almond rules + judge | 100% | 100% | 100% | – | 0% | – | 0% | 100% | 100% | 100% | – |
| Hybrid: rules + judge + NLI veto | 100% | 100% | 100% | – | 0% | – | 0% | 100% | 100% | 100% | – |
| Hybrid: Almond current OR SymAI-VI | 90% | 67% | 86% | 35% | 36% | 50% | 73% | 79% | 100% | 82% | 100% |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 100% | 100% | 100% | – | 0% | – | 100% | 100% | 100% | 100% | – |

### personal · recall by error type, lang=vi

| system | number (10) | entity (3) | negation (10) | hedge_drop (10) | hedge_add (10) | quantifier (10) | temporal (10) | speaker (10) | fabricate (5) | overgeneralise (10) | cluster_insert (2) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 70% | 0% | 90% | 90% | 90% | 100% | 90% | 90% | 100% | 100% | 100% |
| Almond rules only (red/amber only) | 20% | 0% | 0% | 0% | 0% | 0% | 10% | 0% | 100% | 0% | 100% |
| Almond current (rules + verify.nli, ≠ supported) | 70% | 33% | 90% | 20% | 50% | 10% | 40% | 70% | 100% | 70% | 100% |
| Almond current (red/amber only) | 70% | 33% | 90% | 20% | 50% | 10% | 40% | 70% | 100% | 70% | 100% |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 0% | 0% | 0% | 10% | 10% | 20% | 10% | 0% | 0% | 10% | 0% |
| SymAI-VI (multilingual + VI lexicon) | 30% | 33% | 50% | 10% | 0% | 30% | 40% | 0% | 100% | 40% | 100% |
| SummaC-ZS (mDeBERTa XNLI) | 10% | 33% | 50% | 0% | 0% | 20% | 0% | 20% | 100% | 70% | 100% |
| MiniCheck-RoBERTa-L | 90% | 100% | 100% | 80% | 100% | 100% | 100% | 100% | 100% | 100% | 100% |
| LLM judge (plain) | – | 100% | 100% | – | 0% | – | 0% | 100% | – | – | – |
| LLM judge (CoVe) | – | 100% | 100% | – | 0% | – | 0% | 100% | – | – | – |
| Hybrid: Almond rules + judge | – | 100% | 100% | – | 0% | – | 0% | 100% | – | – | – |
| Hybrid: rules + judge + NLI veto | – | 100% | 100% | – | 0% | – | 0% | 100% | – | – | – |
| Hybrid: Almond current OR SymAI-VI | 80% | 33% | 100% | 20% | 50% | 40% | 70% | 70% | 100% | 70% | 100% |
| Proposal: judge + Almond atoms + VI/EN distortion rules | – | 100% | 100% | – | 0% | – | 100% | 100% | – | – | – |

### personal · recall by error type, lang=en

| system | number (9) | entity (9) | negation (19) | hedge_drop (10) | hedge_add (15) | quantifier (4) | temporal (1) | speaker (19) | fabricate (15) | overgeneralise (18) | cluster_insert (6) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Almond rules only (≠ supported) | 78% | 78% | 68% | 70% | 67% | 25% | 100% | 79% | 100% | 72% | 100% |
| Almond rules only (red/amber only) | 78% | 33% | 0% | 0% | 0% | 0% | 0% | 0% | 100% | 0% | 100% |
| Almond current (rules + verify.nli, ≠ supported) | 100% | 78% | 68% | 20% | 27% | 25% | 100% | 84% | 100% | 89% | 100% |
| Almond current (red/amber only) | 100% | 78% | 68% | 20% | 27% | 25% | 100% | 84% | 100% | 89% | 100% |
| SymAI (MiniLM+DeBERTa, EN lexicon) | 89% | 89% | 84% | 90% | 40% | 75% | 100% | 26% | 87% | 28% | 100% |
| SymAI-VI (multilingual + VI lexicon) | 78% | 33% | 58% | 50% | 0% | 75% | 100% | 10% | 100% | 28% | 100% |
| SummaC-ZS (mDeBERTa XNLI) | 89% | 56% | 84% | 10% | 7% | 0% | 0% | 53% | 100% | 56% | 100% |
| MiniCheck-RoBERTa-L | 89% | 89% | 90% | 0% | 60% | 25% | 0% | 100% | 100% | 67% | 100% |
| LLM judge (plain) | 100% | – | – | – | 0% | – | – | – | 100% | 100% | – |
| LLM judge (CoVe) | 100% | – | – | – | 0% | – | – | – | 100% | 100% | – |
| Hybrid: Almond rules + judge | 100% | – | – | – | 0% | – | – | – | 100% | 100% | – |
| Hybrid: rules + judge + NLI veto | 100% | – | – | – | 0% | – | – | – | 100% | 100% | – |
| Hybrid: Almond current OR SymAI-VI | 100% | 78% | 79% | 50% | 27% | 75% | 100% | 84% | 100% | 89% | 100% |
| Proposal: judge + Almond atoms + VI/EN distortion rules | 100% | – | – | – | 0% | – | – | – | 100% | 100% | – |
