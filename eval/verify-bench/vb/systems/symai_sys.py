"""SymAI's capture gate (perception -> clingo rules, minutes profile) on benchmark variants.

- `symai`   : SymAI as published: MiniLM retrieval + DeBERTa-v3 NLI (English), its English lexicons.
- `symai_vi`: same rules, adapted *from outside SymAI's repo*: multilingual retrieval
  (qwen3-embedding:0.6b via Ollama), multilingual NLI (mDeBERTa-v3 XNLI), the Vietnamese lexicon
  extension `vb/symai_vi_lexicon.yaml`, and Unicode-aware tokenisation for negation scope.

Models run on the GPU here (SymAI pins CPU); predictions are memoised so the unchanged sentences
of mutated variants are not re-scored. A bullet counts as flagged when a *factual* rule fires on
its sentence (ACC-*, BIAS-OPF-01, MIN-*), whatever the action (advisory/hold/block).
"""

from __future__ import annotations

import re
import time
from pathlib import Path

import requests
import yaml

from vb import symai_bridge  # noqa: F401  (sys.path + SYMAI_LLM=off)
from vb.paths import OLLAMA, OUT

FACT_PREFIX = ("ACC-", "MIN-")
FACT_EXTRA = {"BIAS-OPF-01"}
CONTRA = {"ACC-CON-01", "MIN-VOTE-01"}


def _is_fact(rule: str) -> bool:
    return rule.startswith(FACT_PREFIX) and rule != "ACC-OMI-01" or rule in FACT_EXTRA


# ------------------------------------------------------------------------------------- backends


class CachedST:
    """sentence-transformers retriever on GPU with an embedding cache."""

    def __init__(self, name: str):
        from sentence_transformers import SentenceTransformer

        self.name = name
        self.model = SentenceTransformer(name, device="cuda")
        self.cache: dict[str, object] = {}

    def _emb(self, texts):
        import torch

        missing = [t for t in dict.fromkeys(texts) if t not in self.cache]
        if missing:
            vecs = self.model.encode(missing, convert_to_tensor=True, normalize_embeddings=True,
                                     show_progress_bar=False, batch_size=64)
            for t, v in zip(missing, vecs):
                self.cache[t] = v
        return torch.stack([self.cache[t] for t in texts])

    def similarities(self, queries, docs):
        if not queries or not docs:
            return [[] for _ in queries]
        q, d = self._emb(queries), self._emb(docs)
        return (q @ d.T).float().cpu().tolist()


class OllamaEmbed(CachedST):
    """Multilingual retriever: qwen3-embedding:0.6b served by the local Ollama (no download)."""

    def __init__(self, name: str = "qwen3-embedding:0.6b"):
        self.name = f"ollama:{name}"
        self.model_name = name
        self.cache = {}

    def _emb(self, texts):
        import torch

        missing = [t for t in dict.fromkeys(texts) if t not in self.cache]
        for k in range(0, len(missing), 64):
            batch = missing[k:k + 64]
            r = requests.post(f"{OLLAMA}/api/embed", json={"model": self.model_name, "input": batch}, timeout=600)
            r.raise_for_status()
            for t, v in zip(batch, r.json()["embeddings"]):
                x = torch.tensor(v, dtype=torch.float32)
                self.cache[t] = x / x.norm().clamp_min(1e-9)
        return torch.stack([self.cache[t] for t in texts])


class CachedNLI:
    """SymAI CrossEncoderNLI interface (predict(pairs, raw_hypothesis)), GPU + memoised."""

    dual_framing = True

    def __init__(self, name: str, multilingual: bool = False):
        from symai.neural.nli import core_claim

        self.core_claim = core_claim
        self.name = name
        if multilingual:
            from vb.systems.nli_sys import NLI

            self.m = NLI(name)
            self._predict = self.m.predict
        else:
            from sentence_transformers import CrossEncoder

            ce = CrossEncoder(name, device="cuda")
            lab = {i: l.lower() for i, l in ce.model.config.id2label.items()}
            idx = {n: next(i for i, l in lab.items() if l.startswith(n[:5])) for n in ("entailment", "neutral", "contradiction")}

            def pred(pairs):
                pr = ce.predict(pairs, apply_softmax=True, show_progress_bar=False, batch_size=32)
                return [(float(p[idx["entailment"]]), float(p[idx["neutral"]]), float(p[idx["contradiction"]])) for p in pr]

            self._predict = pred
        self.cache: dict[tuple[str, str], tuple] = {}

    def predict(self, pairs, raw_hypothesis: bool = False):
        if not pairs:
            return []
        inputs = [(p, h if raw_hypothesis else self.core_claim(h)) for p, h in pairs]
        missing = [x for x in dict.fromkeys(inputs) if x not in self.cache]
        if missing:
            for x, pr in zip(missing, self._predict(missing)):
                self.cache[x] = pr
        return [self.cache[x] for x in inputs]


# --------------------------------------------------------------------------- VI adaptation


