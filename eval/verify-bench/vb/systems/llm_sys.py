"""gemma4:12b-based systems: QA-based (QAGS-style), plain judge, chain-of-verification and
SelfCheck-style sampling consistency. All prompts get the same evidence as the other systems:
the cited segments +- 1 neighbour, rendered "[n] SPEAKER: text"."""

from __future__ import annotations

import re
from collections import Counter

from vb.llm import Ollama
from vb.mutate import VI_CHARS, fold


def _ev(it: dict) -> str:
    return "\n".join(f"[{k}] {spk}: {txt}" for k, spk, txt in it["evidence"])


def _llm(cache_dir) -> Ollama:
    return Ollama(cache_dir / "gemma.sqlite")


class Translator:
    def __init__(self, cache_dir):
        self.llm = _llm(cache_dir)

    @staticmethod
    def is_vi(text: str) -> bool:
        return sum(c in VI_CHARS for c in text.lower()) >= 1

    def __call__(self, text: str) -> tuple[str, float]:
        if not self.is_vi(text):
            return text, 0.0
        j = self.llm.json("Translate the user's text from Vietnamese (possibly mixed with English) into English. "
                          "Translate faithfully: keep every number, name, date, negation and uncertainty word. "
                          'JSON only: {"en": "..."}', text,
                          {"type": "object", "properties": {"en": {"type": "string"}}, "required": ["en"]})
        return ((j or {}).get("en") or text), self.llm.last_ms


# ------------------------------------------------------------------------------------- labels

LABEL_SCHEMA = {"type": "object", "properties": {
    "reason": {"type": "string"},
    "label": {"type": "string", "enum": ["supported", "unsupported", "contradicted"]},
    "confidence": {"type": "number"}}, "required": ["reason", "label", "confidence"]}


def _score(label: str, conf: float) -> float:
    try:
        c = float(conf if conf is not None else 0.7)
    except (TypeError, ValueError):
        c = 0.7
    c = min(1.0, max(0.0, c / 100 if c > 1.0 else c))
    return {"supported": 0.25 * (1 - c), "unsupported": 0.5 + 0.25 * c, "contradicted": 0.75 + 0.25 * c}.get(label, 0.5)


JUDGE_SYSTEM = """You are a strict fact-checker for meeting notes. You get a CLAIM from an AI-written \
meeting summary and EVIDENCE: the transcript segments it cites plus neighbours ("[n] SPEAKER: text"; \
automatic speech recognition, may contain errors). Claim and evidence may be Vietnamese, English or mixed.
- "supported": every detail (numbers, names, who said it, dates/times, certainty, scope such as all/some, \
negation) is stated or directly implied by the evidence.
- "contradicted": some detail conflicts with the evidence.
- "unsupported": the evidence does not state it (or not with that certainty / scope).
Give a short reason, the label and your confidence in [0, 1]. JSON only."""


def judge(items, cache_dir):
    llm = _llm(cache_dir)
    rows = []
    for it in items:
        j = llm.json(JUDGE_SYSTEM, f"CLAIM: {it['text']}\n\nEVIDENCE:\n{_ev(it)}", LABEL_SCHEMA) or {}
        lab = j.get("label", "unsupported")
        rows.append({"item": it["item"], "label": lab, "score": _score(lab, j.get("confidence")), "ms": llm.last_ms})
    return rows


# ------------------------------------------------------------------------------ chain of verification

PLAN_SYSTEM = """You verify a CLAIM from a meeting summary. Write 2-4 short verification questions that \
together check every detail of the claim (who, what, numbers, when, how certain, how many / all or some, \
whether it happened or not). Ask in the claim's language. JSON only: {"questions": ["..."]}"""
PLAN_SCHEMA = {"type": "object", "properties": {"questions": {"type": "array", "items": {"type": "string"}}},
               "required": ["questions"]}
ANSWER_SYSTEM = """Answer each QUESTION using only the TRANSCRIPT excerpt ("[n] SPEAKER: text", automatic \
speech recognition). Quote or closely paraphrase what was said, including who said it and any uncertainty. \
If the excerpt does not say, answer "not stated". JSON only: {"answers": ["..."]} in question order."""
ANSWER_SCHEMA = {"type": "object", "properties": {"answers": {"type": "array", "items": {"type": "string"}}},
                 "required": ["answers"]}
FINAL_SYSTEM = """You are a strict fact-checker for meeting notes. You get a CLAIM, verification \
questions with answers taken independently from the transcript, and the EVIDENCE itself. Using the answers \
first, decide: "supported" (every detail confirmed), "contradicted" (an answer conflicts with a detail), or \
"unsupported" (some detail not stated, or stated less certainly / for a narrower scope). Give a short \
reason, the label and your confidence in [0, 1]. JSON only."""


def cove(items, cache_dir):
    llm = _llm(cache_dir)
    rows = []
    for it in items:
        ms = 0.0
        plan = llm.json(PLAN_SYSTEM, f"CLAIM: {it['text']}", PLAN_SCHEMA) or {}
        ms += llm.last_ms
        qs = [q for q in plan.get("questions", []) if isinstance(q, str) and q.strip()][:4] or [it["text"]]
        # factored-joint: answers are produced without seeing the claim
        ans = llm.json(ANSWER_SYSTEM, "TRANSCRIPT:\n" + _ev(it) + "\n\nQUESTIONS:\n" +
                       "\n".join(f"{k + 1}. {q}" for k, q in enumerate(qs)), ANSWER_SCHEMA) or {}
        ms += llm.last_ms
        a = ans.get("answers", [])
        qa = "\n".join(f"Q{k + 1}: {q}\nA{k + 1}: {a[k] if k < len(a) else 'not stated'}" for k, q in enumerate(qs))
        j = llm.json(FINAL_SYSTEM, f"CLAIM: {it['text']}\n\nVERIFICATION:\n{qa}\n\nEVIDENCE:\n{_ev(it)}", LABEL_SCHEMA) or {}
        ms += llm.last_ms
        lab = j.get("label", "unsupported")
        rows.append({"item": it["item"], "label": lab, "score": _score(lab, j.get("confidence")), "ms": ms,
                     "calls": 3})
    return rows


