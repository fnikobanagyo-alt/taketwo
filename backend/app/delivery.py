"""Delivery: expressiveness analysis, voice settings mapping, and mic matching.

Cloning models copy *how* the reference clip is spoken (pitch movement, energy,
pacing) as much as *who* is speaking. So expressiveness is controlled mainly by which
reference clip the model hears, then by sampling looseness, then (for engines that
have one) a native emotion control.
"""
from __future__ import annotations

import numpy as np
from scipy.signal import fftconvolve, firwin2

from . import audio as au

# ------------------------------------------------------------------ prosody


def prosody(a: np.ndarray, sr: int) -> dict:
    """Pitch movement (semitone std / range), median pitch level and loudness dynamics over voiced frames."""
    import librosa

    x = au.resample(a, sr, 16000)
    if len(x) < 16000 * 0.8:
        return {"pitch_st": 0.0, "pitch_range": 0.0, "dyn_db": 0.0, "pitch_hz": 0.0}
    hop = 320  # 20 ms
    f0 = librosa.yin(x, fmin=60, fmax=420, sr=16000, frame_length=1024, hop_length=hop)
    db = au.frame_db(x, 16000)
    n = min(len(f0), len(db))
    f0, db = f0[:n], db[:n]
    voiced = (db > np.percentile(db, 90) - 20) & (f0 > 62) & (f0 < 410)
    if voiced.sum() < 10:
        return {"pitch_st": 0.0, "pitch_range": 0.0, "dyn_db": 0.0, "pitch_hz": 0.0}
    st = 12 * np.log2(f0[voiced] / np.median(f0[voiced]))
    st = st[np.abs(st) < 12]  # drop octave errors
    if len(st) < 10:
        return {"pitch_st": 0.0, "pitch_range": 0.0, "dyn_db": 0.0, "pitch_hz": 0.0}
    return {
        "pitch_st": float(np.std(st)),
        "pitch_range": float(np.percentile(st, 95) - np.percentile(st, 5)),
        "dyn_db": float(np.std(db[voiced])),
        "pitch_hz": float(np.median(f0[voiced])),
    }


def expressiveness(p: dict) -> float:
    """Single raw score; only meaningful relative to the same speaker."""
    return p["pitch_st"] + 0.35 * p["pitch_range"] / 4 + 0.25 * p["dyn_db"] / 3


# ------------------------------------------------------------------ voice settings

DEFAULT_VOICE_SETTINGS = {"stability": 0.5, "similarity": 0.75, "style": 0.0, "mic_match": True, "studio_finish": True}


def resolve(vs: dict | None, spec, base_params: dict, qc: dict) -> tuple[dict, dict]:
    """Map friendly voice settings onto engine params and QC thresholds.

    stability  0..1  more variable <-> more stable     (sampling temperature)
    similarity 0..1  low <-> high                       (voice-match bar, guidance)
    style      0..1  none <-> exaggerated               (expressive reference, looseness, emotion)
    Explicit advanced params from the user always win.
    """
    v = {**DEFAULT_VOICE_SETTINGS, **(vs or {})}
    keys = {p.key for p in spec.params}
    params: dict = {}
    if "temperature" in keys:
        # default (stability 0.5) = 0.78: natural variation; QC catches the rare bad take
        params["temperature"] = round(min(1.15, max(0.35, 1.05 - 0.55 * v["stability"] + 0.2 * v["style"])), 3)
    if "cfg_value" in keys:  # VoxCPM2 guidance: higher = closer to reference
        params["cfg_value"] = round(1.4 + 1.6 * v["similarity"], 2)
    if "cfg_weight" in keys:  # Chatterbox
        params["cfg_weight"] = round(0.3 + 0.4 * v["similarity"] - 0.15 * v["style"], 2)
    if "exaggeration" in keys:  # Chatterbox native emotion intensity
        params["exaggeration"] = round(0.4 + 0.8 * v["style"], 2)
    if "guidance_scale" in keys:  # OmniVoice
        params["guidance_scale"] = round(1.5 + 1.5 * v["similarity"], 2)
    params.update({k: val for k, val in (base_params or {}).items() if val is not None})
    qc = dict(qc)
    qc["min_speaker_similarity"] = round(0.70 + 0.2 * v["similarity"], 3)
    return params, qc


def pick_variant(profile: dict, style: float) -> int:
    """Style 0 keeps the user's chosen clip; higher styles move to more expressive clips."""
    active = profile.get("active_variant", 0)
    vs = profile.get("variants") or []
    if style < 0.2 or not vs or "expressiveness" not in vs[0]:
        return active
    best_q = max(v["score"] for v in vs)
    usable = [v for v in vs if v["score"] >= best_q - 0.25]
    target = max(usable, key=lambda v: v["expressiveness"])
    if style < 0.5:  # moderate: most expressive clip that is at least as lively as the chosen one
        cur = vs[active]
        mid = [v for v in usable if v["expressiveness"] >= cur.get("expressiveness", 0)]
        target = min(mid, key=lambda v: abs(v["expressiveness"] - (cur.get("expressiveness", 0) + 1) / 2)) if mid else target
    return target["index"]


