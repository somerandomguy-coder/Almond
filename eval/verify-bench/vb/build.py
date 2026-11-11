"""Step 2: build the benchmark.

    uv run python -m vb.build summarize     # personal: gemma4:12b cited bullets per transcript window (GPU)
    uv run python -m vb.build filter        # personal: keep only bullets that pass every faithfulness check
    uv run python -m vb.build assemble      # all subsets: mutate, write variants + items

Subsets
- personal: the user's recordings (PRIVATE only). Clean bullets = gemma4:12b bullets that pass a
  conservative faithfulness filter (see `filter_bullets`); positives = deterministic mutations.
- almond: crates/almond-verify eval fixtures (dev + held-out). Clean = their "ok" bullets; positives =
  their hand-written hallucinations + our mutations of their ok bullets.
- symai: SymAI's 8 English council-minutes notes, mutated by SymAI's own `seed_cases` (exact
  comparability with SymAI's report).

Each *variant* is one summary: the clean summary, or the clean summary with exactly one positive
substituted / inserted (SymAI's protocol). Variants use the `eval/fixtures.json` meeting format
so `crates/almond-verify/examples/bench` reads them directly; `bullets_meta[i]` is the item id of
a scored bullet (or null for context bullets).
"""

from __future__ import annotations

import json
import random
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

from vb import mutate as M
from vb.llm import Ollama
from vb.paths import ALMOND_FIXTURES, CHECK_MODEL, OUT, PRIVATE, ensure

SEED = 20261112
WINDOW_CHARS = 2400
EXTRACTIVE_MIN = 0.6  # share of a bullet's content tokens that must occur in its cited segments
RADIUS = 1  # evidence = cited segments +- RADIUS neighbours (same as verify.nli)

# ------------------------------------------------------------------------------------------ io


def load_json(p: Path):
    return json.loads(p.read_text(encoding="utf-8"))


def dump_json(p: Path, obj) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(obj, ensure_ascii=False, indent=1), encoding="utf-8")


def write_jsonl(p: Path, rows) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    with p.open("w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")


def read_jsonl(p: Path) -> list[dict]:
    return [json.loads(l) for l in p.read_text(encoding="utf-8").splitlines() if l.strip()]


# ------------------------------------------------------------------------------------- personal


def personal_meetings() -> list[dict]:
    out = []
    for p in sorted((PRIVATE / "transcripts").glob("rec*.json")):
        t = load_json(p)
        segs = [s for s in t["segments"] if s["text"].strip()]
        speakers = {s["id"]: s["id"] for s in t["speakers"]}
        dur = max((s["t1_ms"] for s in segs), default=0) / 60000
        out.append({
            "id": p.stem, "title": p.stem, "start": "2026-11-11T19:00:00+07:00", "tz_offset_min": 420,
            "duration_min": max(1, round(dur)), "attendees": [], "speakers": speakers,
            "transcript": [[s.get("speaker") or "S1", s.get("lang") or "vi", s["text"].strip()] for s in segs],
        })
    return out


def windows(tr: list, max_chars: int = WINDOW_CHARS) -> list[list[int]]:
    out, cur, n = [], [], 0
    for i, (_, _, text) in enumerate(tr):
        if cur and n + len(text) > max_chars:
            out.append(cur)
            cur, n = [], 0
        cur.append(i)
        n += len(text)
    if cur:
        out.append(cur)
    return out


SUM_SYSTEM = """You write bullet points for Almond, a meeting-notes app. You get an EXCERPT of an \
automatic transcript: numbered segments "[n] SPEAKER: text". Write {k} summary bullets in {lang}.
Rules:
- One fact per bullet, taken only from the excerpt. Stay close to the speakers' own words \
(near-extractive), but write a clean, complete sentence.
- Keep numbers, names, dates, times and any uncertainty words (maybe, có lẽ, chắc, khoảng) exactly as said.
- When a bullet reports an opinion, proposal, question or plan, attribute it to the speaker label \
(e.g. "S2 nói rằng ..." / "S2 suggested ...").
- Skip greetings, filler and garbled or unclear speech. If the excerpt has no clear content, return no bullets.
- "cites": the 1-3 segment numbers that state the bullet.
Output JSON only: {{"bullets": [{{"text": "...", "cites": [n]}}]}}"""

