"""Where things live.

Everything derived from the user's own recordings stays under PRIVATE (the gitignored
`personal_record/` folder of the *main* checkout); synthetic data and results go to OUT
(gitignored `eval/verify-bench/out/`). Only aggregate numbers ever leave these folders.
"""

from __future__ import annotations

import os
from pathlib import Path

BENCH = Path(__file__).resolve().parents[1]
REPO = BENCH.parents[1]
OUT = Path(os.environ.get("VB_OUT", BENCH / "out"))

# The worktree has no personal_record/; the main checkout does.
_default_private = Path(r"C:\Users\Nam\Projects\Deploying\SWE\Almond\personal_record\bench")
PRIVATE = Path(os.environ.get("VB_PRIVATE", _default_private))

SYMAI_SRC = Path(os.environ.get("SYMAI_SRC", r"C:\Users\Nam\Projects\Deploying\SWE\SymAI\src"))
SYMAI_DATA = SYMAI_SRC.parent / "data" / "synthetic"

ALMOND_FIXTURES = [REPO / "crates/almond-verify/eval/fixtures.json", REPO / "crates/almond-verify/eval/heldout.json"]

OLLAMA = os.environ.get("OLLAMA_URL", "http://127.0.0.1:11434")
MODEL = os.environ.get("VB_MODEL", "gemma4:12b")
CHECK_MODEL = os.environ.get("VB_CHECK_MODEL", "qwen3.5:4b")  # independent cross-check for clean bullets


def ensure(*dirs: Path) -> None:
    for d in dirs:
        d.mkdir(parents=True, exist_ok=True)
