"""Convert VinAI PhoWhisper (Hugging Face transformers checkpoint) to whisper.cpp ggml.

Adapted from whisper.cpp `models/convert-h5-to-ggml.py` (MIT). Differences:
  * mel filters come from transformers' WhisperFeatureExtractor (same librosa/slaney filters as
    openai/whisper's mel_filters.npz), so no openai/whisper checkout is needed;
  * downloads the checkpoint with huggingface_hub and writes straight into the Almond models dir
    (`ALMOND_MODELS_DIR` or ~/.almond/models/whisper__phowhisper-<size>/ggml-phowhisper-<size>.bin),
    the layout `crates/almond-stt/models.json` expects;
  * prints the SHA-256 of the result.

Usage:
    python eval/stt/convert_phowhisper.py small|medium|large [--out FILE] [--f32]

Requirements: torch, transformers, huggingface_hub, numpy. PhoWhisper license: BSD-3-Clause.
"""

import argparse
import hashlib
import json
import os
import struct
import sys
from pathlib import Path

import numpy as np

CONV_MAP = {
    "self_attn.k_proj": "attn.key",
    "self_attn.q_proj": "attn.query",
    "self_attn.v_proj": "attn.value",
    "self_attn.out_proj": "attn.out",
    "self_attn_layer_norm": "attn_ln",
    "encoder_attn.q_proj": "cross_attn.query",
    "encoder_attn.v_proj": "cross_attn.value",
    "encoder_attn.out_proj": "cross_attn.out",
    "encoder_attn_layer_norm": "cross_attn_ln",
    "fc1": "mlp.0",
    "fc2": "mlp.2",
    "final_layer_norm": "mlp_ln",
    "encoder.layer_norm.bias": "encoder.ln_post.bias",
    "encoder.layer_norm.weight": "encoder.ln_post.weight",
    "encoder.embed_positions.weight": "encoder.positional_embedding",
    "decoder.layer_norm.bias": "decoder.ln.bias",
    "decoder.layer_norm.weight": "decoder.ln.weight",
    "decoder.embed_positions.weight": "decoder.positional_embedding",
    "decoder.embed_tokens.weight": "decoder.token_embedding.weight",
    "proj_out.weight": "decoder.proj.weight",
}


def bytes_to_unicode():
    bs = (
        list(range(ord("!"), ord("~") + 1))
        + list(range(ord("¡"), ord("¬") + 1))
        + list(range(ord("®"), ord("ÿ") + 1))
    )
    cs = bs[:]
    n = 0
    for b in range(2**8):
        if b not in bs:
            bs.append(b)
            cs.append(2**8 + n)
            n += 1
    return dict(zip(bs, [chr(c) for c in cs]))


def models_dir() -> Path:
    d = os.environ.get("ALMOND_MODELS_DIR")
    return Path(d) if d else Path.home() / ".almond" / "models"


def ggml_name(hf_name: str) -> str:
    nn = hf_name.split(".")[1:]  # drop "model."
    if nn[1] == "layers":
        nn[1] = "blocks"
        inner = ".".join(nn[3:-1])
        if inner == "encoder_attn.k_proj":
            mapped = "attn.key" if nn[0] == "encoder" else "cross_attn.key"
        else:
            mapped = CONV_MAP[inner]
        return ".".join(nn[:3] + [mapped] + nn[-1:])
    name = ".".join(nn)
    return CONV_MAP.get(name, name)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("size", choices=["tiny", "base", "small", "medium", "large"])
    ap.add_argument("--out", type=Path)
    ap.add_argument("--f32", action="store_true")
    a = ap.parse_args()

    import torch
    from huggingface_hub import snapshot_download
    from transformers import WhisperFeatureExtractor, WhisperForConditionalGeneration

    repo = f"vinai/PhoWhisper-{a.size}"
    dir_model = Path(snapshot_download(repo, allow_patterns=["*.json", "*.bin", "*.safetensors", "*.txt"]))
    out = a.out or models_dir() / f"whisper__phowhisper-{a.size}" / f"ggml-phowhisper-{a.size}.bin"
    out.parent.mkdir(parents=True, exist_ok=True)

    vocab = json.load((dir_model / "vocab.json").open(encoding="utf8"))
    hp = json.load((dir_model / "config.json").open(encoding="utf8"))
    max_len = hp.get("max_length") or hp.get("max_target_positions", 448)
    model = WhisperForConditionalGeneration.from_pretrained(dir_model, torch_dtype=torch.float32)
    fe = WhisperFeatureExtractor.from_pretrained(dir_model)
    filters = np.asarray(fe.mel_filters, dtype=np.float32).T  # (n_mels, n_fft/2+1)
    assert filters.shape[0] == hp["num_mel_bins"], filters.shape

    use_f16 = not a.f32
    tmp = out.with_suffix(".bin.tmp")
    with tmp.open("wb") as f:
        f.write(struct.pack("i", 0x67676D6C))
        for v in (
            hp["vocab_size"],
            hp["max_source_positions"],
            hp["d_model"],
            hp["encoder_attention_heads"],
            hp["encoder_layers"],
            int(max_len),
            hp["d_model"],
            hp["decoder_attention_heads"],
            hp["decoder_layers"],
            hp["num_mel_bins"],
            int(use_f16),
        ):
            f.write(struct.pack("i", v))
        f.write(struct.pack("ii", *filters.shape))
        f.write(filters.astype(np.float32).tobytes())

        byte_decoder = {v: k for k, v in bytes_to_unicode().items()}
        f.write(struct.pack("i", len(vocab)))
        for tok, _ in sorted(vocab.items(), key=lambda x: x[1]):
            b = bytearray(byte_decoder[c] for c in tok)
            f.write(struct.pack("i", len(b)))
            f.write(b)

        for src, t in model.state_dict().items():
            if src == "proj_out.weight":  # tied to token embedding
                continue
            name = ggml_name(src)
            data = t.squeeze().numpy()
            if name in ("encoder.conv1.bias", "encoder.conv2.bias"):
                data = data.reshape(data.shape[0], 1)
            n_dims = len(data.shape)
            small = n_dims < 2 or name in (
                "encoder.conv1.bias",
                "encoder.conv2.bias",
                "encoder.positional_embedding",
                "decoder.positional_embedding",
            )
            if use_f16 and not small:
                data, ftype = data.astype(np.float16), 1
            else:
                data, ftype = data.astype(np.float32), 0
            nb = name.encode()
            f.write(struct.pack("iii", n_dims, len(nb), ftype))
            for i in range(n_dims):
                f.write(struct.pack("i", data.shape[n_dims - 1 - i]))
            f.write(nb)
            f.write(np.ascontiguousarray(data).tobytes())
    tmp.replace(out)

    h = hashlib.sha256()
    with out.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    print(f"wrote {out} ({out.stat().st_size} bytes) sha256={h.hexdigest()}")


if __name__ == "__main__":
    sys.exit(main())