SUM_SCHEMA = {"type": "object", "properties": {"bullets": {"type": "array", "items": {
    "type": "object", "properties": {"text": {"type": "string"}, "cites": {"type": "array", "items": {"type": "integer"}}},
    "required": ["text", "cites"]}}}, "required": ["bullets"]}


def render(tr, idxs) -> str:
    return "\n".join(f"[{i}] {tr[i][0]}: {tr[i][2]}" for i in idxs)


def summarize() -> None:
    llm = Ollama(PRIVATE / "cache" / "gemma.sqlite")
    out = {}
    for m in personal_meetings():
        tr = m["transcript"]
        bullets = []
        # two passes (large + small windows) for more, finer-grained bullets; duplicates are
        # removed by the filter
        for w in windows(tr) + windows(tr, max_chars=1100):
            langs = Counter(tr[i][1] for i in w)
            lang = "Vietnamese" if langs.get("vi", 0) >= langs.get("en", 0) else "English"
            chars = sum(len(tr[i][2]) for i in w)
            k = "2-4" if chars > 800 else "1-2"
            j = llm.json(SUM_SYSTEM.format(k=k, lang=lang), "EXCERPT:\n" + render(tr, w), SUM_SCHEMA)
            for b in (j or {}).get("bullets", []):
                bullets.append({"text": b.get("text", "").strip(), "cites": b.get("cites", []), "window": w})
        out[m["id"]] = bullets
        print(m["id"], len(tr), "segments ->", len(bullets), "bullets", f"(llm calls {llm.calls})", flush=True)
    dump_json(PRIVATE / "summaries_raw.json", out)


# ---------------------------------------------------------------------------- faithfulness filter

_STOP = set("""và là của có cho với các những một được này đó thì mà để không đã sẽ đang rằng nói ở trong
ra vào lại cũng như khi về từ theo nên vì bị hay hoặc nhưng nếu đến người ai gì nào s1 s2 s3 s4 s5 s6
the a an of to and or in on at for with is was are were be been it that this as by from but he she they i
you we said says say suggested noted mentioned asked explained stated that will would can could should do
does did has have had not no so if about also there their them his her its our your my me""".split())


_VI_DIGIT = {"khong": 0, "mot": 1, "hai": 2, "ba": 3, "bon": 4, "tu": 4, "nam": 5, "lam": 5, "sau": 6, "bay": 7,
             "tam": 8, "chin": 9}
_EN_DIGIT = {"zero": 0, "one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6, "seven": 7, "eight": 8,
             "nine": 9, "ten": 10, "eleven": 11, "twelve": 12, "thirteen": 13, "fourteen": 14, "fifteen": 15,
             "sixteen": 16, "seventeen": 17, "eighteen": 18, "nineteen": 19, "twenty": 20, "thirty": 30, "forty": 40,
             "fifty": 50, "sixty": 60, "seventy": 70, "eighty": 80, "ninety": 90}
_MULT = {"tram": 100, "hundred": 100, "nghin": 1000, "ngan": 1000, "thousand": 1000, "trieu": 10**6, "million": 10**6,
         "ty": 10**9, "billion": 10**9}
_AMBIG = {"mot", "ba", "tu", "nam", "sau", "bay", "tam", "khong", "lam", "one"}
_UNITISH = {"phut", "gio", "tieng", "ngay", "tuan", "thang", "nam", "nguoi", "lan", "tuoi", "cai", "con", "chuc", "muoi",
            "tram", "nghin", "ngan", "trieu", "ty", "rưỡi", "ruoi", "minutes", "hours", "days", "weeks", "years", "people"}


