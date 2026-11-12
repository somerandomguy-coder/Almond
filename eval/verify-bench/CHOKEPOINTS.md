# Choke points of the fast fact-checkers (verify-bench, 2026-11-12)

Where SymAI, SummaC, MiniCheck and Almond's own rules fail, and why. The aim is to decide what
`crates/almond-verify` should do next on its symbolic side. The analysis reuses the per-claim results
of the existing run (`out/results/*.jsonl` in the bench worktree, and `personal_record/bench/results/`).
Nothing was re-scored except SymAI-EN on 5 synthetic items, on CPU, to get alignment traces (§3).
No LLM or Ollama call was made.

**Privacy.** Personal (real-recording) data appears here only as counts, rates and generic
categories. Every quoted example is synthetic: Almond's fixtures or SymAI's council minutes.

**Systems.**

- **Almond rules v1** is `almond_symbolic`: the rules before today's fixes, scored "≠ supported", so
  `unverifiable` counts as flagged.
- **Almond rules v2** is derived from `almond_full_v2`. A claim counts as flagged when any *non-LLM*
  verifier returns `contradicted` or `needs_review`. Those verifiers are numbers, dates, entities,
  consistency, calendar, citation and the new `distortion`. This is the symbolic side as it stands
  today.
- **Almond v2 + LLM** is `almond_full_v2`, the full pipeline with `verify.nli` and gemma. It is shown
  only as a reference.

Cells show *flagged / n*: recall on error columns, false alarms (FA) on clean bullets. **Bold** marks
a choke: recall ≤ 30% with n ≥ 4, or FA ≥ 20%.

## 1. Tables

### 1.1 Recall and false alarms by error type × language × dataset

**Synthetic, VI** (12 clean, 54 errors; no quantifier items)

| system | FA clean | recall all | num (9) | ent (2) | neg (4) | h-drop (4) | h-add (6) | time (9) | spk (8) | fab (5) | overgen (1) | cluster (6) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| SymAI (EN) | 0/12 | 44% | 5/9 | 1/2 | 4/4 | **1/4** | **0/6** | 3/9 | 4/8 | 2/5 | 0/1 | 4/6 |
| SymAI-VI | 1/12 | 63% | 9/9 | 1/2 | 3/4 | 3/4 | **0/6** | 6/9 | **2/8** | 4/5 | 0/1 | 6/6 |
| SummaC-ZS | 1/12 | 69% | 9/9 | 1/2 | 3/4 | 3/4 | **0/6** | 7/9 | **2/8** | 5/5 | 1/1 | 6/6 |
| MiniCheck | **4/12** | 87% | 9/9 | 2/2 | 4/4 | 3/4 | 4/6 | 8/9 | 5/8 | 5/5 | 1/1 | 6/6 |
| Almond rules v1 | 0/12 | 72% | 9/9 | 2/2 | **0/4** | **1/4** | **0/6** | 8/9 | 8/8 | 5/5 | 0/1 | 6/6 |
| Almond rules v2 | 0/12 | 80% | 9/9 | 2/2 | 4/4 | 2/4 | **0/6** | 7/9 | 8/8 | 5/5 | 0/1 | 6/6 |
| *Almond v2 + LLM* | 0/12 | 91% | 9/9 | 2/2 | 4/4 | 4/4 | **1/6** | 9/9 | 8/8 | 5/5 | 1/1 | 6/6 |

