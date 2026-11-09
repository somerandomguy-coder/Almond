# ADR: Neuro-symbolic verification of AI claims

Status: accepted for v0.1 (baselines shipped in `crates/almond-verify`), open for research.
Owner: WS-H. Date: 2026-11-09.

## Context

Almond's AI writes summaries, enhanced notes and action items from meeting transcripts
(Vietnamese, English, code-switched). Every AI bullet carries `cites: [SpanRef]`, but a citation
is only a promise. We want to *check* the promise, show the user why, and leave room for research
on neuro-symbolic checking without changing the shared contract (`almond-types`, frozen at
`m0-contracts`).

The contract already gives us the hooks:

- `Claim { text, source: SpanRef, evidence: [SpanRef], logical_form_kind?, logical_form?: Json }`
- `Verdict { verifier, label: supported|contradicted|unverifiable|needs_review, confidence,
  evidence: [SpanRef], explanation, trace?: Json }`
- `Verifier` trait, wrapped as pipeline `Processor`s; external processors over JSON-RPC.

What ships now (see `crates/almond-verify/EVAL.md` for numbers):

```
summary/enhanced_notes/action_items
   └─ verify.extract_claims (rules | LLM + JSON schema)  ── claims (+ triples.v1)
         └─ verify.all ── citation_exists · numbers · dates · entities · calendar   (symbolic)
                       ── consistency (ascent Datalog over triples)                (symbolic)
                       ── nli (LLM judge, must quote evidence)                      (neural)
                       └─ verify.aggregate (precedence.v1)  ── verdicts
```

## Decision 1 — logical form: `triples.v1` now, Datalog facts derived from it

Options:

| Option | + | − |
|---|---|---|
| **Triples** `(s, p, o)` with typed literal objects | trivial for a 4–9B LLM to emit under a JSON schema; maps 1:1 to Datalog facts; easy to show in UI; good fit for meeting facts (owner, due, amount, decision) | no quantifiers, negation, modality ("proposed" vs "decided") or n-ary relations without reification |
| Datalog facts with a fixed schema (`owner(task, person)`, `due(task, date)`) | directly executable | schema must be designed up front; LLMs drift from it; every new relation is a code change |
| AMR / first-order logic | expressive (negation, modality, quantifiers) | small local models produce poor AMR/FOL, especially in Vietnamese; parsing/grounding cost; hard to explain to users |

**Chosen:** `logical_form_kind = "triples.v1"`:
`{"triples":[{"s": str, "p": str, "o": {"type": person|entity|number|money|percent|date|text, ...}}]}`.

- Predicates are normalized to a small vocabulary (`owner, due, amount, count, percent, date,
  status, decision, attendee`); unknown predicates are kept in snake_case.
- **Literals are re-normalized by our own parsers**, not trusted from the LLM: "hai trăm triệu",
  "200tr" → `money 200000000 VND`; "thứ Sáu", "trước tháng 12" → resolved against the meeting date.
  LLMs are poor at Vietnamese number words and date arithmetic; symbolic normalization is cheap and
  deterministic.
- Claims without a logical form get heuristic triples (owner from "X sẽ/will …", amounts, deadlines).
- New forms (e.g. `datalog.v1`, `amr.v1`, `fol.tptp.v1`) are just new `logical_form_kind` strings;
  `VerifierDescriptor.logical_forms` advertises which a verifier understands. No contract change.

## Decision 2 — rule engine: in-process Datalog (`ascent`), heavy solvers out of process

| Option | + | − |
|---|---|---|
| **`ascent`** (Rust macro Datalog) | in-process, fast, compiles to plain Rust; stratified negation; derivations easy to reconstruct; no runtime deps; works on Android | rules are compiled in (changing them = rebuild); no arithmetic theories |
| `crepe` | similar, smaller | less active, fewer features (aggregation) |
| Soufflé | very fast, provenance support | C++ toolchain, separate process; not on Android |
| Z3 / SMT | arithmetic, dates, quantifiers, UNSAT cores as proofs | heavy native dep; overkill for current rules |
| Prolog (SWI/Scryer) | flexible, natural proof trees | runtime + FFI or process |

**Chosen:** `ascent` behind the `datalog` feature (default on) for `verify.consistency`; anything
heavier (Z3, Soufflé, Prolog, fine-tuned NLI models in Python) runs as an **external processor**
(`processor.describe` / `processor.run` over JSON-RPC, docs/API.md) consuming `claims` and producing
`verdicts`. That keeps the core small and lets research code iterate without touching Rust.

Current program `consistency.v1`:

```
conflict(C1,C2,S,P,O1,O2)   <- triple(C1,S,P,T,O1), triple(C2,S,P,T,O2), functional(P), C1<C2, O1!=O2.
clash(C,D,…) both directions of conflict.
blamed(C,D,S,P,O,OD)        <- clash(C,D,S,P,O,OD), grounded(D,P,OD), !grounded(C,P,O).
unresolved(C,D,S,P,O,OD)    <- clash(C,D,S,P,O,OD), !grounded(C,P,O), !grounded(D,P,OD).
owner_not_attendee(C,S,W)   <- owner(C,S,W), !attendee(W).
due_before_meeting(C,S,D,M) <- due_day(C,S,D), meeting_day(M), D<M.
```

`grounded(C,P,O)` is the neuro-symbolic seam: it is computed outside the rules (today by symbolic
normalizers over cited spans; tomorrow by an NLI model with a probability) and turns a symmetric
conflict into blame. Without it, a conflict flags both claims (7 false positives on the dev set);
with it, the grounded claim is cleared and only the ungrounded one is flagged (0 false positives).

