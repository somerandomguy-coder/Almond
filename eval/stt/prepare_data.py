"""Download small public STT eval sets and write 16 kHz mono WAV + manifest.jsonl.

Nothing is committed: data goes to $ALMOND_EVAL_DIR or ~/.almond/eval/stt/<set>/.

Sets:
  vivos-test   VIVOS test split (VNU-HCM AILab, 760 utt, 19 speakers, read Vietnamese) CC BY-NC-SA 4.0
  fleurs-vi    FLEURS vi_vn test (Google, read Wikipedia sentences)                    CC BY 4.0
  fleurs-en    FLEURS en_us test                                                       CC BY 4.0

Usage:
  python eval/stt/prepare_data.py vivos-test fleurs-vi fleurs-en [--limit N] [--seed 0]

Manifest lines: {"id", "audio" (absolute path), "text", "lang", "duration_s", "speaker"}.
"""

import argparse
import csv
import gzip
import io
import json
import os
import random
import sys
import tarfile
import wave
from pathlib import Path

import numpy as np


def eval_dir() -> Path:
    d = os.environ.get("ALMOND_EVAL_DIR")
    return Path(d) if d else Path.home() / ".almond" / "eval" / "stt"


def write_wav16k(path: Path, data: bytes):
    """Normalize any WAV bytes to 16 kHz mono int16."""
    import soundfile as sf

    x, sr = sf.read(io.BytesIO(data), dtype="float32", always_2d=True)
    x = x.mean(axis=1)
    if sr != 16000:
        n = int(round(len(x) * 16000 / sr))
        x = np.interp(np.linspace(0, len(x) - 1, n), np.arange(len(x)), x).astype(np.float32)
    pcm = (np.clip(x, -1, 1) * 32767).astype(np.int16)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(pcm.tobytes())
    return len(pcm) / 16000


def safe_member_name(name: str) -> str:
    base = os.path.basename(name)
    if not base or base.startswith(".") or "/" in base or "\\" in base:
        raise ValueError(f"bad member {name!r}")
    return base


def prep_vivos(out: Path, limit, rng):
    from huggingface_hub import hf_hub_download

    tar_path = hf_hub_download("AILAB-VNUHCM/vivos", "data/vivos.tar.gz", repo_type="dataset")
    prompts = {}
    rows = []
    wav_dir = out / "wav"
    wav_dir.mkdir(parents=True, exist_ok=True)
    with tarfile.open(tar_path, "r:gz") as tf:
        # first pass collects test prompts (they appear before/after waves depending on order)
        members = []
        for m in tf:
            if "/test/" not in m.name:
                continue
            if m.name.endswith("prompts.txt"):
                for line in tf.extractfile(m).read().decode("utf8").splitlines():
                    if line.strip():
                        uid, text = line.split(" ", 1)
                        prompts[uid] = text.strip()
            elif m.name.endswith(".wav") and m.isfile():
                members.append(m)
        if limit:
            rng.shuffle(members)
            members = members[:limit]
        for m in members:
            uid = safe_member_name(m.name)[:-4]
            p = wav_dir / f"{uid}.wav"
            dur = write_wav16k(p, tf.extractfile(m).read())
            rows.append({"id": uid, "audio": str(p), "lang": "vi", "duration_s": dur,
                         "speaker": uid.split("_")[0]})
    for r in rows:
        r["text"] = prompts[r["id"]]
    return rows


def prep_fleurs(out: Path, lang_dir: str, lang: str, limit, rng):
    from huggingface_hub import hf_hub_download

    tsv = hf_hub_download("google/fleurs", f"data/{lang_dir}/test.tsv", repo_type="dataset")
    tgz = hf_hub_download("google/fleurs", f"data/{lang_dir}/audio/test.tar.gz", repo_type="dataset")
    meta = {}
    with open(tsv, encoding="utf8") as f:
        for row in csv.reader(f, delimiter="\t", quoting=csv.QUOTE_NONE):
            # id, file name, raw transcription, transcription, phonemes, num_samples, gender
            meta[row[1]] = {"raw": row[2], "norm": row[3], "gender": row[6] if len(row) > 6 else ""}
    names = sorted(meta)
    if limit:
        rng.shuffle(names)
        names = set(names[:limit])
    wav_dir = out / "wav"
    wav_dir.mkdir(parents=True, exist_ok=True)
    rows = []
    with tarfile.open(tgz, "r:gz") as tf:
        for m in tf:
            if not m.isfile():
                continue
            base = safe_member_name(m.name)
            if base not in names:
                continue
            p = wav_dir / base
            dur = write_wav16k(p, tf.extractfile(m).read())
            rows.append({"id": base[:-4], "audio": str(p), "text": meta[base]["raw"], "lang": lang,
                         "duration_s": dur, "speaker": ""})
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("sets", nargs="+", choices=["vivos-test", "fleurs-vi", "fleurs-en"])
    ap.add_argument("--limit", type=int, default=0, help="random subset size per set (0 = all)")
    ap.add_argument("--seed", type=int, default=0)
    a = ap.parse_args()
    for s in a.sets:
        rng = random.Random(a.seed)
        out = eval_dir() / s
        out.mkdir(parents=True, exist_ok=True)
        if s == "vivos-test":
            rows = prep_vivos(out, a.limit, rng)
        elif s == "fleurs-vi":
            rows = prep_fleurs(out, "vi_vn", "vi", a.limit, rng)
        else:
            rows = prep_fleurs(out, "en_us", "en", a.limit, rng)
        rows.sort(key=lambda r: r["id"])
        with (out / "manifest.jsonl").open("w", encoding="utf8") as f:
            for r in rows:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
        hours = sum(r["duration_s"] for r in rows) / 3600
        print(f"{s}: {len(rows)} utterances, {hours:.2f} h -> {out}")


if __name__ == "__main__":
    sys.exit(main())