def spoken_numbers(folded: str) -> list[str]:
    """Digits for spelled-out numbers in ASR text ("ba muoi phut" -> 30, "twelve" -> 12), so that a
    bullet's "30 phút" can be checked against a transcript that says "BA MƯƠI PHÚT"."""
    words = re.findall(r"[a-z]+", folded)
    out, i = [], 0
    while i < len(words):
        j, total, cur, used = i, 0, 0, 0
        while j < len(words):
            w = words[j]
            if w in _VI_DIGIT or w in _EN_DIGIT:
                cur += _VI_DIGIT.get(w, _EN_DIGIT.get(w, 0)) if not (w == "nam" and used and words[j - 1] == "muoi") else 5
            elif w == "muoi":
                cur = (cur or 1) * 10
            elif w in _MULT:
                total += (cur or 1) * _MULT[w]
                cur = 0
            elif w in ("linh", "le", "and") and used:
                pass
            else:
                break
            used += 1
            j += 1
        if used:
            single_ambiguous = used == 1 and words[i] in _AMBIG
            nxt = words[j] if j < len(words) else ""
            if not single_ambiguous or nxt in _UNITISH:
                out.append(str(total + cur))
            i = j
        else:
            i += 1
    return out


def toks(s: str) -> list[str]:
    f = M.fold(s)
    return [t for t in re.findall(r"[a-z0-9]+", f) if len(t) >= 2 or t.isdigit()] + spoken_numbers(f)


def content(s: str) -> list[str]:
    return [t for t in toks(s) if t not in _STOP]


def nums(s: str) -> set[str]:
    return set(re.findall(r"\d+(?:[.,]\d+)?", s)) | set(spoken_numbers(M.fold(s)))


CHECK_MODEL2 = "llama3.1:8b"  # second independent cross-check (neither is a benchmarked judge)
# folded tokens/phrases: scope words in a bullet must occur in its source; a hedged source must
# keep a hedge or an opinion attribution in the bullet
SCOPE_WORDS = ["some", "many", "most", "all", "every", "few", "several", "always", "never", "often", "usually",
               "everyone", "nobody", "mot so", "nhieu", "tat ca", "hau het", "luon", "thuong xuyen",
               "khong bao gio", "it khi"]
SOURCE_HEDGES = ["i think", "maybe", "probably", "i guess", "might", "perhaps", "not sure", "co le", "chac", "hinh nhu",
                 "khong biet", "chac la", "i feel"]
KEEPS_HEDGE = ["think", "thinks", "believe", "believes", "feel", "feels", "felt", "guess", "maybe", "might", "may",
               "probably", "possibly", "perhaps", "suggested", "unsure", "not sure", "could", "co le", "chac", "hinh nhu",
               "co the", "nghi", "cho rang", "thay", "cam thay", "khoang", "du doan", "khong chac", "khong biet",
               "opinion", "view", "speculated", "wondered"]

CHECK_SYSTEM = """You check one bullet from a meeting summary against the transcript segments it cites. \
Answer "entailed" only if everything in the bullet (facts, numbers, names, who said it, certainty, \
time) is clearly stated in the segments. Answer "not_entailed" if anything is missing, changed, \
more certain than said, or unclear. Bullet and segments may be Vietnamese or English. JSON only."""
CHECK_SCHEMA = {"type": "object", "properties": {"label": {"type": "string", "enum": ["entailed", "not_entailed"]}},
                "required": ["label"]}


def evidence_idx(tr, cites, radius=RADIUS) -> list[int]:
    out = set()
    for c in cites:
        for k in range(c - radius, c + radius + 1):
            if 0 <= k < len(tr):
                out.add(k)
    return sorted(out)