**Synthetic, EN** (103 clean, 116 errors; 91 clean and 79 errors are SymAI's own minutes, which are in-sample for SymAI)

| system | FA clean | recall all | num (16) | ent (5) | neg (15) | h-drop (8) | h-add (6) | quant (6) | time (15) | spk (8) | fab (13) | overgen (2) | cluster (22) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| SymAI (EN) | 3/103 | 95% | 15/16 | 5/5 | 13/15 | 8/8 | 5/6 | 6/6 | 14/15 | 7/8 | 13/13 | 2/2 | 22/22 |
| SymAI-VI | 4/103 | 87% | 15/16 | 5/5 | 11/15 | 8/8 | 2/6 | 6/6 | 13/15 | 6/8 | 12/13 | 2/2 | 21/22 |
| SummaC-ZS | 9/103 | 83% | 14/16 | 4/5 | 11/15 | 4/8 | 4/6 | 3/6 | 13/15 | 6/8 | 13/13 | 2/2 | 22/22 |
| MiniCheck | 12/103 | 93% | 16/16 | 5/5 | 15/15 | 3/8 | 5/6 | 4/6 | 15/15 | 8/8 | 13/13 | 2/2 | 22/22 |
| Almond rules v1 | **38/103** | 70% | 16/16 | 5/5 | **3/15** | **2/8** | **0/6** | **0/6** | 12/15 | 8/8 | 13/13 | 0/2 | 22/22 |
| Almond rules v2 | 0/103 | 66% | 16/16 | 4/5 | **3/15** | 3/8 | **0/6** | 6/6 | 14/15 | 8/8 | 7/13 | 0/2 | 16/22 |
| *Almond v2 + LLM* | 4/103 | 89% | 16/16 | 5/5 | 14/15 | 3/8 | **0/6** | 6/6 | 15/15 | 8/8 | 13/13 | 1/2 | 22/22 |

**Personal (real ASR), VI** (58 clean, 90 errors)

| system | FA clean | recall all | num (10) | ent (3) | neg (10) | h-drop (10) | h-add (10) | quant (10) | time (10) | spk (10) | fab (5) | overgen (10) | cluster (2) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| SymAI (EN) | 4/58 | **7%** | **0/10** | 0/3 | **0/10** | **1/10** | **1/10** | **2/10** | **1/10** | **0/10** | **0/5** | **1/10** | 0/2 |
| SymAI-VI | 5/58 | 31% | **3/10** | 1/3 | 5/10 | **1/10** | **0/10** | **3/10** | 4/10 | **0/10** | 5/5 | 4/10 | 2/2 |
| SummaC-ZS | 0/58 | 28% | **1/10** | 1/3 | 5/10 | **0/10** | **0/10** | **2/10** | **0/10** | **2/10** | 5/5 | 7/10 | 2/2 |
| MiniCheck | **55/58** | 97% | 9/10 | 3/3 | 10/10 | 8/10 | 10/10 | 10/10 | 10/10 | 10/10 | 5/5 | 10/10 | 2/2 |
| Almond rules v1 | **49/58** | 88% | 7/10 | 0/3 | 9/10 | 9/10 | 9/10 | 10/10 | 9/10 | 9/10 | 5/5 | 10/10 | 2/2 |
| Almond rules v2 | 2/58 | 27% | **2/10** | 0/3 | **0/10** | **1/10** | **0/10** | **2/10** | **3/10** | **0/10** | 5/5 | 9/10 | 2/2 |
| *Almond v2 + LLM* | 9/58 | 54% | 6/10 | 1/3 | 8/10 | **1/10** | **2/10** | **3/10** | 4/10 | 7/10 | 5/5 | 10/10 | 2/2 |

**Personal (real ASR), EN** (49 clean, 125 errors)

| system | FA clean | recall all | num (9) | ent (9) | neg (19) | h-drop (10) | h-add (15) | quant (4) | time (1) | spk (19) | fab (15) | overgen (18) | cluster (6) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| SymAI (EN) | **15/49** | 64% | 8/9 | 8/9 | 16/19 | 9/10 | 6/15 | 3/4 | 1/1 | **5/19** | 13/15 | **5/18** | 6/6 |
| SymAI-VI | 5/49 | 46% | 7/9 | 3/9 | 11/19 | 5/10 | **0/15** | 3/4 | 1/1 | **2/19** | 15/15 | **5/18** | 6/6 |
| SummaC-ZS | 2/49 | 58% | 8/9 | 5/9 | 16/19 | **1/10** | **1/15** | **0/4** | 0/1 | 10/19 | 15/15 | 10/18 | 6/6 |
| MiniCheck | **10/49** | 76% | 8/9 | 8/9 | 17/19 | **0/10** | 9/15 | **1/4** | 0/1 | 19/19 | 15/15 | 12/18 | 6/6 |
| Almond rules v1 | **33/49** | 76% | 7/9 | 7/9 | 13/19 | 7/10 | 10/15 | **1/4** | 1/1 | 15/19 | 15/15 | 13/18 | 6/6 |
| Almond rules v2 | 1/49 | 35% | 7/9 | 4/9 | **1/19** | **2/10** | **0/15** | 2/4 | 0/1 | **0/19** | 15/15 | 7/18 | 6/6 |
| *Almond v2 + LLM* | 7/49 | 78% | 9/9 | 7/9 | 14/19 | 4/10 | **4/15** | 3/4 | 1/1 | 16/19 | 15/15 | 18/18 | 6/6 |

Notes:

- On personal data, the high recall of Almond rules v1 and of MiniCheck on VI is not detection. It is
  the same flag-everything behaviour that produces 84–95% FA.
- Fabrication and cluster rows read 100% on personal because the inserted claims come from generic
  business templates and are off-topic. They are easy, and those cells overstate real recall
  (REPORT §6).

### 1.2 Error types that defeat every fast method

Union of SymAI, SymAI-VI, SummaC and Almond rules v2: an error counts as caught if **any** of them
flags it. MiniCheck is left out because of its VI FA.

| cell | union of fast methods | Almond v2 + LLM | can a deterministic rule plausibly catch it? |
|---|---|---|---|
| personal VI hedge_add | 1/10 | 2/10 | Yes: flag a hedge in the claim that is absent from the evidence (§4, rec. 3). |
| personal VI hedge_drop | 2/10 | 1/10 | Partly. 6 of the 10 swap an opinion verb for an assertion verb (mutator rule "think → assert"). That needs attribution-strength rules; a first attempt was weak (§2.6). |
| personal VI speaker | 2/10 | 7/10 | Yes: diarization labels as name atoms (§4, rec. 1). |
| personal VI entity | 1/3 | 1/3 | Partly: ALL-CAPS names are dropped as acronyms (§2.5). |
| personal VI number | 5/10 | 6/10 | Mostly: 5 of 10 are unit swaps, and units are not part of Almond's quantity atom (§4, rec. 5). |
| personal VI quantifier / temporal | 6/10, 6/10 | 3/10, 4/10 | Partly: lexicon gaps plus alignment (§2.5). |
| personal EN hedge_add | 6/15 | 4/15 | Yes: same rule as VI hedge_add. |
| personal VI negation | 6/10 | 8/10 | **Needs a model.** On ALL-CAPS ASR, lexical negation either never fires or fires on clean bullets (§2.6). |
| synthetic VI hedge_add | 0/6 | 1/6 | Yes: same rule. |

### 1.3 What ASR noise does

Recall and FA, synthetic VI (clean fixtures) → personal VI (Zipformer ASR):

| system | recall | FA |
|---|---|---|
| SymAI (EN) | 44% → 7% | 0% → 7% |
| SymAI-VI | 63% → 31% | 8% → 9% |
| SummaC-ZS | 69% → 28% | 8% → 0% |
| Almond rules v2 | 80% → 27% | 0% → 3% |
| Almond v2 + LLM | 91% → 54% | 0% → 16% |

Symptoms in the personal evidence windows, and what each one breaks:

| ASR symptom (personal) | measured | what it breaks |
|---|---|---|
| VI evidence is ALL CAPS | 145/148 VI windows are ≥ 90% uppercase | **Almond**: `names.rs::is_acronym` treats every all-caps token as an acronym, so no person or entity is ever extracted from VI ASR text (entities VI 0/3). **SymAI**: `person_re` and title regexes need `Cr/Mr` + Capitalised. |
| no punctuation, run-on segments | 11/148 VI windows have any punctuation | **SymAI**: `_CLAUSE_BREAK` and sentence split need punctuation, so negation scope runs across clauses. **Almond**: negation parity over a whole window picks up unrelated "không". In simulation, a SymAI-style scoped negation at alignment ≥ 0.5 flags 11/58 VI clean bullets. |
| numbers spelled out | 0/148 VI windows contain a digit; claims use digits | Almond `numbers.rs` does parse spelled numbers. SymAI-VI's `number_words` is a flat list (up to "hai mươi", "ba mươi", "một trăm") with no compositional parse. SummaC catches 1/10 VI number errors. |
| over-split diarization | every personal item uses `S<n>` labels; 33 "speakers" in 22 min | **Almond**: `S3` is an all-caps + digit token, so it is an acronym and never a name (speaker 0/29). **SymAI**: `ACC-SPK-01` needs a titled reporter ("Cr X said") or a role alias, so it never fires on `S3 said` (0 SPK flags on 29 speaker errors). Over-splitting also makes "S3 vs S11" possibly the same person. |
| paraphrased bullets over noisy text | Almond's lexical alignment ≥ 0.5 for 51% of personal claims (VI 65%, EN 40%), ≥ 0.7 for 12%; synthetic 72% and 47% | Almond `distortion.rs` abstains below 0.5 and checks negation only at ≥ 0.7. So on ASR most claims never reach the distortion rules. |

## 2. Per-method choke points and mechanisms

### 2.1 SymAI (EN, as published)

| choke | numbers | mechanism (code) |
|---|---|---|
| any Vietnamese | personal VI 6/90; synthetic VI 24/54 | `[a-z'-]+` regexes in `symbolic/features.py` (`scope`, `entities_of`, `surname`) cut Vietnamese words into ASCII fragments: `surname("Bình")` is `"nh"`. Lexicons are English, and MiniLM and DeBERTa are English-only. Trace: EN DeBERTa gives E = 0.99 for "Có lẽ tuyển 2 kỹ sư backend…" against the unhedged turn (pure lexical overlap), and C = 0.95 for an unrelated VI turn at cosine 0.47. That is just under `anchor_strong` 0.55; slightly above it, it becomes `ACC-CON-01`, which is the mechanism behind its 4/58 personal VI clean false alarms (all `ACC-CON-01`). |
| EN ASR clean bullets | FA 15/49, 12 of them `ACC-FAB-01` | Support needs NLI entail, or cosine ≥ `support_sim` 0.65 to a single turn (`accuracy.lp`). Over-split speakers spread one statement over many short turns. Joint support only concatenates the top 3 retrieved turns, so faithful paraphrases read as fabricated. |
| speaker | personal 5/19 EN, 0/10 VI (all via FAB, never SPK) | `reporter()` matches only `Title Name said` or role aliases (chair, officer…). `S3 said` never yields `reporter(S,V)`. |
| overgeneralise "S3 said" → "Everyone said" | personal EN 5/18 | `ACC-GEN-01` keys on generic and episodic *time* markers. The group-subject form is not modelled. |
| hedge_add | no rule | Only `ACC-HDG-01` exists (hedge **dropped**). Synthetic EN 5/6 are caught incidentally by `ACC-FAB-01`, because "Possibly, X" is not entailed. |
| anchor-only comparison | synthetic VI temporal/speaker misses | Feature rules compare the claim with the *anchor* only (the best-ranked entailing turn). Trace: "Bình review API thanh toán trong hôm qua" anchors to An's turn (cosine 0.77, no time word). "hôm nay" is in Bình's turn at rank 2, so `ACC-TMP-01` has nothing to compare, and NLI says entail (0.99). |

### 2.2 SymAI-VI (same rules, multilingual models + VI lexicon)

| choke | numbers | mechanism |
|---|---|---|
| speaker | personal 2/29; synthetic VI 2/8 | Same reporter regex. The VI patch fixed `scope` and `emb.tokens`, not `entities_of` or `surname`, so diacritic names are still fragments. Example: "An review API thanh toán…" (the source says Bình will review) gets no flag. |
| hedge_add | 0/37 everywhere | No rule. |
| VI hedge_drop | personal 1/10 | 6/10 are "think → assert" mutations. "nghĩ là" is an `epistemic_attribution`, not a `hedge`, so `ACC-HDG-01` (which needs `hedge(U,_)`) cannot fire. |
| VI clean false alarms | 5/58: 3 involve `ACC-HDG-01`, 2 `ACC-NUM-01` (some bullets fire several rules) | The VI hedge lexicon includes "có thể" (also "can") and "chắc" (also "surely"). |
| thresholds | not traced | `anchor_min`, `anchor_strong` and `support_sim` are calibrated for MiniLM cosine and were reused unchanged with qwen3-embedding. This is a hypothesis: tracing it needs Ollama, which was not used. |

### 2.3 SummaC-ZS (mDeBERTa XNLI)

- **Choke:** modality. Personal hedge_drop 1/20, hedge_add 1/25, temporal 0/11, quantifier 2/14.
  Synthetic VI hedge_add 0/6.
  - **Mechanism:** zero-shot NLI is monotone. "X" entails "possibly X", "khoảng 50" ≈ "50", and
    "hôm nay" vs "hôm qua" is "compatible".
  - VI numbers: 1/10 on personal (digits in the claim against spelled-out ALL-CAPS evidence).
- **Strength:** negation (personal 21/29) and the lowest FA (2/107). It is the only fast method that
  survives ASR on negation.

### 2.4 MiniCheck-RoBERTa-L

- **Choke:** any VI input. FA 55/58 on personal VI and 4/12 on synthetic VI. It is an English-only
  model, so its VI recall carries no information.
- On EN it chokes on hedge_drop (personal 0/10, synthetic 3/8): it treats a hedged source as
  supporting the certain claim.

### 2.5 Almond rules v2 (today's symbolic side)

| choke | numbers | mechanism (crate code) |
|---|---|---|
| speaker with diarization labels | personal 0/29 | `names.rs::is_acronym` returns true for `S3` (all caps + digit), so `extract()` never yields it and `verify.entities` has nothing to check. With real names, synthetic speaker is 16/16. |
| negation | personal 1/29; synthetic EN 3/15 | `distortion.rs` checks negation only at `NEG_ALIGN` = 0.7, which 10–14% of personal claims reach. Alignment is exact folded-word overlap: no stemming ("raised concerns" vs "I'm concerned"), no number normalisation ("three" vs "3"), and the cited segment's speaker name is not counted ("Mr Whitaker raised…"). The negation list lacks `couldn/won/wouldn/shouldn`. Antonym flips are not negation: 6 synthetic EN items are carried↔lost (SymAI catches them with exclusive categoricals; in-sample for SymAI). Cross-lingual claims ("Ad spend was not 300 million VND" over VI "hết 300tr") cannot align at all. |
| hedge_add | 0/37 | No rule: the verifier only checks hedges the claim *dropped*. |
| hedge_drop | synthetic EN 3/8; personal VI 1/10 | `may/could` are missing from `hedges`. Approximators share the hedge class, so "about 14 months" in the claim masks a dropped "may slip". Opinion framing was removed on purpose, so VI "think → assert" (6/10) is invisible. |
| VI number | personal 2/10 | `numbers.rs::Quantity` has value + Plain/Money/Percent but **no unit**. "30 phút" matches "30 giờ", so 0/7 personal unit swaps (VI 5, EN 2) are caught. In 7/10 VI number claims, `verify.numbers` returned no verdict: it found no quantity, typically a bare count or a unit word. |
| VI temporal | personal 3/10 | `dates.rs` knows hôm nay, ngày mai and tomorrow but not hôm qua, yesterday, tuần trước or tháng sau/này. `distortion.rs` has planned→done only (not done→planned), and "sắp" is missing from `future`. |
| VI quantifier | personal 2/10 | Missing from the lexicon: "ít" and "hiếm khi" (no *rare* class), "thường", "luôn" (only "luôn luôn") and "chỉ". |
| VI entity | personal 0/3 | ALL-CAPS ASR names are treated as acronyms. |

### 2.6 Ideas that do *not* work (simulated on the same items)

- **Lowering the negation gate.** Window-parity negation at alignment ≥ 0.6, even with better
  alignment, gains +10 personal negation catches but raises FA from 3/107 to 8/107 (VI 6/58).
- **SymAI-style scoped negation.** It does worse on ALL-CAPS VI: 11/58 VI clean false alarms at
  alignment ≥ 0.5, because there are no clause breaks.
- **Opinion→assertion verb rule.** It fired on 0 of the 6 VI "think → assert" items (the opinion
  verb is usually not in the aligned window of a paraphrased bullet) and on 2/49 EN clean bullets.
  This error class stays open.

## 3. SymAI traces (CPU, MiniLM + DeBERTa, 5 synthetic items)

| claim (synthetic) | top alignment | rules |
|---|---|---|
| "Có lẽ tuyển 2 kỹ sư backend trước tháng 12." (hedge_add) | Minh's turn, cosine 0.76, **E 0.99** | none. No hedge_add rule exists, and the EN NLI entails the hedged copy. |
| "Bình review API thanh toán trong hôm qua." (temporal) | An's turn, cosine 0.77, E 0.99; Bình's "hôm nay" turn at rank 2 | none: the anchor has no time marker |
| "An review API thanh toán trong hôm nay." (speaker) | An's turn, cosine 0.69, E 1.00 | none: NLI cannot tell the speaker from the actor, and VI names are fragmented |
| "Priya did attend the meeting." (negation) | "Priya couldn't make it today", cosine 0.61, **C 1.00** | `ACC-CON-01` |
| "There are not three open roles." (negation) | "We have 3 open roles…", cosine 0.61, C 1.00 | `ACC-CON-01` |

SymAI's EN negation strength comes from NLI on a strongly similar turn, not from its lexical rules.
Almond missed both negation examples on lexical alignment ("attend" vs "make it", "three" vs "3").

## 4. Ranked recommendations for `almond-verify` (symbolic side)

Gains are simulated by applying each rule, written in Python, to the same items and evidence the
benchmark used. They are counted on top of today's Almond rules v2 (personal: 68/215 recall,
3/107 FA; synthetic: 120/170, 0/115).

