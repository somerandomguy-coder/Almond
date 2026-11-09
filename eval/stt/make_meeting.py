"""Build a synthetic ~30-min two-channel VI/EN "online meeting" from the prepared eval sets.

Left channel = mic (the local user, one VIVOS speaker), right channel = system loopback (other
VIVOS speakers + English utterances from FLEURS). Turns never overlap. Writes:

  <eval_dir>/meeting/meeting_stereo.wav   L = mic, R = system
  <eval_dir>/meeting/meeting_mono.wav     mixdown (for the "mixed" path)
  <eval_dir>/meeting/meeting.ref.json     [{t0_ms, t1_ms, channel, speaker, lang, text}]

Usage: python eval/stt/make_meeting.py [--minutes 30] [--remote 5] [--en 60] [--seed 0]
Requires prepare_data.py vivos-test fleurs-en to have run first.
"""

import argparse
import json
import random
import sys
import wave
from collections import defaultdict
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
from prepare_data import eval_dir  # noqa: E402


def read(path):
    with wave.open(path, "rb") as w:
        assert w.getframerate() == 16000 and w.getnchannels() == 1
        return np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32) / 32768


def write(path, chans):
    x = np.stack(chans, axis=1) if len(chans) > 1 else chans[0][:, None]
    pcm = (np.clip(x, -1, 1) * 32767).astype(np.int16)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(pcm.shape[1])
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(pcm.tobytes())


def load_manifest(name):
    p = eval_dir() / name / "manifest.jsonl"
    return [json.loads(l) for l in p.open(encoding="utf8")]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--minutes", type=float, default=30)
    ap.add_argument("--remote", type=int, default=5, help="remote Vietnamese speakers")
    ap.add_argument("--en", type=int, default=60, help="English utterances (remote)")
    ap.add_argument("--seed", type=int, default=0)
    a = ap.parse_args()
    rng = random.Random(a.seed)

    by_spk = defaultdict(list)
    for r in load_manifest("vivos-test"):
        by_spk[r["speaker"]].append(r)
    spks = sorted(by_spk)
    rng.shuffle(spks)
    me, remotes = spks[0], spks[1 : 1 + a.remote]
    en = load_manifest("fleurs-en")
    rng.shuffle(en)
    en = en[: a.en]

    # queues per speaker; English utterances belong to pseudo-speaker "EN" on the system side
    queues = {s: list(by_spk[s]) for s in [me] + remotes}
    queues["EN"] = en
    for q in queues.values():
        rng.shuffle(q)

    sr = 16000
    target = int(a.minutes * 60 * sr)
    mic, sysch = [], []
    refs = []
    t = int(0.5 * sr)
    last = None
    while t < target and any(queues.values()):
        cands = [s for s, q in queues.items() if q and s != last]
        if not cands:
            cands = [s for s, q in queues.items() if q]
        # "me" talks ~25% of turns
        weights = [3.0 if s == me else 1.0 for s in cands]
        s = rng.choices(cands, weights)[0]
        last = s
        for _ in range(rng.randint(1, 3)):
            if not queues[s]:
                break
            u = queues[s].pop()
            x = read(u["audio"])
            ch = "mic" if s == me else "system"
            (mic if ch == "mic" else sysch).append((t, x))
            refs.append(
                {
                    "t0_ms": t * 1000 // sr,
                    "t1_ms": (t + len(x)) * 1000 // sr,
                    "channel": ch,
                    "speaker": "me" if s == me else s,
                    "lang": u["lang"],
                    "text": u["text"],
                }
            )
            t += len(x) + int(rng.uniform(0.25, 0.8) * sr)
        t += int(rng.uniform(0.3, 1.2) * sr)

    n = t + sr
    L = np.zeros(n, np.float32)
    R = np.zeros(n, np.float32)
    for t0, x in mic:
        L[t0 : t0 + len(x)] += x
    for t0, x in sysch:
        R[t0 : t0 + len(x)] += x
    # a little noise floor so silence isn't digital zero
    noise = np.random.default_rng(a.seed).normal(0, 0.0007, (2, n)).astype(np.float32)
    L += noise[0]
    R += noise[1]

    out = eval_dir() / "meeting"
    out.mkdir(parents=True, exist_ok=True)
    write(out / "meeting_stereo.wav", [L, R])
    write(out / "meeting_mono.wav", [0.5 * (L + R)])
    (out / "meeting.ref.json").write_text(json.dumps(refs, ensure_ascii=False, indent=1), encoding="utf8")
    langs = defaultdict(int)
    for r in refs:
        langs[r["lang"]] += 1
    print(f"{n / sr / 60:.1f} min, {len(refs)} utterances {dict(langs)}, me={me}, remote={remotes} -> {out}")


if __name__ == "__main__":
    sys.exit(main())
