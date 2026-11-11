"""Step 4: metrics. Reads items + per-system outputs of both datasets and writes *aggregates only*
to OUT/metrics.json and OUT/tables.md (safe to commit / paste: no item text).

    uv run python -m vb.metrics
"""

from __future__ import annotations

import json
import random
import statistics
from collections import defaultdict
from pathlib import Path

from vb.build import read_jsonl
from vb.paths import OUT, PRIVATE

SYSTEMS = ["almond_symbolic", "almond_symbolic_alarm", "almond_full", "almond_full_alarm", "symai", "symai_vi", "summac", "minicheck", "minicheck_mt", "qa",
           "judge", "cove", "selfcheck"]
NAMES = {
    "almond_symbolic": "Almond rules only (≠ supported)", "almond_full": "Almond current (rules + verify.nli, ≠ supported)",
    "almond_symbolic_alarm": "Almond rules only (red/amber only)", "almond_full_alarm": "Almond current (red/amber only)",
    "symai": "SymAI (MiniLM+DeBERTa, EN lexicon)", "symai_vi": "SymAI-VI (multilingual + VI lexicon)",
    "summac": "SummaC-ZS (mDeBERTa XNLI)", "minicheck": "MiniCheck-RoBERTa-L", "minicheck_mt": "MiniCheck + gemma MT",
    "qa": "QA-based (QAGS-style)", "judge": "LLM judge (plain)", "cove": "LLM judge (CoVe)",
    "selfcheck": "SelfCheck-prompt (4 samples)",
}
TYPES = ["number", "entity", "negation", "hedge_drop", "hedge_add", "quantifier", "temporal", "speaker", "fabricate",
         "overgeneralise", "cluster_insert"]
B = 1000


def load(ds: str):
    base = OUT if ds == "synthetic" else PRIVATE
    items = read_jsonl(base / f"{ds}_items.jsonl") if (base / f"{ds}_items.jsonl").exists() else []
    res = {}
    for p in (base / "results").glob("*.jsonl"):
        rows = read_jsonl(p)
        meta_p = p.with_name(p.name + ".meta.json")
        if meta_p.exists():  # Rust runner: per-claim latency from the run's totals
            meta = json.loads(meta_p.read_text(encoding="utf-8"))
            per_judge = meta["judge_ms"] / meta["judge_calls"] if meta.get("judge_calls") else 0.0
            per_sym = 1000 * meta["symbolic_s"] / max(1, meta["scored"])
            for r in rows:
                r["raw_label"] = r["label"]
                r["label"] = {"unverifiable": "unsupported", "needs_review": "unsupported"}.get(r["label"], r["label"])
                r["ms"] = per_sym + (per_judge if "verify.nli" in r.get("verifiers", {}) else 0.0)
        res[p.stem] = {r["item"]: r for r in rows}
        if meta_p.exists():  # "alarm" view: only red/amber (contradicted, needs_review) counts as flagged
            alarm = {"contradicted": "contradicted", "needs_review": "unsupported"}
            res[p.stem + "_alarm"] = {r["item"]: {**r, "label": alarm.get(r["raw_label"], "supported")} for r in rows}
    return items, res


# ------------------------------------------------------------------------------------- hybrids


def _row(label, score, ms):
    return {"label": label, "score": score, "ms": ms}


def hybrid_rows(items, res) -> dict[str, dict]:
    """Post-hoc combinations of per-item outputs (latency = sum of the components)."""
    out = defaultdict(dict)
    for it in items:
        i = it["item"]
        r = {s: res[s][i] for s in res if i in res[s]}
        sym, jud, sc, full = r.get("almond_symbolic"), r.get("judge"), r.get("summac"), r.get("almond_full")
        symvi = r.get("symai_vi")
        # H1: Almond rules (contradicted = decisive) + plain judge decides the rest; judge abstains -> rules
        if sym and jud:
            if sym["label"] == "contradicted":
                out["hybrid_rules_judge"][i] = _row("contradicted", max(sym["score"], jud["score"]), sym["ms"] + jud["ms"])
            else:
                out["hybrid_rules_judge"][i] = _row(jud["label"], jud["score"], sym["ms"] + jud["ms"])
        # H2: H1 + SummaC/NLI veto: a strong NLI entailment downgrades a judge-only 'unsupported' to supported
        if sym and jud and sc:
            if sym["label"] == "contradicted":
                lab = "contradicted"
            elif jud["label"] == "unsupported" and sc["label"] == "supported":
                lab = "supported"
            else:
                lab = jud["label"]
            out["hybrid_rules_judge_nli"][i] = _row(lab, 0.5 * jud["score"] + 0.5 * sc["score"],
                                                     sym["ms"] + jud["ms"] + sc["ms"])
        # H3: Almond current + SymAI-VI symbolic distortion rules (either flags)
        if full and symvi:
            flag = full["label"] != "supported" or symvi["label"] != "supported"
            lab = "contradicted" if "contradicted" in (full["label"], symvi["label"]) else ("unsupported" if flag else "supported")
            out["hybrid_almond_symai"][i] = _row(lab, max(full["score"], symvi["score"]), full["ms"] + symvi["ms"])
    return out