**IS** = in-sample guess. The rule's lexicon matches the mutator's own templates, or the clean filter
already guarantees 0 FA, so the number is optimistic.

| # | change | where | personal gain (recall, FA) | synthetic gain | confidence |
|---|---|---|---|---|---|
| 1 | **Diarization labels as name atoms.** `S<n>` / known speaker ids are names, never acronyms. A claim's label must be a speaker of the cited segments ±1, else `needs_review` (not `contradicted`, because of over-splitting). | `names.rs::extract`, `verify.entities` | speaker 0/29 → 28/29; overall 32% → 45%; FA 3 → 3/107 | none (fixtures use names) | Recall is solid. **FA is IS**: clean-filter check 6 removed any bullet whose label was not cited. Over-split diarization will cause real false alarms; fixing the diarizer's labels upstream helps more. |
| 2 | **Multilingual NLI (SummaC-ZS, mDeBERTa) as a deterministic encoder signal** for contradiction and negation. It is not generative: 17 ms per claim, run as an external processor. | new `verify.xnli` (JSON-RPC) | on top of rows 1, 3–5: 132 → 161/215 (61% → 75%), FA 3 → 5/107; negation 1 → 21/29 | 144 → 166/170, FA 1 → 11/115 | Measured, not in-sample. The only fast fix for negation on ASR. Watch synthetic EN FA (9%). |
| 3 | **Hedge-added rule.** A hedge in the claim (có lẽ, hình như, maybe, possibly, likely…) that is absent from all of the evidence gives low-severity `needs_review`. Exclude the ambiguous "có thể" and "chắc". | `distortion.rs` | hedge_add 0/25 → 18/25; FA 0 | 0/12 → 10/12; FA 0 | FA 0/222 is fair (no filter enforced it). Partly IS: the mutator only adds "Possibly,"/"Có lẽ". |
| 4 | **Group-subject overgeneralisation.** "Everyone / Mọi người đều / All members …" when no group marker is in the evidence gives `needs_review`. | `distortion.rs` | overgen 16/28 → 28/28; FA 0 | none | **IS**: this is the mutator's template, and clean-filter check 7 removed unsupported scope words. |
| 5 | **Unit-aware quantities.** Add a dimension/unit to `Quantity` (duration: phút, giờ, ngày, tuần, tháng, năm; scale: nghìn, triệu, tỷ; percent) with VI↔EN canon (tỷ = billion, triệu = million). | `numbers.rs`, `verify.numbers` | number 9/19 → 13/19; FA 0 | 0 net; 3 FA before canonicalisation (EN claims over VI evidence) | Moderate: n = 7 unit swaps. |
| 6 | **Alignment normalisation in `distortion.rs`.** Count the cited segment's speaker name as present, drop `S<n>` tokens from the claim, light EN stemming, number words → digits. | `distortion.rs::align` | coverage at ≥ 0.5: 51% → 63% of personal claims (VI 65 → 71%, EN 40 → 56%) | small | An enabler: it lets the distortion rules run at all. Keep `NEG_ALIGN` at 0.7 (see §2.6). |
| 7 | **Lexicon gaps.** Each gap is closed by adding the words below. | `distortion.rs::LEXICON`, `dates.rs` | | | |
| 7a | EN modals `may` (guard the month), `could`, `should`. Approximators become their own class, so "about N" no longer masks a dropped modal. | | +1 hedge_drop | hedge_drop 5/12 → 9/12, 1 FA/115 | **IS**: SymAI subset. |
| 7b | VI quantifiers: a *rare* class (ít, hiếm khi), thường → majority, luôn → universal, chỉ. | | VI quantifier +3, FA 0 | none | **IS**: mutator words. |
| 7c | Generic/episodic markers (regularly, every day, mỗi ngày, hằng ngày, lần nào cũng, repeatedly). | | none (no such personal items) | overgen 0/3 → 3/3, FA 0 | Small n. |
| 7d | Tense: "sắp" as future; a done→planned rule (đã → sẽ); relative days hôm qua, yesterday, tuần trước, tháng sau/này in `dates.rs`. | | VI temporal +1 | small | Low n. |
| 7e | EN negation contractions: couldn, wouldn, shouldn, won. | | not measurable alone | needed for "couldn't make it" | Cheap. |
| 8 | **Exclusive categoricals** (carried/lost/deferred, approved/rejected, đồng ý/từ chối/phản đối) as a contradiction rule against the aligned window. | `distortion.rs` | none | EN negation +6, FA 0 | **IS**: SymAI's own minutes only. |