# --------------------------------------------------------------------------------- QA-based (QAGS)

QG_SYSTEM = """You write questions for checking a CLAIM from a meeting summary (QAGS method). Write 3 \
short questions whose answers are spans of the claim, covering its key details (names, numbers, dates, \
actions, certainty, scope). Give each question with its answer taken from the claim. Use the claim's \
language. JSON only: {"qa": [{"question": "...", "answer": "..."}]}"""
QG_SCHEMA = {"type": "object", "properties": {"qa": {"type": "array", "items": {"type": "object", "properties": {
    "question": {"type": "string"}, "answer": {"type": "string"}}, "required": ["question", "answer"]}}},
    "required": ["qa"]}
QA_SYSTEM = """Answer each QUESTION with a short span taken from the TRANSCRIPT excerpt ("[n] SPEAKER: \
text", automatic speech recognition), in the transcript's words. If the excerpt does not contain the \
answer, answer exactly "unanswerable". JSON only: {"answers": ["..."]} in question order."""

_QA_STOP = set("the a an of to and in on is was are be by for at it that this with as cua la va cac nhung mot duoc".split())


def _f1(a: str, b: str) -> float:
    ta = [t for t in re.findall(r"[a-z0-9]+", fold(a)) if t not in _QA_STOP]
    tb = [t for t in re.findall(r"[a-z0-9]+", fold(b)) if t not in _QA_STOP]
    if not ta or not tb:
        return float(ta == tb)
    common = sum((Counter(ta) & Counter(tb)).values())
    if not common:
        return 0.0
    p, r = common / len(tb), common / len(ta)
    return 2 * p * r / (p + r)


def qa(items, cache_dir):
    llm = _llm(cache_dir)
    rows = []
    for it in items:
        ms = 0.0
        g = llm.json(QG_SYSTEM, f"CLAIM: {it['text']}", QG_SCHEMA) or {}
        ms += llm.last_ms
        pairs = [p for p in g.get("qa", []) if p.get("question") and p.get("answer")][:4]
        if not pairs:
            rows.append({"item": it["item"], "label": "unsupported", "score": 0.5, "ms": ms, "calls": 1})
            continue
        ans = llm.json(QA_SYSTEM, "TRANSCRIPT:\n" + _ev(it) + "\n\nQUESTIONS:\n" +
                       "\n".join(f"{k + 1}. {p['question']}" for k, p in enumerate(pairs)), ANSWER_SCHEMA) or {}
        ms += llm.last_ms
        got = ans.get("answers", [])
        f1s, unans = [], 0
        for k, p in enumerate(pairs):
            a = got[k] if k < len(got) and isinstance(got[k], str) else "unanswerable"
            if "unanswerable" in a.lower() or "not stated" in a.lower():
                unans += 1
                f1s.append(0.0)
            else:
                f1s.append(_f1(p["answer"], a))
        mean = sum(f1s) / len(f1s)
        label = "supported" if mean >= 0.5 and unans == 0 else ("unsupported" if unans else "contradicted")
        rows.append({"item": it["item"], "label": label, "score": round(1 - mean, 4), "ms": ms, "calls": 2,
                     "f1": [round(x, 2) for x in f1s]})
    return rows


# --------------------------------------------------------------------- SelfCheck (prompt variant)

SAMPLE_SYSTEM = """Summarise the meeting TRANSCRIPT excerpt ("[n] SPEAKER: text", automatic speech \
recognition) in 3-5 sentences, in the excerpt's main language. Include who said what, numbers, dates and \
how certain people were."""
SC_SYSTEM = """Context: a PASSAGE written about part of a meeting. Is the SENTENCE supported by the \
PASSAGE? Answer "yes" or "no" (JSON only: {"answer": "yes"})."""
SC_SCHEMA = {"type": "object", "properties": {"answer": {"type": "string", "enum": ["yes", "no"]}}, "required": ["answer"]}
N_SAMPLES = 4


def selfcheck(items, cache_dir):
    """SelfCheckGPT-Prompt adapted to grounded summaries: N stochastic re-summaries of the cited
    evidence (temperature 0.8, fixed seeds), then a yes/no support check of the claim against each.
    Score = fraction of samples that do not support the claim."""
    llm = _llm(cache_dir)
    rows = []
    for it in items:
        ms = 0.0
        no = 0
        for s in range(N_SAMPLES):
            passage = llm.chat(SAMPLE_SYSTEM, "TRANSCRIPT:\n" + _ev(it), temperature=0.8, seed=1000 + s,
                               num_predict=400)
            ms += llm.last_ms
            j = llm.json(SC_SYSTEM, f"PASSAGE: {passage}\n\nSENTENCE: {it['text']}", SC_SCHEMA) or {}
            ms += llm.last_ms
            no += j.get("answer", "no") != "yes"
        score = no / N_SAMPLES
        rows.append({"item": it["item"], "label": "supported" if score < 0.5 else "unsupported", "score": score,
                     "ms": ms, "calls": 2 * N_SAMPLES})
    return rows
