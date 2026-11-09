"""GPU reference eval of Whisper-family checkpoints with Hugging Face transformers.

Used for GPU numbers on the Windows PC, where the Rust whisper.cpp CUDA build is blocked on
libclang (see RESULTS.md). Same manifests, same normalization and the same WER/CER definitions as
`crates/almond-stt/examples/stt_eval.rs` (scoring code shared via score_meeting.normalize/edits).

Usage:
  python eval/stt/hf_eval.py --model vinai/PhoWhisper-large --set vivos-test [--limit N]
         [--lang vi] [--beam 5] [--dtype fp16] [--device cuda]

Writes <eval_dir>/results/<model>__<set>__hf-<device>-<dtype>-beam<k>.summary.json (+ .jsonl).
"""

import argparse
import json
import sys
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
from prepare_data import eval_dir  # noqa: E402
from score_meeting import edits, normalize  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--set", required=True)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--lang")
    ap.add_argument("--beam", type=int, default=5)
    ap.add_argument("--dtype", default="fp16", choices=["fp16", "fp32"])
    ap.add_argument("--device", default="cuda")
    a = ap.parse_args()

    import soundfile as sf
    import torch
    from transformers import WhisperForConditionalGeneration, WhisperProcessor

    dtype = torch.float16 if a.dtype == "fp16" else torch.float32
    t0 = time.time()
    proc = WhisperProcessor.from_pretrained(a.model)
    model = WhisperForConditionalGeneration.from_pretrained(a.model, torch_dtype=dtype).to(a.device).eval()
    load_s = time.time() - t0

    rows = [json.loads(l) for l in (eval_dir() / a.set / "manifest.jsonl").open(encoding="utf8")]
    if a.limit:
        rows = rows[: a.limit]

    def run(x, lang):
        feats = proc(x, sampling_rate=16000, return_tensors="pt").input_features.to(a.device, dtype)
        with torch.inference_mode():
            ids = model.generate(feats, language=lang, task="transcribe", num_beams=a.beam, max_new_tokens=440)
        if a.device == "cuda":
            torch.cuda.synchronize()
        return proc.batch_decode(ids, skip_special_tokens=True)[0]

    x0, _ = sf.read(rows[0]["audio"], dtype="float32")
    run(x0[:16000], a.lang or rows[0]["lang"])  # warm-up

    we = wr = ce = cr = 0
    audio_s = proc_s = 0.0
    out = []
    for i, r in enumerate(rows):
        x, sr = sf.read(r["audio"], dtype="float32")
        assert sr == 16000
        t = time.time()
        hyp = run(x, a.lang or r["lang"])
        dt = time.time() - t
        rw, hw = normalize(r["text"]).split(), normalize(hyp).split()
        rc, hc = list(normalize(r["text"]).replace(" ", "")), list(normalize(hyp).replace(" ", ""))
        e_w, e_c = edits(rw, hw), edits(rc, hc)
        we, wr, ce, cr = we + e_w, wr + len(rw), ce + e_c, cr + len(rc)
        audio_s += len(x) / 16000
        proc_s += dt
        out.append({"id": r["id"], "ref": r["text"], "hyp": hyp, "sec": dt})
        if (i + 1) % 50 == 0:
            print(f"[{i + 1}/{len(rows)}] WER {100 * we / wr:.2f}% CER {100 * ce / cr:.2f}% RTF {proc_s / audio_s:.4f}",
                  file=sys.stderr)
    dev = f"hf-{a.device}-{a.dtype}-beam{a.beam}"
    summary = {"model": a.model, "set": a.set, "n": len(rows), "device": dev, "wer": we / wr, "cer": ce / cr,
               "ref_words": wr, "audio_s": audio_s, "proc_s": proc_s, "rtf": proc_s / audio_s, "load_s": load_s}
    print(json.dumps(summary))
    res = eval_dir() / "results"
    res.mkdir(parents=True, exist_ok=True)
    stem = f"{a.model.replace('/', '_')}__{a.set}__{dev}"
    (res / f"{stem}.summary.json").write_text(json.dumps(summary))
    with (res / f"{stem}.jsonl").open("w", encoding="utf8") as f:
        for o in out:
            f.write(json.dumps(o, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    sys.exit(main())
