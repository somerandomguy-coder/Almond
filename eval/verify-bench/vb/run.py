"""Step 3: run one system over one dataset; per-item outputs go next to the dataset.

    uv run python -m vb.run SYSTEM DATASET [--limit N]
    SYSTEM  : summac | minicheck | minicheck_mt | qa | judge | cove | selfcheck | symai | symai_vi
    DATASET : synthetic (OUT) | personal (PRIVATE)

Every system emits, per scored bullet: label in {supported, unsupported, contradicted}, an error
score in [0, 1] (higher = more likely wrong; used for AUROC) and the latency it took (ms).
LLM latency is the time of the request when it was first made (responses are cached, so re-runs
are free but keep honest latencies).
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

from vb.build import read_jsonl
from vb.paths import OUT, PRIVATE


def dataset(name: str) -> tuple[Path, Path, Path]:
    base = OUT if name == "synthetic" else PRIVATE
    return base / f"{name}_items.jsonl", base / f"{name}.json", base / "results"


def evidence_text(item: dict, quote: bool = False) -> str:
    return "\n".join(f'[{k}] {spk}: {txt}' for k, spk, txt in item["evidence"])


def main() -> None:
    system, ds = sys.argv[1], sys.argv[2]
    limit = int(sys.argv[sys.argv.index("--limit") + 1]) if "--limit" in sys.argv else None
    items_p, variants_p, res_dir = dataset(ds)
    res_dir.mkdir(parents=True, exist_ok=True)
    items = read_jsonl(items_p)[:limit]
    cache_dir = (OUT if ds == "synthetic" else PRIVATE) / "cache"
    t0 = time.time()
    if system in ("symai", "symai_vi"):
        from vb.systems.symai_sys import run as run_symai
        rows = run_symai(system, json.loads(variants_p.read_text(encoding="utf-8")), {i["item"] for i in items},
                         cache_dir)
    elif system in ("summac", "minicheck", "minicheck_mt"):
        from vb.systems import nli_sys
        rows = getattr(nli_sys, system)(items, cache_dir)
    else:
        from vb.systems import llm_sys
        rows = getattr(llm_sys, system)(items, cache_dir)
    out = res_dir / f"{system}.jsonl"
    with out.open("w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"{system} on {ds}: {len(rows)} rows in {time.time() - t0:.0f} s -> {out.name}")


if __name__ == "__main__":
    main()
