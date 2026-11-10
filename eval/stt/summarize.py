"""Collect <eval_dir>/results/*.summary.json into a Markdown table (paste into RESULTS.md)."""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from prepare_data import eval_dir  # noqa: E402


def main():
    rows = [json.loads(p.read_text()) for p in sorted((eval_dir() / "results").glob("*.summary.json"))]
    rows.sort(key=lambda r: (r["set"], r["model"], r["device"]))
    print("| set | model | runtime | n | WER % | CER % | RTF | audio min |")
    print("|---|---|---|---:|---:|---:|---:|---:|")
    for r in rows:
        print(
            f"| {r['set']} | {r['model']} | {r['device']}{(' ' + r['tag']) if r.get('tag') else ''} | {r['n']} "
            f"| {100 * r['wer']:.2f} | {100 * r['cer']:.2f} | {r['rtf']:.3f} | {r['audio_s'] / 60:.1f} |"
        )


if __name__ == "__main__":
    main()
