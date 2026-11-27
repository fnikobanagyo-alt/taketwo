"""Python side of the Sidon parity run (step S18). The ignored Rust test `sidon_parity` calls it:

    TAKETWO_DATA=<fresh empty tmp> VOICEFORGE_DATA=<same> HF_HUB_OFFLINE=1 \
        backend/.venv/bin/python -B rust/scripts/sidon_python_ref.py <dir>

Reads <dir>/noisy.f32 (16 kHz mono float32, written by the Rust test) and writes, all float32 little-endian:
  feats.f32        SeamlessM4TFeatureExtractor on the clip plus the 0.5 s zero tail, as _restore_segment does
  feats_short.f32  the same for the clip without its last 160 samples (the other frame-count parity)
  out.f32          the app's own studio._restore_segment(clip)  (48 kHz)
  python.json      shapes, load and restore seconds, peak memory
Imports only numpy, transformers, onnxruntime and backend/app/studio.py (read-only); nothing is downloaded.
"""
from __future__ import annotations

import json
import os
import resource
import sys
import time
from pathlib import Path

import numpy as np

WT = Path(__file__).resolve().parent.parent.parent


def guard() -> None:
    data = Path(os.environ["TAKETWO_DATA"])
    assert os.environ.get("VOICEFORGE_DATA") == str(data), "TAKETWO_DATA and VOICEFORGE_DATA must match"
    assert os.environ.get("HF_HUB_OFFLINE") == "1", "HF_HUB_OFFLINE=1 is required"
    assert data.is_dir() and not any(data.iterdir()), f"data folder must be a fresh empty folder: {data}"
    sys.path.insert(0, str(WT / "backend"))
    from app import config

    assert config.DATA_DIR.resolve() == data.resolve(), config.DATA_DIR  # never the real library


def main() -> None:
    d = Path(sys.argv[1])
    guard()
    from transformers import SeamlessM4TFeatureExtractor

    from app import studio

    x16 = np.fromfile(d / "noisy.f32", dtype="<f4")
    pad = np.zeros(int(0.5 * studio.SR_IN), np.float32)
    fe = SeamlessM4TFeatureExtractor.from_pretrained(studio.FE_REPO)
    shapes = {}
    for name, clip in (("feats", x16), ("feats_short", x16[:-160])):
        f = fe(np.concatenate([clip, pad]), sampling_rate=studio.SR_IN, return_tensors="np")["input_features"].astype(np.float32)
        f.astype("<f4").tofile(d / f"{name}.f32")
        shapes[name] = list(f.shape)
    t = time.time()
    studio._load()
    load_s = time.time() - t
    t = time.time()
    y = studio._restore_segment(x16)
    restore_s = time.time() - t
    y.astype("<f4").tofile(d / "out.f32")
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    json.dump({"shapes": shapes, "out_len": int(len(y)), "load_s": round(load_s, 2), "restore_s": round(restore_s, 2),
               "peak_rss_bytes": int(rss if sys.platform == "darwin" else rss * 1024)}, open(d / "python.json", "w"), indent=1)


if __name__ == "__main__":
    main()
