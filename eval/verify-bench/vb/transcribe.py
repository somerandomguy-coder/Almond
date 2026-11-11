"""Step 1: decode the private .m4a recordings to 16 kHz mono WAV and transcribe them with
Almond's own speech stack (`crates/almond-stt/examples/transcribe.rs`, CUDA build: Zipformer-30M
for Vietnamese, whisper large-v3-turbo for English + language ID, pyannote + 3D-Speaker diarization).

    uv run python -m vb.transcribe            # all recordings under PRIVATE/audio
Outputs PRIVATE/transcripts/<name>.json (Almond `Transcript` artifact body) and a timing log.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import wave
from pathlib import Path

import av
import numpy as np

from vb.paths import PRIVATE, ensure

EXE = Path(os.environ["USERPROFILE"]) / ".almond/t/release/examples/transcribe.exe"


def to_wav(src: Path, dst: Path) -> float:
    """Decode any container PyAV reads to 16 kHz mono s16 WAV. Returns duration (s)."""
    container = av.open(str(src))
    resampler = av.AudioResampler(format="s16", layout="mono", rate=16000)
    chunks = []
    for frame in container.decode(audio=0):
        for out in resampler.resample(frame):
            chunks.append(out.to_ndarray().reshape(-1))
    for out in resampler.resample(None):
        chunks.append(out.to_ndarray().reshape(-1))
    pcm = np.concatenate(chunks).astype(np.int16)
    with wave.open(str(dst), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(pcm.tobytes())
    return len(pcm) / 16000


def main() -> None:
    audio = PRIVATE / "audio"
    wavs, outs = PRIVATE / "wav", PRIVATE / "transcripts"
    ensure(wavs, outs)
    log = []
    for i, src in enumerate(sorted(audio.glob("*.m4a")), start=1):
        mid = f"rec{i}"  # neutral ids: file names stay private
        wav = wavs / f"{mid}.wav"
        dur = to_wav(src, wav) if not wav.exists() else wave.open(str(wav)).getnframes() / 16000
        out = outs / f"{mid}.json"
        if out.exists() and "--force" not in sys.argv:
            print(f"{mid}: exists, skip")
            continue
        t0 = time.time()
        p = subprocess.run([str(EXE), str(wav), "--vi", "sherpa:zipformer-vi-30m-int8",
                            "--en", "whisper:large-v3-turbo", "--langs", "vi,en", "--out", str(out)],
                           capture_output=True, text=True, encoding="utf-8", errors="replace")
        el = time.time() - t0
        print(f"{mid}: {dur/60:.1f} min audio, {el:.0f} s, exit {p.returncode}")
        print(p.stderr[-1500:])
        log.append({"id": mid, "source": src.name, "duration_s": round(dur, 1), "wall_s": round(el, 1),
                    "exit": p.returncode, "stderr_tail": p.stderr[-800:]})
    if log:
        (outs / "transcribe_log.json").write_text(json.dumps(log, ensure_ascii=False, indent=1), encoding="utf-8")


if __name__ == "__main__":
    main()
