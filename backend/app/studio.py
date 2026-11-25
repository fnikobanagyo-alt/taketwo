"""Studio restoration: turns echoey / noisy / low-bandwidth speech into clean 48 kHz
"studio mic" speech with Sidon (speech restoration: denoise + dereverb + bandwidth
restoration, MIT licence), run locally through ONNX Runtime.

Used two ways:
* on the reference at clone time, so the model never learns the room echo, hiss or
  phone-mic colour in the first place (the biggest cause of robotic/echoey clones);
* optionally on the finished narration, to polish codec roughness and restore air.
"""
from __future__ import annotations

import numpy as np

from . import audio as au

REPO = "soniqo/Sidon-ONNX"
VARIANT = "fp16"
FE_REPO = "facebook/w2v-bert-2.0"
SR_IN, SR_OUT = 16000, 48000
CHUNK_S, OVERLAP_S = 20.0, 1.0

_sess = None
_fe = None


def _download(repo: str, name: str) -> str:
    from huggingface_hub import hf_hub_download

    try:
        return hf_hub_download(repo, name, local_files_only=True)
    except Exception:
        for attempt in range(4):
            try:
                return hf_hub_download(repo, name)
            except Exception:
                if attempt == 3:
                    raise


def is_installed() -> bool:
    try:
        from huggingface_hub import hf_hub_download

        for n in (f"{VARIANT}/sidon-predictor.onnx", f"{VARIANT}/sidon-vocoder.onnx"):
            hf_hub_download(REPO, n, local_files_only=True)
        hf_hub_download(FE_REPO, "preprocessor_config.json", local_files_only=True)
        return True
    except Exception:
        return False


def _load():
    global _sess, _fe
    if _sess is None:
        import onnxruntime as ort
        from transformers import SeamlessM4TFeatureExtractor

        opts = ort.SessionOptions()
        opts.log_severity_level = 3
        prov = ["CPUExecutionProvider"]
        _sess = (
            ort.InferenceSession(_download(REPO, f"{VARIANT}/sidon-predictor.onnx"), opts, providers=prov),
            ort.InferenceSession(_download(REPO, f"{VARIANT}/sidon-vocoder.onnx"), opts, providers=prov),
        )
        _download(FE_REPO, "preprocessor_config.json")
        _fe = SeamlessM4TFeatureExtractor.from_pretrained(FE_REPO)
    return _sess, _fe


def _restore_segment(x16: np.ndarray) -> np.ndarray:
    (pred, voc), fe = _load()
    pad = int(0.5 * SR_IN)  # a little tail room so the last word decodes fully
    x = np.concatenate([x16, np.zeros(pad, np.float32)])
    feats = fe(x, sampling_rate=SR_IN, return_tensors="np")["input_features"].astype(np.float32)
    h = pred.run(None, {"input_features": feats})[0]
    y = voc.run(None, {"features": h.astype(np.float32)})[0][0]
    return y[: len(x16) * 3].astype(np.float32)


def restore(a: np.ndarray, sr: int, progress=lambda p: None) -> tuple[np.ndarray, int]:
    """Restore speech; returns (48 kHz audio, 48000). Long audio is processed in
    20 s windows with 1 s crossfaded overlap."""
    x = au.highpass(au.resample(a, sr, SR_IN), SR_IN, 50)
    peak = np.max(np.abs(x)) or 1.0
    x = (x / peak * 0.9).astype(np.float32)
    n, step, ov = len(x), int(CHUNK_S * SR_IN), int(OVERLAP_S * SR_IN)
    if n <= step + ov:
        y = _restore_segment(x)
    else:
        y = np.zeros(n * 3, np.float32)
        w = np.zeros(n * 3, np.float32)
        starts = list(range(0, n, step))
        for k, s in enumerate(starts):
            seg = x[max(0, s - ov) : min(n, s + step + ov)]
            out = _restore_segment(seg)
            o0 = max(0, s - ov) * 3
            ramp = np.ones(len(out), np.float32)
            r = ov * 3
            if s > 0:
                ramp[:r] = np.linspace(0, 1, r)
            if s + step + ov < n:
                ramp[-r:] = np.linspace(1, 0, r)
            y[o0 : o0 + len(out)] += out * ramp
            w[o0 : o0 + len(out)] += ramp
            progress((k + 1) / len(starts))
        y = y / np.maximum(w, 1e-4)
    return y.astype(np.float32), SR_OUT