def _patch_vietnamese() -> None:
    """Merge the VI lexicon into SymAI's common lexicon (via a merged copy under OUT) and make the
    negation-scope tokenizer Unicode-aware. SymAI's files are not touched."""
    from symai.neural import embed as emb
    from symai.neural import nli as nli_mod
    from symai.profiles import loader
    from symai.symbolic import features as feat

    common = yaml.safe_load(loader.COMMON_LEXICON.read_text(encoding="utf-8"))
    vi = yaml.safe_load((Path(__file__).resolve().parents[1] / "symai_vi_lexicon.yaml").read_text(encoding="utf-8"))
    for k, v in vi.items():
        if isinstance(v, dict) and isinstance(common.get(k), dict):
            for kk, vv in v.items():
                if isinstance(vv, list) and isinstance(common[k].get(kk), list):
                    common[k][kk] = common[k][kk] + [x for x in vv if x not in common[k][kk]]
                else:
                    common[k][kk] = vv
        elif isinstance(v, list):
            common[k] = common.get(k, []) + [x for x in v if x not in common.get(k, [])]
    merged = OUT / "symai_lexicon_common_vi.yaml"
    merged.parent.mkdir(parents=True, exist_ok=True)
    merged.write_text(yaml.safe_dump(common, allow_unicode=True), encoding="utf-8")
    loader.COMMON_LEXICON = merged
    loader.load_profile.cache_clear()
    feat.extractor_for.cache_clear()

    word = re.compile(r"[^\W\d_][\w'-]+")
    orig_scope = feat.FeatureExtractor.scope

    def scope(self, low: str):
        masked = self._mask(low, self.neg_excl.find(low))
        cue_spans = [m.span() for m in self.neg.regex.finditer(masked)] if self.neg.regex else []
        skip = feat._STOP | {"no", "not", "never", "none", "nil", "any", "anything", "really", "very", "much", "at", "all"}
        terms, negated = [], []
        for m in word.finditer(masked):
            w = m.group(0)
            if w in skip or len(w) < 2 or "'" in w:
                continue
            s = feat.stem(w)
            if s not in terms:
                terms.append(s)
            for a, b in cue_spans:
                gap = masked[b:m.start()]
                if 0 <= m.start() - b and not feat._CLAUSE_BREAK.search(gap) and len(re.findall(r"\w+", gap)) < feat._NEG_WINDOW:
                    if s not in negated and not self.neg.find(w):
                        negated.append(s)
                    break
        return terms, negated

    feat.FeatureExtractor.scope = scope
    feat.FeatureExtractor._orig_scope = orig_scope

    def tokens(text: str) -> list[str]:
        return [feat.stem(w) for w in re.findall(r"[^\W_][\w'-]*", text.lower()) if w not in emb._STOP]

    emb.tokens = tokens
    nli_mod.tokens = tokens


# ------------------------------------------------------------------------------------- notes


def to_note(v: dict):
    from symai.datagen.corpus import note_from_dict

    roles = v.get("roles") or {}
    topics = v.get("topics") or []
    sids = v.get("sentence_ids") or [f"s{j + 1}" for j in range(len(v["bullets"]))]
    turns = []
    for k, (spk, _, text) in enumerate(v["transcript"]):
        turns.append({"id": f"t{k + 1}", "speaker": v["speakers"].get(spk, spk), "role": roles.get(spk, "member"),
                      "text": text, **({"topic": topics[k]} if k < len(topics) and topics[k] else {})})
    summary = [{"id": sid, "text": b["text"], "source_turns": [f"t{c + 1}" for c in b["cites"]]}
               for sid, b in zip(sids, v["bullets"])]
    d = {"id": v["id"].replace("#", "-"), "profile": "minutes", "title": v.get("title", v["id"]),
         "creator": "Almond", "date": "2026-11-11", "turns": turns, "summary": summary}
    return note_from_dict(d), sids


def run(system: str, bench: dict, wanted: set[str], cache_dir) -> list[dict]:
    from symai.config import Settings
    from symai.neural.llm import LLM
    from symai.pipeline import Backends, Pipeline

    from vb.models import DEBERTA_NLI, MINILM, MNLI

    settings = Settings(llm="off", backend="neural")
    if system == "symai_vi":
        _patch_vietnamese()
        backends = Backends(OllamaEmbed(), CachedNLI(MNLI, multilingual=True), LLM(settings), "neural")
    else:
        backends = Backends(CachedST(MINILM), CachedNLI(DEBERTA_NLI), LLM(settings), "neural")
    pipe = Pipeline(settings=settings, store=None, backends=backends, alignment="hybrid")
    rows = []
    for vi, v in enumerate(bench["meetings"]):
        metas = v["bullets_meta"]
        if not any(m in wanted for m in metas if m):
            continue
        note, sids = to_note(v)
        t0 = time.perf_counter()
        try:
            res = pipe.check(note, persist=False)
            err = None
        except Exception as e:  # noqa: BLE001 - a governance error must not sink the run
            res, err = None, repr(e)[:300]
        el = (time.perf_counter() - t0) * 1000
        for sid, iid in zip(sids, metas):
            if not iid or iid not in wanted:
                continue
            if res is None:
                rows.append({"item": iid, "label": "unsupported", "score": 0.5, "ms": el / len(sids), "error": err})
                continue
            flags = [f for f in res.flags if f.target == sid and _is_fact(f.rule_id)]
            pe = max((a.p_entail for a in res.alignments if a.sentence_id == sid), default=0.0)
            if flags:
                gated = any(f.action.value in ("hold", "block") for f in flags)
                score = 0.75 + 0.25 * gated
                label = "contradicted" if any(f.rule_id in CONTRA for f in flags) else "unsupported"
            else:
                score, label = 0.5 * (1 - pe), "supported"
            rows.append({"item": iid, "label": label, "score": round(score, 4), "ms": el / len(sids),
                         "variant": v["id"], "clean_variant": v["id"].endswith("#clean"),
                         "rules": sorted({f.rule_id for f in flags}),
                         "gated": any(f.action.value in ("hold", "block") for f in flags)})
        if vi % 20 == 0:
            print(f"  [{vi}/{len(bench['meetings'])}] {v['id']} {el:.0f} ms", flush=True)
    return rows