Rows 1, 3, 4, 5, 7a, 7c, 7d and 8 together, *without NLI*, score as follows. Row 6's alignment
normalisation is built into the simulation; 7b is not included.

- personal: 32% → **61%** recall at 3/107 FA. Almond v2 + LLM scores 68% recall at 16/107.
- synthetic: 71% → **85%** at 1/115.

Adding row 2 (the NLI encoder) gives **75% / 5%** on personal. That beats the current LLM pipeline on
both recall and FA. About a third of that gain rests on IS rules (1 and 4), so re-measure on new
recordings before trusting it.

Same rules on top of the LLM pipeline:

- personal: 68% → 79% recall; FA unchanged at 15%, and that FA comes from `verify.nli`.
- synthetic: 89% → 98%, FA 3% → 4%.

**Genuinely needs a model** (no plausible deterministic rule here):

- paraphrased negation ("did attend" vs "couldn't make it");
- negation on ALL-CAPS VI ASR;
- cross-lingual claims (EN bullet over VI speech);
- realistic on-topic fabrication (the template fabrications here are too easy);
- the VI "think → assert" certainty inflation.

For these the choice is NLI (row 2) or the judge. Not worth porting from SymAI:

- whole-transcript retrieval: Almond's cites are better, and SymAI's support path is the source of
  its 12/49 EN false alarms;
- English `[a-z]` features;
- anchor-only feature comparison.

## 5. Reproduce

The analysis scripts live in the session scratchpad. They are not committed and do not read or write
the repo, except to read the result files.

- `agg.py`: tables in §1.1.
- `rules.py`: rule and verifier breakdowns.
- `diag_run.py`: alignment buckets and mutation rules.
- `sim_run.py` and `combo.py`: rule simulations for §4.
- `trace.py`: the SymAI CPU traces in §3.

All of them read `synthetic_items.jsonl`, `personal_items.jsonl` and `results/*.jsonl`, and print
only aggregates for personal data.
