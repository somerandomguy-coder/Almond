"""Hugging Face models used by the benchmark (total download ~2.8 GB, cache ~/.cache/huggingface).

    uv run python -m vb.models        # download only what is needed (no ONNX/TF duplicates)
"""

from __future__ import annotations

from huggingface_hub import snapshot_download

MINILM = "sentence-transformers/all-MiniLM-L6-v2"  # SymAI retriever (English)
DEBERTA_NLI = "cross-encoder/nli-deberta-v3-base"  # SymAI NLI (English)
MNLI = "MoritzLaurer/mDeBERTa-v3-base-xnli-multilingual-nli-2mil7"  # multilingual NLI (SummaC, SymAI-VI)
MINICHECK = "lytang/MiniCheck-RoBERTa-Large"  # MiniCheck (English-only training data)

PATTERNS = {
    MINILM: ["*.json", "*.txt", "model.safetensors", "1_Pooling/*"],
    DEBERTA_NLI: ["*.json", "*.txt", "*.model", "model.safetensors"],
    MNLI: ["*.json", "*.model", "model.safetensors"],
    MINICHECK: ["*.json", "*.txt", "pytorch_model.bin"],
}


def main() -> None:
    for repo, pats in PATTERNS.items():
        path = snapshot_download(repo, allow_patterns=pats)
        print(repo, "->", path)


if __name__ == "__main__":
    main()
