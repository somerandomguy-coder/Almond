"""Import SymAI from its own checkout (read-only; nothing is copied or modified)."""

from __future__ import annotations

import os
import sys

from vb.paths import OUT, SYMAI_SRC

os.environ.setdefault("SYMAI_LLM", "off")  # no Claude escalation: everything stays local
os.environ.setdefault("SYMAI_DB_PATH", str(OUT / "symai-unused.db"))
if str(SYMAI_SRC) not in sys.path:
    sys.path.insert(0, str(SYMAI_SRC))

import symai  # noqa: E402,F401

CONTENT_MUTATORS = ["fabricate", "negate", "drop_hedge", "quantifier", "number", "entity", "temporal",
                    "misattribute", "generalise", "cluster_insert"]
# SymAI mutator -> benchmark type
TYPE_OF = {"fabricate": "fabricate", "negate": "negation", "drop_hedge": "hedge_drop", "quantifier": "quantifier",
           "number": "number", "entity": "entity", "temporal": "temporal", "misattribute": "speaker",
           "generalise": "overgeneralise", "cluster_insert": "cluster_insert"}