def filter_bullets() -> None:
    raw = load_json(PRIVATE / "summaries_raw.json")
    meetings = {m["id"]: m for m in personal_meetings()}
    check = Ollama(PRIVATE / "cache" / "check.sqlite", model=CHECK_MODEL)
    check2 = Ollama(PRIVATE / "cache" / "check.sqlite", model=CHECK_MODEL2)
    kept, stats = {}, {}
    for mid, bullets in raw.items():
        tr = meetings[mid]["transcript"]
        st = Counter()
        seen: list[set] = []
        keep = []
        for b in bullets:
            st["generated"] += 1
            text, cites, win = b["text"], b["cites"], set(b["window"])
            if not text or not cites or len(cites) > 3 or not all(isinstance(c, int) and c in win for c in cites):
                st["bad_cites"] += 1
                continue
            nw = len(text.split())
            if nw < 5 or nw > 45:
                st["length"] += 1
                continue
            cited = " ".join(f"{tr[c][0]} {tr[c][2]}" for c in cites)
            ct = set(toks(cited))
            cont = content(text)
            if not cont or sum(t in ct for t in cont) / len(cont) < EXTRACTIVE_MIN:
                st["not_extractive"] += 1
                continue
            if not nums(text) <= nums(cited):
                st["number_not_in_source"] += 1
                continue
            if any(M.fold(e) not in M.fold(cited) for e in M.entities(text) if not re.fullmatch(r"S\d+", e)):
                st["entity_not_in_source"] += 1
                continue
            spk = re.findall(r"\bS\d+\b", text)
            if spk and not set(spk) <= {tr[c][0] for c in cites}:
                st["speaker_not_cited"] += 1
                continue
            fb, fc = " " + " ".join(toks(text)) + " ", " " + " ".join(toks(cited)) + " "
            if any(f" {w} " in fb and f" {w} " not in fc for w in SCOPE_WORDS):
                st["scope_word_not_in_source"] += 1
                continue
            if any(f" {w} " in fc for w in SOURCE_HEDGES) and not any(f" {w} " in fb for w in KEEPS_HEDGE):
                st["hedge_lost"] += 1
                continue
            cs = set(cont)
            if any(len(cs & s) / max(1, len(cs | s)) > 0.8 for s in seen):
                st["duplicate"] += 1
                continue
            j = check.json(CHECK_SYSTEM, f"BULLET: {text}\n\nCITED SEGMENTS:\n{render(tr, sorted(set(cites)))}",
                           CHECK_SCHEMA)
            if not j or j.get("label") != "entailed":
                st["crosscheck_rejected"] += 1
                continue
            j2 = check2.json(CHECK_SYSTEM, f"BULLET: {text}\n\nCITED SEGMENTS:\n{render(tr, sorted(set(cites)))}",
                             CHECK_SCHEMA)
            if not j2 or j2.get("label") != "entailed":
                st["crosscheck2_rejected"] += 1
                continue
            seen.append(cs)
            st["kept"] += 1
            keep.append({"text": text, "cites": sorted(set(cites)), "lang": M.lang_of(text)})
        kept[mid], stats[mid] = keep, dict(st)
        print(mid, dict(st), flush=True)
    dump_json(PRIVATE / "summaries_clean.json", kept)
    dump_json(PRIVATE / "filter_stats.json", stats)


# ----------------------------------------------------------------------------------- mutation


def meeting_ctx(m: dict, clean: list[dict]) -> dict:
    ents = []
    for b in clean:
        ents += M.entities(b["text"])
    for _, _, t in m["transcript"]:
        ents += M.entities(t)
    speakers = sorted(set(m["speakers"].values()) | set(m["speakers"].keys()))
    if any(k != v for k, v in m["speakers"].items()):  # named speakers: use the names
        speakers = sorted(set(m["speakers"].values()))
    text = " ".join([t for _, _, t in m["transcript"]] + [b["text"] for b in clean])
    lower_vocab = {w for w in re.findall(r"[\wÀ-ỹ]+", text) if w[:1].islower()}
    speakers += [a["name"] for a in m.get("attendees", [])]
    return {"entities": sorted(set(ents)), "speakers": speakers, "lower_vocab": lower_vocab,
            "folded": M.fold(" ".join(t for _, _, t in m["transcript"]))}


