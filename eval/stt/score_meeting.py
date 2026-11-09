"""Score a transcript artifact body (JSON from `examples/transcribe.rs`) against meeting.ref.json.

Usage: python eval/stt/score_meeting.py transcript.json [ref.json]

Reports: WER/CER per language (hyp segments assigned to the reference utterance they overlap
most), language-ID accuracy per utterance, speaker attribution (mic -> "me" accuracy; remote
speakers: each hypothesis label is mapped to the reference speaker it covers most; accuracy =
utterances whose label maps to their true speaker), number of speakers found.
Normalization mirrors crates/almond-stt/src/metrics.rs (NFC, lowercase, punctuation -> space).
"""

import json
import sys
import unicodedata
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from prepare_data import eval_dir  # noqa: E402


def normalize(s):
    s = unicodedata.normalize("NFC", s).lower()
    out = []
    for i, c in enumerate(s):
        keep = c.isalnum() or unicodedata.category(c).startswith("M")
        if c == "'" and 0 < i < len(s) - 1 and s[i - 1].isalpha() and s[i + 1].isalpha():
            keep = True
        out.append(c if keep else " ")
    return " ".join("".join(out).split())


def edits(r, h):
    prev = list(range(len(h) + 1))
    for i in range(1, len(r) + 1):
        cur = [i] + [0] * len(h)
        for j in range(1, len(h) + 1):
            cur[j] = min(prev[j - 1] + (r[i - 1] != h[j - 1]), prev[j] + 1, cur[j - 1] + 1)
        prev = cur
    return prev[-1]


def main():
    hyp = json.load(open(sys.argv[1], encoding="utf8"))
    ref_path = Path(sys.argv[2]) if len(sys.argv) > 2 else eval_dir() / "meeting" / "meeting.ref.json"
    refs = json.load(open(ref_path, encoding="utf8"))
    segs = hyp["segments"]

    # Split segments into per-word pseudo-segments when word timings exist, so a hypothesis
    # segment spanning several reference utterances is scored against each of them.
    expanded = []
    for s in segs:
        if s.get("words"):
            for w in s["words"]:
                expanded.append({**s, "t0_ms": w["t0_ms"], "t1_ms": max(w["t1_ms"], w["t0_ms"] + 1), "text": w["text"]})
        else:
            expanded.append(s)
    segs = expanded

    assigned = defaultdict(list)
    unassigned = []
    for s in segs:
        best, bo = None, 0
        for i, r in enumerate(refs):
            o = min(s["t1_ms"], r["t1_ms"]) - max(s["t0_ms"], r["t0_ms"])
            if o > bo:
                best, bo = i, o
        (assigned[best] if best is not None else unassigned).append(s)

    werr, wref, cerr, cref = Counter(), Counter(), Counter(), Counter()
    lid_ok, lid_n = Counter(), Counter()
    spk = []
    for i, r in enumerate(refs):
        hs = sorted(assigned.get(i, []), key=lambda s: s["t0_ms"])
        htext = " ".join(s["text"] for s in hs)
        rw, hw = normalize(r["text"]).split(), normalize(htext).split()
        werr[r["lang"]] += edits(rw, hw)
        wref[r["lang"]] += len(rw)
        rc, hc = normalize(r["text"]).replace(" ", ""), normalize(htext).replace(" ", "")
        cerr[r["lang"]] += edits(list(rc), list(hc))
        cref[r["lang"]] += len(rc)
        if hs:
            dur = Counter()
            spd = Counter()
            for s in hs:
                d = s["t1_ms"] - s["t0_ms"]
                dur[s.get("lang")] += d
                spd[s.get("speaker")] += d
            lid_n[r["lang"]] += 1
            lid_ok[r["lang"]] += dur.most_common(1)[0][0] == r["lang"]
            spk.append((r, spd.most_common(1)[0][0]))
    ins = sum(len(normalize(s["text"]).split()) for s in unassigned)

    print("== text (hyp words not overlapping any reference utterance: %d)" % ins)
    for l in sorted(wref):
        print(f"  {l}: WER {100 * werr[l] / wref[l]:.2f}%  CER {100 * cerr[l] / max(cref[l], 1):.2f}%  ({wref[l]} ref words)")
    tw = sum(werr.values()) + ins
    print(f"  all: WER {100 * tw / sum(wref.values()):.2f}%")
    print("== language ID per utterance")
    for l in sorted(lid_n):
        print(f"  {l}: {100 * lid_ok[l] / lid_n[l]:.1f}% ({lid_n[l]} utt)")
    print("== speakers")
    me = [(r, h) for r, h in spk if r["channel"] == "mic"]
    print(f"  mic utterances labelled 'me': {100 * sum(h == 'me' for _, h in me) / max(len(me), 1):.1f}% ({len(me)})")
    remote = [(r, h) for r, h in spk if r["channel"] == "system" and r["lang"] == "vi"]
    m = defaultdict(Counter)
    for r, h in remote:
        m[h][r["speaker"]] += 1
    mapping = {h: c.most_common(1)[0][0] for h, c in m.items()}
    ok = sum(mapping.get(h) == r["speaker"] for r, h in remote)
    ref_spk = {r["speaker"] for r, _ in remote}
    print(f"  remote VI utterances attributed to the right speaker: {100 * ok / max(len(remote), 1):.1f}% ({len(remote)})")
    print(f"  remote VI speakers: ref {len(ref_spk)}, hyp labels {len(m)}; all hyp speakers: {[s['id'] for s in hyp['speakers']]}")


if __name__ == "__main__":
    main()