NAMES.update({"hybrid_rules_judge": "Hybrid: Almond rules + judge", "hybrid_rules_judge_nli": "Hybrid: rules + judge + NLI veto",
              "hybrid_almond_symai": "Hybrid: Almond current OR SymAI-VI"})


# ------------------------------------------------------------------------------------- metrics


def flagged(r) -> bool:
    return r["label"] != "supported"


def auroc(pos: list[float], neg: list[float]) -> float | None:
    if not pos or not neg:
        return None
    wins = 0.0
    for p in pos:
        for n in neg:
            wins += 1.0 if p > n else 0.5 if p == n else 0.0
    return wins / (len(pos) * len(neg))


def stats(pairs: list[tuple[dict, dict]]) -> dict:
    """pairs: (item, row). Recall / precision / false-alarm / AUROC with bootstrap 95% CIs."""
    pos = [(it, r) for it, r in pairs if it["gold"]]
    neg = [(it, r) for it, r in pairs if not it["gold"]]

    def point(P, N):
        tp = sum(flagged(r) for _, r in P)
        fp = sum(flagged(r) for _, r in N)
        rec = tp / len(P) if P else None
        far = fp / len(N) if N else None
        prec = tp / (tp + fp) if tp + fp else None
        auc = auroc([r["score"] for _, r in P], [r["score"] for _, r in N]) if len(P) * len(N) <= 400_000 else None
        return rec, prec, far, auc

    rec, prec, far, auc = point(pos, neg)
    rng = random.Random(7)
    boots = defaultdict(list)
    if pos and neg:
        for _ in range(B):
            P = [rng.choice(pos) for _ in pos]
            N = [rng.choice(neg) for _ in neg]
            tp = sum(flagged(r) for _, r in P)
            fp = sum(flagged(r) for _, r in N)
            boots["recall"].append(tp / len(P))
            boots["far"].append(fp / len(N))
            if tp + fp:
                boots["precision"].append(tp / (tp + fp))
    elif pos:
        for _ in range(B):
            P = [rng.choice(pos) for _ in pos]
            boots["recall"].append(sum(flagged(r) for _, r in P) / len(P))

    def ci(k):
        xs = sorted(boots.get(k, []))
        return [round(xs[int(0.025 * (len(xs) - 1))], 3), round(xs[int(0.975 * (len(xs) - 1))], 3)] if xs else None

    ms = [r["ms"] for _, r in pairs if r.get("ms") is not None]
    return {"n_pos": len(pos), "n_clean": len(neg), "recall": _r(rec), "recall_ci": ci("recall"),
            "precision": _r(prec), "precision_ci": ci("precision"), "false_alarm": _r(far), "false_alarm_ci": ci("far"),
            "auroc": _r(auc), "ms_median": _r(statistics.median(ms), 1) if ms else None,
            "ms_mean": _r(statistics.mean(ms), 1) if ms else None}


def _r(x, d=3):
    return None if x is None else round(x, d)