# ------------------------------------------------------------------ mic match

BANDS = np.array([80 * 2 ** (i / 3) for i in range(24)])  # 80 Hz .. ~16 kHz, 1/3 octave


def ltas(a: np.ndarray, sr: int) -> list[float]:
    """Long-term average spectrum of voiced frames, in 1/3-octave bands (dB)."""
    n = 2048
    if len(a) < n * 4:
        return []
    frames = a[: len(a) // n * n].reshape(-1, n)
    lvl = 10 * np.log10(np.mean(frames**2, axis=1) + 1e-12)
    frames = frames[lvl > np.percentile(lvl, 90) - 20] * np.hanning(n)
    spec = np.mean(np.abs(np.fft.rfft(frames, axis=1)) ** 2, axis=0)
    freqs = np.fft.rfftfreq(n, 1 / sr)
    out = []
    for f in BANDS:
        m = (freqs >= f / 2 ** (1 / 6)) & (freqs < f * 2 ** (1 / 6))
        out.append(float(10 * np.log10(spec[m].mean() + 1e-12)) if m.any() and f < sr / 2 * 0.92 else float("nan"))
    return out


def mic_match(a: np.ndarray, sr: int, ref_bands: list[float], max_db: float = 6.0) -> tuple[np.ndarray, list[float]]:
    """Gently EQ the output so its tonal balance matches the source recording's mic/room.

    Level-neutral (mean difference removed), smoothed across bands and capped at
    +/-6 dB, so it corrects colour (boomy, thin, dull, bright) without changing the voice.
    """
    gen = np.array(ltas(a, sr))
    ref = np.array(ref_bands, dtype=float)
    if not len(gen) or len(ref) != len(gen):
        return a, []
    diff = ref - gen
    ok = np.isfinite(diff)
    if ok.sum() < 6:
        return a, []
    diff[ok] -= np.median(diff[ok])
    sm = diff.copy()
    for i in range(len(diff)):  # 3-band smoothing, ignore missing bands
        w = diff[max(0, i - 1) : i + 2]
        w = w[np.isfinite(w)]
        sm[i] = w.mean() if len(w) else 0.0
    sm = np.clip(np.nan_to_num(sm, nan=0.0), -max_db, max_db)
    # frequency-sampled linear-phase FIR
    nyq = sr / 2
    fpts = [0.0] + [min(f, nyq * 0.999) / nyq for f in BANDS if f < nyq * 0.999] + [1.0]
    gains_db = [sm[0]] + [g for f, g in zip(BANDS, sm) if f < nyq * 0.999] + [0.0]
    fpts, idx = np.unique(fpts, return_index=True)
    gains = 10 ** (np.array(gains_db)[idx] / 20)
    fir = firwin2(1025, fpts, gains)
    out = fftconvolve(a, fir, mode="same").astype(np.float32)
    return out, [round(float(g), 1) for g in sm]


def backfill(profile: dict, pdir) -> bool:
    """Give voices analyzed before delivery/mic features their tags and mic profile,
    without changing the user's chosen clip or transcript edits."""
    import json

    changed = False
    vs = profile.get("variants") or []
    if vs and "expressiveness" not in vs[0]:
        raw = []
        for v in vs:
            a, sr = au.load(pdir / v["file"])
            pro = prosody(a, sr)
            v["pitch_range"] = round(pro["pitch_range"], 1)
            raw.append(expressiveness(pro))
        ranks = np.array(raw).argsort().argsort() / max(1, len(raw) - 1)
        for v, r in zip(vs, ranks):
            v["expressiveness"] = round(float(r), 3) if len(vs) > 1 else 0.5
            v["delivery"] = "expressive" if r >= 0.67 else "calm" if r <= 0.33 else "balanced"
        changed = True
    if not profile.get("pitch_hz") and (pdir / "clean.wav").exists():
        a, sr = au.load(pdir / "clean.wav")  # the whole cleaned sample: the voice's own pitch level
        profile["pitch_hz"] = round(prosody(a, sr)["pitch_hz"], 1) or None
        changed = changed or bool(profile["pitch_hz"])
    mic = pdir / "mic.json"
    if not mic.exists():
        src = pdir / profile.get("original_file", "")
        if src.exists():
            tmp = pdir / "_mic_tmp.wav"
            au.decode_to_wav(src, tmp, 24000, 300)
            a, sr = au.load(tmp)
            tmp.unlink(missing_ok=True)
            mic.write_text(json.dumps({"sr": sr, "bands": ltas(a, sr)}))
            changed = True
    return changed
