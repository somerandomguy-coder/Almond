# Summary eval (WS-C)

10 short synthetic meetings written for this eval (no real data): 5 Vietnamese (with natural
VI–EN code-switching: "deploy", "staging", "KOL", "take-home", ...) and 5 English (one with
Vietnamese names/phrases). Each file in `data/` has:

- `segments`: `[speaker, text]` (timestamps are synthesized by the harness),
- `key_points`: reference points with keyword groups (all groups must match; any alternative in a
  group matches; diacritics-insensitive) and the gold supporting segment indices,
- `action_items`: reference action items with keyword groups.

Run (needs Ollama; models must already be pulled — the harness exits non-zero if a model is
missing or any call fails):

```text
OLLAMA_HOST=http://127.0.0.1:11434 \
cargo run --release -p almond-llm --example eval_summary -- \
  --models qwen3.5:4b,gemma4:12b,llama3.1:8b --judge gemma4:12b --cross [--template-lang en]
```

Writes `results/rows*.json` (per-run metrics + the generated summaries) and prints the markdown
table used in [RESULTS.md](RESULTS.md).

Metrics: see the header of `crates/almond-llm/examples/eval_summary.rs`.