def main() -> None:
    report = {"datasets": {}}
    for ds in ("synthetic", "personal"):
        items, res = load(ds)
        if not items:
            continue
        res.update(hybrid_rows(items, res))
        dsrep = {"counts": defaultdict(int), "systems": {}}
        for it in items:
            dsrep["counts"][f"{it['subset']}|{it['lang']}|{it['type']}"] += 1
        for s, rows in sorted(res.items()):
            pairs = [(it, rows[it["item"]]) for it in items if it["item"] in rows]
            if not pairs:
                continue
            sysrep = {"coverage": f"{len(pairs)}/{len(items)}"}
            groups = defaultdict(list)
            for it, r in pairs:
                groups["all"].append((it, r))
                groups[f"lang={it['lang']}"].append((it, r))
                groups[f"subset={it['subset']}"].append((it, r))
                groups[f"subset={it['subset']}|lang={it['lang']}"].append((it, r))
            sysrep["groups"] = {g: stats(v) for g, v in sorted(groups.items())}
            by_type = defaultdict(list)
            for it, r in pairs:
                if it["gold"]:
                    by_type[f"{it['type']}|{it['lang']}"].append(flagged(r))
                    by_type[f"{it['type']}|all"].append(flagged(r))
            sysrep["recall_by_type"] = {k: {"n": len(v), "recall": round(sum(v) / len(v), 3)} for k, v in sorted(by_type.items())}
            # cost of verifying one whole summary: sum over the clean bullets of each meeting
            per_meeting = defaultdict(float)
            for it, r in pairs:
                if not it["gold"] and r.get("ms") is not None:
                    per_meeting[it["meeting"]] += r["ms"]
            sysrep["ms_per_meeting_mean"] = _r(statistics.mean(per_meeting.values()), 0) if per_meeting else None
            # contradiction vs unsupported split among caught positives
            caught = [r["label"] for it, r in pairs if it["gold"] and flagged(r)]
            sysrep["caught_as_contradicted"] = _r(caught.count("contradicted") / len(caught)) if caught else None
            dsrep["systems"][s] = sysrep
        dsrep["counts"] = dict(sorted(dsrep["counts"].items()))
        report["datasets"][ds] = dsrep
    (OUT / "metrics.json").write_text(json.dumps(report, indent=1, ensure_ascii=False), encoding="utf-8")
    (OUT / "tables.md").write_text(tables(report), encoding="utf-8")
    print((OUT / "tables.md").read_text(encoding="utf-8"))


def _f(x, pct=True):
    if x is None:
        return "–"
    return f"{100 * x:.0f}%" if pct else f"{x:.2f}"


def _ci(c):
    return f" [{100 * c[0]:.0f}–{100 * c[1]:.0f}]" if c else ""


def tables(rep: dict) -> str:
    out = []
    order = SYSTEMS + ["hybrid_rules_judge", "hybrid_rules_judge_nli", "hybrid_almond_symai"]
    for ds, d in rep["datasets"].items():
        out.append(f"\n## {ds}\n")
        for g in ["all", "lang=vi", "lang=en"] + sorted({k for s in d["systems"].values() for k in s["groups"] if k.startswith("subset=") and "|" not in k}):
            out.append(f"\n### {ds} · {g}\n")
            out.append("| system | n pos / clean | recall [95% CI] | precision | false alarms [95% CI] | AUROC | ms/claim (median) | s/meeting |")
            out.append("|---|---|---|---|---|---|---|---|")
            for s in order:
                if s not in d["systems"] or g not in d["systems"][s]["groups"]:
                    continue
                x = d["systems"][s]["groups"][g]
                out.append(f"| {NAMES.get(s, s)} | {x['n_pos']} / {x['n_clean']} | {_f(x['recall'])}{_ci(x['recall_ci'])} | "
                           f"{_f(x['precision'])} | {_f(x['false_alarm'])}{_ci(x['false_alarm_ci'])} | {_f(x['auroc'], False)} | "
                           f"{x['ms_median'] if x['ms_median'] is not None else '–'} | "
                           f"{round(d['systems'][s]['ms_per_meeting_mean'] / 1000, 1) if d['systems'][s]['ms_per_meeting_mean'] else '–'} |")
        out.append(f"\n### {ds} · recall by error type (all languages; n in header)\n")
        types = [t for t in TYPES if any(f"{t}|all" in s["recall_by_type"] for s in d["systems"].values())]
        ns = {t: max((s["recall_by_type"].get(f"{t}|all", {}).get("n", 0) for s in d["systems"].values()), default=0) for t in types}
        out.append("| system | " + " | ".join(f"{t} ({ns[t]})" for t in types) + " |")
        out.append("|---|" + "---|" * len(types))
        for s in order:
            if s not in d["systems"]:
                continue
            rb = d["systems"][s]["recall_by_type"]
            out.append(f"| {NAMES.get(s, s)} | " + " | ".join(_f(rb.get(f'{t}|all', {}).get('recall')) for t in types) + " |")
        for lang in ("vi", "en"):
            types_l = [t for t in TYPES if any(f"{t}|{lang}" in s["recall_by_type"] for s in d["systems"].values())]
            if not types_l:
                continue
            ns = {t: max((s["recall_by_type"].get(f"{t}|{lang}", {}).get("n", 0) for s in d["systems"].values()), default=0) for t in types_l}
            out.append(f"\n### {ds} · recall by error type, lang={lang}\n")
            out.append("| system | " + " | ".join(f"{t} ({ns[t]})" for t in types_l) + " |")
            out.append("|---|" + "---|" * len(types_l))
            for s in order:
                if s not in d["systems"]:
                    continue
                rb = d["systems"][s]["recall_by_type"]
                out.append(f"| {NAMES.get(s, s)} | " + " | ".join(_f(rb.get(f'{t}|{lang}', {}).get('recall')) for t in types_l) + " |")
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    main()
