"""Score diarization turns (JSON from examples/diarize.rs) of the synthetic meeting's system channel.

Usage: python eval/stt/score_diarization.py turns.json [ref.json]

For each reference utterance on the system channel, the hypothesis speaker is the turn label with
the most overlap. Each hypothesis label is mapped to the reference speaker it covers most
(many-to-one); attribution accuracy = utterances whose label maps to their true speaker.
Also reports purity-style numbers for VI speakers only (EN utterances come from many FLEURS voices).
"""

import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from prepare_data import eval_dir  # noqa: E402


def main():
    turns = json.load(open(sys.argv[1], encoding="utf8"))
    ref_path = Path(sys.argv[2]) if len(sys.argv) > 2 else eval_dir() / "meeting" / "meeting.ref.json"
    refs = [r for r in json.load(open(ref_path, encoding="utf8")) if r["channel"] == "system"]
    labels = []
    missed = 0
    for r in refs:
        ov = Counter()
        for t in turns:
            o = min(r["t1_ms"], t["t1_ms"]) - max(r["t0_ms"], t["t0_ms"])
            if o > 0:
                ov[t["speaker"]] += o
        if not ov:
            missed += 1
            continue
        labels.append((r, ov.most_common(1)[0][0]))
    vi = [(r, h) for r, h in labels if r["lang"] == "vi"]
    m = defaultdict(Counter)
    for r, h in vi:
        m[h][r["speaker"]] += 1
    mapping = {h: c.most_common(1)[0][0] for h, c in m.items()}
    ok = sum(mapping[h] == r["speaker"] for r, h in vi)
    # how fragmented is each true speaker (number of labels used)
    frag = defaultdict(set)
    for r, h in vi:
        frag[r["speaker"]].add(h)
    print(f"hyp speakers: {len({t['speaker'] for t in turns})}, turns: {len(turns)}, system utterances without any turn: {missed}/{len(refs)}")
    print(f"VI remote utterances attributed correctly: {100 * ok / max(len(vi), 1):.1f}% ({len(vi)})")
    print("labels per true VI speaker:", {k: len(v) for k, v in sorted(frag.items())})


if __name__ == "__main__":
    main()
