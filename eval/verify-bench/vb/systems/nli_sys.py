"""Encoder baselines: SummaC-ZS (multilingual NLI) and MiniCheck (English RoBERTa-L), the latter
also via gemma4:12b translation of Vietnamese inputs (`minicheck_mt`)."""

from __future__ import annotations

import re
import time

import torch
from transformers import AutoModelForSequenceClassification, AutoTokenizer

from vb.models import MINICHECK, MNLI

DEV = "cuda" if torch.cuda.is_available() else "cpu"


def _sentences(text: str) -> list[str]:
    parts = [p.strip() for p in re.split(r"(?<=[.!?…])\s+", text) if p.strip()]
    return [p for p in parts if len(p.split()) >= 2] or [text.strip()]


class NLI:
    def __init__(self, name: str = MNLI):
        self.tok = AutoTokenizer.from_pretrained(name)
        self.model = AutoModelForSequenceClassification.from_pretrained(name).to(DEV).eval().half()
        lab = {i: l.lower() for i, l in self.model.config.id2label.items()}
        self.e = next(i for i, l in lab.items() if l.startswith("entail"))
        self.c = next(i for i, l in lab.items() if l.startswith("contra"))
        self.n = next(i for i, l in lab.items() if l.startswith("neutral"))

    @torch.inference_mode()
    def predict(self, pairs: list[tuple[str, str]], bs: int = 32) -> list[tuple[float, float, float]]:
        out = []
        for k in range(0, len(pairs), bs):
            p, h = zip(*pairs[k:k + bs])
            enc = self.tok(list(p), list(h), truncation="only_first", max_length=512, padding=True,
                           return_tensors="pt").to(DEV)
            pr = torch.softmax(self.model(**enc).logits.float(), -1).cpu().tolist()
            out += [(x[self.e], x[self.n], x[self.c]) for x in pr]
        return out


def premises(item: dict) -> list[str]:
    """SummaC-ZS source units: every sentence of every evidence segment, each segment, and the
    whole window (claims often fuse two segments)."""
    units = []
    for _, spk, txt in item["evidence"]:
        seg = f"{spk}: {txt}"
        units.append(seg)
        units += [f"{spk}: {s}" for s in _sentences(txt) if s != txt.strip()]
    units.append(" ".join(f"{spk}: {txt}" for _, spk, txt in item["evidence"]))
    return list(dict.fromkeys(units))


def summac(items: list[dict], cache_dir) -> list[dict]:
    nli = NLI()
    nli.predict([("warm up", "warm up")])  # CUDA warm-up outside the timed loop
    rows = []
    for it in items:
        t0 = time.perf_counter()
        ps = premises(it)
        probs = nli.predict([(p, it["text"]) for p in ps])
        best = max(probs, key=lambda x: x[0] - x[2])
        zs = best[0] - best[2]  # SummaC-ZS sentence score: max over source units of P(e) - P(c)
        if max(p[0] for p in probs) >= 0.5:
            label = "supported"
        elif max(p[2] for p in probs) >= 0.5 and min(p[2] for p in probs) > 0.3:
            label = "contradicted"
        else:
            label = "unsupported"
        rows.append({"item": it["item"], "label": label, "score": round((1 - zs) / 2, 4),
                     "ms": (time.perf_counter() - t0) * 1000, "units": len(ps)})
    return rows


class MiniCheck:
    """lytang/MiniCheck-RoBERTa-Large: binary (supported / unsupported) over (document, claim)."""

    def __init__(self):
        self.tok = AutoTokenizer.from_pretrained("roberta-large")
        self.model = AutoModelForSequenceClassification.from_pretrained(MINICHECK).to(DEV).eval().half()

    @torch.inference_mode()
    def p_support(self, doc: str, claim: str) -> float:
        enc = self.tok(doc, claim, truncation="only_first", max_length=512, return_tensors="pt").to(DEV)
        return float(torch.softmax(self.model(**enc).logits.float(), -1)[0, 1])


def _doc(it: dict, texts: dict | None = None) -> str:
    return "\n".join(f"{spk}: {(texts or {}).get(txt, txt)}" for _, spk, txt in it["evidence"])


def minicheck(items: list[dict], cache_dir, translate: bool = False) -> list[dict]:
    mc = MiniCheck()
    mc.p_support("warm up", "warm up")
    tr = None
    if translate:
        from vb.systems.llm_sys import Translator
        tr = Translator(cache_dir)
    rows = []
    for it in items:
        mt_ms = 0.0
        claim, doc = it["text"], _doc(it)
        if tr is not None and (it["lang"] == "vi" or any(s for s in it["evidence"] if tr.is_vi(s[2]))):
            claim, ms1 = tr(claim)
            texts = {}
            for _, _, txt in it["evidence"]:
                texts[txt], ms = tr(txt)
                mt_ms += ms
            doc = _doc(it, texts)
            mt_ms += ms1
        t1 = time.perf_counter()
        p = mc.p_support(doc, claim)
        # latency = MiniCheck time + translation time when the translation was first computed
        rows.append({"item": it["item"], "label": "supported" if p >= 0.5 else "unsupported",
                     "score": round(1 - p, 4), "ms": (time.perf_counter() - t1) * 1000 + mt_ms, "mt_ms": mt_ms})
    return rows


def minicheck_mt(items, cache_dir):
    return minicheck(items, cache_dir, translate=True)