# ------------------------------------------------------------------ mastering


def expander(a: np.ndarray, sr: int, below_db: float = 45.0, max_cut_db: float = 8.0, guard_ms: int = 300) -> np.ndarray:
    """Soft downward expander: lowers breath/room/hiss in the gaps between words by up to
    8 dB with slow, smooth gain moves. Only true silence is touched: any frame within
    guard_ms of speech (word tails, soft consonants, decays) keeps its level, so a word
    can never fade out under the expander."""
    hop = max(1, int(0.005 * sr))
    n = len(a) // hop
    if n < 20:
        return a
    fr = a[: n * hop].reshape(n, hop)
    lvl = 10 * np.log10(np.mean(fr**2, axis=1) + 1e-12)
    p90 = np.percentile(lvl, 90)
    thr = p90 - below_db
    target = -np.clip((thr - lvl) * 1.0, 0, max_cut_db)  # 1:1 below threshold
    guard = max(1, int(guard_ms / 5))
    near_speech = lvl > p90 - 42  # speech and its soft edges
    # 'same' has the longer operand's size when the clip is shorter than the
    # guard kernel. Padding + 'valid' always returns exactly n frames.
    protected = np.convolve(np.pad(near_speech.astype(np.float32), (guard, guard)),
                            np.ones(2 * guard + 1), mode="valid") > 0
    target[protected] = 0.0
    g = np.empty(n)
    cur = 0.0
    att, rel = np.exp(-1 / 2.0), np.exp(-1 / 30.0)  # ~10 ms open, ~150 ms close (in 5 ms frames)
    for i, t in enumerate(target):
        coef = att if t > cur else rel
        cur = coef * cur + (1 - coef) * t
        g[i] = cur
    gain = 10 ** (np.interp(np.arange(len(a)), np.arange(n) * hop + hop / 2, g) / 20)
    return (a * gain).astype(np.float32)


def de_ess(y: np.ndarray, sr: int, strength: float = 1.0) -> np.ndarray:
    """Dynamic de-esser: only when the 4.5-9.5 kHz band spikes relative to the whole
    signal (a harsh 's'/'sh'), that band is pulled down, up to about 9 dB."""
    from scipy.signal import butter, sosfiltfilt

    if sr < 20000:
        return y
    sos = butter(4, [4500, min(9500, sr / 2 * 0.95)], btype="bandpass", fs=sr, output="sos")
    s = sosfiltfilt(sos, y)
    n = int(0.004 * sr)
    k = np.ones(n) / n
    env_s = np.sqrt(np.convolve(s**2, k, mode="same") + 1e-12)
    env_a = np.sqrt(np.convolve(y**2, k, mode="same") + 1e-12)
    ratio = env_s / env_a
    reduce = np.clip((ratio - 0.35) * 1.6 * strength, 0, 0.65)  # fraction of band removed
    # Only harsh peaks in loud speech are tamed. Soft fricative word endings ("s", "t",
    # "sh") are quiet and band-heavy, and cutting them made words stop short.
    loud_db = 20 * np.log10(env_a)
    loud = (loud_db > np.percentile(loud_db, 90) - 14).astype(np.float32)
    loud = np.convolve(loud, np.ones(3 * n) / (3 * n), mode="same")
    reduce = reduce * loud
    reduce = np.convolve(reduce, np.ones(3 * n) / (3 * n), mode="same")
    return (y - s * reduce).astype(np.float32)


def master_chain(a: np.ndarray, sr: int) -> np.ndarray:
    """Broadcast-style voice finish: rumble cut, de-ess, light compression, clean gaps.
    Deliberately subtle: it should sound like a good mic, not like an effect."""
    y = au.highpass(a, sr, 75)
    y = de_ess(y, sr)
    # light 2:1 compression above -24 dBFS (RMS, 30 ms), smooth gain
    n = int(0.03 * sr)
    rms = np.sqrt(np.convolve(y**2, np.ones(n) / n, mode="same") + 1e-12)
    lvl = 20 * np.log10(rms)
    over = np.maximum(0, lvl - (-24))
    g = 10 ** (-(over * 0.5) / 20)
    g = np.convolve(g, np.ones(n) / n, mode="same")
    y = (y * g).astype(np.float32)
    return expander(y, sr)
