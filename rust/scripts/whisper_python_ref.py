"""Python side of the Whisper parity run (step S13). Called by rust/scripts/whisper_parity.sh.

    TAKETWO_DATA=<fresh tmp> VOICEFORGE_DATA=<same> HF_HUB_OFFLINE=1 \
        backend/.venv/bin/python -B rust/scripts/whisper_python_ref.py <clips dir> <out json>
    ... whisper_python_ref.py --heads     only load the model and print its alignment heads

Runs the app's own ml.transcribe(clip, 16000, language="en", words=True) on clip1..clip4.wav
(and once with language=None on clip1), and writes what it returns, plus each word normalized
by the app's own ml.normalize_for_wer. The data folder must be a fresh temporary folder.
"""
from __future__ import annotations

import json
import os
import resource
import sys
import time
import wave
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
WT = HERE.parent.parent
CLIPS = ["clip1", "clip2", "clip3", "clip4"]


def guard() -> None:
    data = Path(os.environ["TAKETWO_DATA"])
    assert os.environ.get("VOICEFORGE_DATA") == str(data), "TAKETWO_DATA and VOICEFORGE_DATA must match"
    assert os.environ.get("HF_HUB_OFFLINE") == "1", "HF_HUB_OFFLINE=1 is required"
    assert data.is_dir() and not any(data.iterdir()), f"data folder must be a fresh empty folder: {data}"
    sys.path.insert(0, str(WT / "backend"))
    from app import config

    assert config.DATA_DIR.resolve() == data.resolve(), config.DATA_DIR  # never the real library


def read_wav(path: Path) -> tuple[np.ndarray, int]:
    with wave.open(str(path), "rb") as w:
        assert w.getnchannels() == 1 and w.getsampwidth() == 2, path
        sr = w.getframerate()
        a = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").astype(np.float32) / 32768.0
    return a, sr


def main() -> None:
    guard()
    from app import ml

    if sys.argv[1:] == ["--heads"]:
        heads = np.array(ml.stt_model().alignment_heads)
        print("alignment_heads.shape", tuple(heads.shape))
        print("alignment_heads", heads.tolist())
        return

    clips, out = Path(sys.argv[1]), Path(sys.argv[2])
    t = time.time()
    model = ml.stt_model()
    load_s = time.time() - t
    heads = np.array(model.alignment_heads)
    print("alignment_heads.shape", tuple(heads.shape), flush=True)
    result = {"load_seconds": load_s, "alignment_heads": heads.tolist(), "clips": {}}
    runs = [(c, "en") for c in CLIPS] + [("clip1", None)]
    for name, lang in runs:
        a, sr = read_wav(clips / f"{name}.wav")
        assert sr == 16000, (name, sr)
        t = time.time()
        r = ml.transcribe(a, sr, language=lang, words=True)
        secs = time.time() - t
        words = [w for s in r["segments"] for w in s["words"]]
        key = name if lang else f"{name}_auto"
        result["clips"][key] = {
            "duration": len(a) / sr,
            "seconds": secs,
            "transcript": r,
            "normalized_words": [ml.normalize_for_wer(w["word"]) for w in words],
        }
        print(f"{key}: {secs:.2f} s, {len(words)} words, language {r['language']}", flush=True)
    # ru_maxrss is bytes on macOS
    result["peak_rss_bytes"] = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(result, indent=1, ensure_ascii=False))
    print("wrote", out)


if __name__ == "__main__":
    main()