## Decision 3 — proofs in `Verdict.trace`

Every symbolic verdict carries `trace.rule = "<verifier>.v1"` plus a derivation:

- Atomic verifiers (numbers/dates/entities/calendar): `checks: [{claim atom, status:
  match|partial|uncited|absent|conflict, evidence: {span, normalized value}}]`, e.g.
  `{"claim": {"value": 3e8, "kind": "money", "currency": "VND", "surface": "300 triệu đồng"},
  "status": "conflict", "evidence": [{"span": {...segment 1}, "quantity": {"value": 2e8, ...}}]}`.
- Datalog: `{"engine": "ascent", "program": "consistency.v1", "derivations": [{"rule":
  "blamed", "head": {"rel": "blamed", "args": [...]}, "body": [<sub-derivation>, {"rel":
  "grounded", ...}, {"not": {"rel": "grounded", ...}}]}]}` — a proof tree whose leaves are facts
  (triples, grounding, calendar facts) or builtins (`{"builtin": "<", "args": [...]}`).
- Neural: `{"prompt": "nli.v1", "model": "ollama:<name>@<digest>", "raw_label", "quote",
  "quote_found"}`; the judge must quote the evidence and an unverifiable quote downgrades the
  verdict to `needs_review`.
- Aggregate: `{"policy": "precedence.v1", "fired": "R1".."R7", "decisive": [...], "inputs": [...]}`.

Shape rule for future verifiers: `trace.rule` names the program/version; any `{rel, args}` node is a
fact, `{not: …}` is negation-as-failure, `{builtin: …}` an evaluated comparison, and nested objects
with `head`/`body` are sub-proofs. The UI can render any such tree generically.

### Aggregation (`precedence.v1`)

R1 any `contradicted` ≥ 0.7 → contradicted · R2 weaker contradiction → needs_review · R3 any
needs_review → needs_review · R4 holistic judge (NLI) decides supported/unverifiable (needs_review if
atoms check out but the judge disagrees) · R5 atomic support → supported (conf ≤ 0.8) · R6 only
"citation exists" → unverifiable · R7 nothing → unverifiable. A single precise counter-example beats
any amount of fuzzy support; "the citation exists" is never content support.

## Decision 4 — how the UI should show it

- Underline per bullet from the aggregate label: green (supported), red (contradicted), amber
  (needs_review), grey dotted (unverifiable). Never hide the bullet.
- Hover card: aggregate explanation + "why" list, one line per decisive verifier, each with the
  evidence spans as clickable transcript links (`SpanRef` → scroll + highlight + play audio
  `time_ms`).
- "Show proof" expander renders `trace` generically: atoms as `claim value ↔ evidence value` chips
  with ✓/✗; Datalog derivations as an indented tree (facts as chips, `not` in italics); NLI shows the
  quoted span highlighted in the transcript.
- One-click actions: "Fix to evidence value" (when a numbers/dates check gives a single conflicting
  value), "Mark correct" (user feedback, stored as a verdict with `verifier = "user"`), "Re-run".
- Language: explanations are generated in English today; the UI should template them from
  `trace` (status + values) so they can be localized to Vietnamese.

## Consequences

- Strong, cheap baselines today with full provenance; no contract change needed for new logical
  forms, rules or solvers.
- Known limits: heuristic NER for Vietnamese names; subject matching in Datalog is string equality
  after normalization (no entity linking); modality ("proposed" vs "decided") is invisible to
  symbolic checks and is left to the NLI judge; time zones come from `VerifyConfig`.

## Research roadmap

1. **Grounding as probabilistic facts.** Replace boolean `grounded` with NLI probabilities and run
   probabilistic Datalog (ProbLog-style weighted model counting, or Scallop's provenance semirings)
   so blame carries calibrated confidence.
2. **Self-consistency.** Sample k extractions / k judge runs at temperature > 0; use agreement as
   confidence and disagreement as `needs_review`. Cheap on a local GPU.
3. **Abstention & calibration.** Calibrate each verifier's confidence on labelled data
   (temperature scaling / isotonic), learn the aggregation weights, and let the system say
   "unverifiable" instead of guessing (selective prediction; report coverage–risk curves).
4. **Modality & negation.** Add `status`/`modality` to triples (`proposed | decided | rejected |
   question`) extracted by the LLM, with rules like `decided(X) ∧ evidence says proposed(X) →
   contradicted`; this targets the "invented decision" class that symbolic checks miss.
5. **Cross-meeting knowledge base.** Persist grounded triples per meeting (an artifact kind
   `facts`); run consistency across meetings ("budget was 200tr last week, 300tr today" →
   needs_review / "changed since"), with temporal validity intervals.
6. **Calendar and email as ground truth.** Attendees, times and RSVPs from calendar, commitments
   and dates from email threads as extra fact sources (`calendar_event`, `mail` artifacts) — the
   same Datalog program, more EDB.
7. **Richer logic out of process.** Z3 for date/number arithmetic ("two weeks after the launch"),
   UNSAT cores as proofs; Soufflé/Prolog via the external processor API; compare against `ascent`.
8. **Better NLI.** Distil the LLM judge into a small multilingual NLI cross-encoder (XLM-R/mDeBERTa
   fine-tuned on Vietnamese) for speed on the phone; keep the LLM as an escalation path.
9. **Benchmarks.** Grow `eval/` with real (consented, anonymized) meetings and synthetic
   perturbations per error class; report per-class recall, precision, and human-verified
   explanation faithfulness.