def make_positives(m: dict, clean: list[dict], k_per_type: int, rng: random.Random) -> list[dict]:
    """-> [{type, base (index into clean) or None, text, cites, insert_at, group}]"""
    ctx = meeting_ctx(m, clean)
    tr = m["transcript"]
    out = []
    for name, fn in M.MUTATORS.items():
        cands = []
        for i, b in enumerate(clean):
            bb = {**b, "evidence": " ".join(tr[k][2] for k in evidence_idx(tr, b["cites"]))}
            new = fn(bb, ctx, random.Random(f"{SEED}:{m['id']}:{name}:{i}"))
            if new and new.strip() != b["text"].strip() and len(new) > 3:
                cands.append((i, new))
        for i, new in rng.sample(cands, min(k_per_type, len(cands))):
            out.append({"type": name, "base": i, "text": new, "cites": clean[i]["cites"], "lang": clean[i]["lang"]})
    langs = Counter(b["lang"] for b in clean)
    if not clean:
        return out
    n_fab = max(1, k_per_type // 2)
    for f in range(n_fab):
        lang = rng.choice([l for l in ("vi", "en") if langs.get(l)])
        texts = M.fabrication(lang, ctx["folded"], random.Random(f"{SEED}:{m['id']}:fab:{f}"))
        if texts:
            at = rng.randrange(len(clean) + 1)
            nb = clean[min(at, len(clean) - 1)]
            out.append({"type": "fabricate", "base": None, "text": texts[0], "cites": nb["cites"], "lang": lang,
                        "insert_at": at})
    lang = max(langs, key=langs.get)
    texts = M.fabrication(lang, ctx["folded"], random.Random(f"{SEED}:{m['id']}:cluster"), k=2)
    if len(texts) == 2:
        at = rng.randrange(len(clean) + 1)
        nb = clean[min(at, len(clean) - 1)]
        for t in texts:
            out.append({"type": "cluster_insert", "base": None, "text": t, "cites": nb["cites"], "lang": lang,
                        "insert_at": at, "group": f"{m['id']}:cluster"})
    return out


# ------------------------------------------------------------------------------------ variants


def variant(m: dict, vid: str, bullets: list[dict], meta: list[str | None], extra: dict | None = None) -> dict:
    v = {k: m[k] for k in ("title", "start", "tz_offset_min", "duration_min", "attendees", "speakers", "transcript")}
    v.update({"id": vid, "base": m["id"],
              "bullets": [{"text": b["text"], "cites": b["cites"], "gold": "hallucinated" if b.get("pos") else "ok",
                           **({"type": b["type"]} if b.get("pos") else {})} for b in bullets],
              "bullets_meta": meta})
    if extra:
        v.update(extra)
    return v


def item_row(subset, m, vid, iid, b, typ, gold, original="") -> dict:
    tr = m["transcript"]
    ev = evidence_idx(tr, b["cites"])
    return {"item": iid, "subset": subset, "meeting": m["id"], "variant": vid, "lang": b.get("lang") or M.lang_of(b["text"]),
            "type": typ, "gold": gold, "text": b["text"], "original": original, "cites": b["cites"],
            "evidence": [[k, tr[k][0], tr[k][2]] for k in ev],
            "meeting_lang": "mixed" if len({x[1] for x in tr}) > 1 else tr[0][1],
            "src": b.get("src", "clean" if not gold else "mutation")}


def assemble_from(subset: str, meetings: list[dict], cleans: dict[str, list[dict]], k_per_type: int,
                  fixed_pos: dict[str, list[dict]] | None = None):
    rng = random.Random(SEED)
    variants, items = [], []
    for m in meetings:
        clean = cleans.get(m["id"], [])
        if not clean:
            continue
        cv = f"{m['id']}#clean"
        meta = [f"{subset}:{m['id']}:c{i}" for i in range(len(clean))]
        variants.append(variant(m, cv, clean, meta))
        for i, b in enumerate(clean):
            items.append(item_row(subset, m, cv, meta[i], b, "clean", 0))
        pos = make_positives(m, clean, k_per_type, rng) + (fixed_pos or {}).get(m["id"], [])
        groups = defaultdict(list)
        for p in pos:
            groups[p.get("group") or id(p)].append(p)
        for g, ps in enumerate(groups.values()):
            vid = f"{m['id']}#p{g}"
            bullets = [dict(b) for b in clean]
            metas: list[str | None] = [None] * len(clean)
            originals = {}
            for p in ps:
                iid = f"{subset}:{m['id']}:p{g}:{p['type']}" + (f":{ps.index(p)}" if len(ps) > 1 else "")
                pb = {"text": p["text"], "cites": p["cites"], "lang": p["lang"], "pos": True, "type": p["type"],
                      "src": p.get("src", "mutation")}
                if p.get("base") is not None:
                    bullets[p["base"]] = pb
                    metas[p["base"]] = iid
                    originals[iid] = clean[p["base"]]["text"]
                else:
                    at = min(p.get("insert_at", len(bullets)), len(bullets))
                    bullets.insert(at, pb)
                    metas.insert(at, iid)
            variants.append(variant(m, vid, bullets, metas))
            for b, iid in zip(bullets, metas):
                if iid:
                    items.append(item_row(subset, m, vid, iid, b, b["type"], 1, originals.get(iid, "")))
    return variants, items


ALMOND_TYPE = {"wrong_number": "number", "wrong_owner": "speaker", "wrong_date": "temporal",
               "wrong_meeting_time": "temporal", "invented_decision": "hedge_drop", "unsupported": "fabricate",
               "wrong_attendee": "negation"}


def almond_subset():
    meetings, cleans, fixed = [], {}, {}
    for path in ALMOND_FIXTURES:
        for m in load_json(path)["meetings"]:
            m = dict(m)
            ok = [{"text": b["text"], "cites": b["cites"], "lang": M.lang_of(b["text"])} for b in m["bullets"]
                  if b["gold"] != "hallucinated"]
            hal = []
            for i, b in enumerate(m["bullets"]):
                if b["gold"] == "hallucinated":
                    at = sum(1 for x in m["bullets"][:i] if x["gold"] != "hallucinated")
                    hal.append({"type": ALMOND_TYPE.get(b.get("type", ""), "fabricate"), "src": "handwritten:" + b.get("type", "other"), "base": None, "text": b["text"],
                                "cites": b["cites"], "lang": M.lang_of(b["text"]), "insert_at": at})
            meetings.append(m)
            cleans[m["id"]] = ok
            fixed[m["id"]] = hal
    return meetings, cleans, fixed


def symai_subset():
    from vb.symai_bridge import CONTENT_MUTATORS, TYPE_OF
    from symai.datagen.corpus import load_corpus
    from symai.datagen.mutate import seed_cases

    corpus = load_corpus("minutes")
    variants, items = [], []

    def to_meeting(note, vid):
        spk_ids = {}
        for t in note.transcript.turns:
            spk_ids.setdefault(t.speaker, f"P{len(spk_ids) + 1}")
        tid = {t.id: k for k, t in enumerate(note.transcript.turns)}
        tr = [[spk_ids[t.speaker], "en", t.text] for t in note.transcript.turns]
        return {"id": vid, "base": note.id, "title": note.metadata.title or note.id, "start": "2026-10-08T10:00:00+10:00",
                "tz_offset_min": 600, "duration_min": 60, "attendees": [],
                "speakers": {v: k for k, v in spk_ids.items()}, "transcript": tr,
                "roles": {spk_ids[t.speaker]: t.role for t in note.transcript.turns},
                "topics": [t.topic for t in note.transcript.turns]}, tid

    for note in corpus:
        m, tid = to_meeting(note, f"{note.id}#clean")
        bullets, meta = [], []
        for s in note.summary:
            bullets.append({"text": s.text, "cites": [tid[x] for x in s.source_turns if x in tid] or [0], "lang": "en"})
            meta.append(f"symai:{note.id}:{s.id}")
        m["bullets"] = [{"text": b["text"], "cites": b["cites"], "gold": "ok"} for b in bullets]
        m["bullets_meta"] = meta
        m["sentence_ids"] = [s.id for s in note.summary]
        variants.append(m)
        base_m = {**m, "id": note.id}
        for b, iid in zip(bullets, meta):
            items.append(item_row("symai", base_m, m["id"], iid, b, "clean", 0))
    cases = seed_cases(corpus, mutators=CONTENT_MUTATORS)
    by_id = {n.id: n for n in corpus}
    for k, case in enumerate(cases):
        note = case.note
        m, tid = to_meeting(note, f"{case.base_note}#{case.mutator}")
        base_cites = {s.id: [tid[x] for x in s.source_turns if x in tid] for s in by_id[case.base_note].summary}
        targets = set(case.affected) if case.mutator == "cluster_insert" else {case.target}
        bullets, meta = [], []
        for j, s in enumerate(note.summary):
            cites = [tid[x] for x in s.source_turns if x in tid]
            if not cites:  # inserted sentence: cite the neighbouring sentence's turns (as a generator would)
                nb = note.summary[j - 1] if j > 0 else note.summary[min(j + 1, len(note.summary) - 1)]
                cites = base_cites.get(nb.id) or [tid[x] for x in nb.source_turns if x in tid] or [0]
            pos = s.id in targets
            bullets.append({"text": s.text, "cites": cites, "gold": "hallucinated" if pos else "ok",
                            **({"type": TYPE_OF[case.mutator]} if pos else {})})
            meta.append(f"symai:{case.case_id}:{s.id}" if pos else None)
        m["bullets"], m["bullets_meta"] = bullets, meta
        m["sentence_ids"] = [s.id for s in note.summary]
        variants.append(m)
        base_m = {**m, "id": case.base_note}
        for b, iid, s in zip(bullets, meta, note.summary):
            if iid:
                items.append(item_row("symai", base_m, m["id"], iid, {**b, "lang": "en"}, TYPE_OF[case.mutator], 1,
                                      case.original))
    return variants, items


def assemble() -> None:
    ensure(OUT)
    # synthetic: almond fixtures + SymAI minutes
    am, ac, af = almond_subset()
    v1, i1 = assemble_from("almond", am, ac, k_per_type=2, fixed_pos=af)
    v2, i2 = symai_subset()
    dump_json(OUT / "synthetic.json", {"meetings": v1 + v2})
    write_jsonl(OUT / "synthetic_items.jsonl", i1 + i2)
    print("synthetic:", len(v1 + v2), "variants,", len(i1 + i2), "items",
          dict(Counter((r["subset"], r["lang"], r["type"]) for r in i1 + i2)))
    # personal
    cp = PRIVATE / "summaries_clean.json"
    if cp.exists():
        pm = personal_meetings()
        v3, i3 = assemble_from("personal", pm, load_json(cp), k_per_type=int(sys.argv[2]) if len(sys.argv) > 2 else 6)
        dump_json(PRIVATE / "personal.json", {"meetings": v3})
        write_jsonl(PRIVATE / "personal_items.jsonl", i3)
        print("personal:", len(v3), "variants,", len(i3), "items",
              dict(Counter((r["lang"], r["type"]) for r in i3)))


if __name__ == "__main__":
    {"summarize": summarize, "filter": filter_bullets, "assemble": assemble}[sys.argv[1]]()
