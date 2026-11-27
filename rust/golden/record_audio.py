"""Record Python parity vectors for the Rust `audio` module (step S10, gate G1).

    HF_HUB_OFFLINE=1 backend/.venv/bin/python -B rust/golden/record_audio.py

Writes rust/golden/audio.json and the raw arrays under rust/golden/audio/:
every input signal and every output array as raw little-endian float32 (`.f32`), plus a few WAV files.
Signals are synthetic and seeded; the Rust tests read these files and never regenerate a signal.
Each decision case (frame index, cut sample, bool, count, gap list) stores `margin_db`: the smallest
distance in dB between any level it compares and the threshold used. The recorder fails when a
margin is under 0.01 dB, so no case depends on the last bit of a float.
Safety: the data folder is a fresh temporary folder (checked before any app import), Hugging Face is
offline, no model is imported, and ffmpeg is never run (audio.py is imported, not its ffmpeg helpers).
"""
from __future__ import annotations

import io
import json
import math
import os
import shutil
import struct
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
WT = HERE.parent.parent
BACKEND = WT / "backend"
OUT = HERE / "audio.json"
ARR = HERE / "audio"
MIN_MARGIN_DB = 0.01


# ---------------------------------------------------------------- storage helpers

np = None  # numpy, imported after the guards


def num(x):
    """A JSON number, or a string for -inf, inf and NaN."""
    x = float(x)
    if math.isnan(x):
        return "nan"
    if math.isinf(x):
        return "inf" if x > 0 else "-inf"
    return x


class Store:
    def __init__(self):
        self.files: set[str] = set()

    def put(self, name: str, a) -> dict:
        a = np.ascontiguousarray(np.asarray(a), dtype="<f4")
        fname = f"{name}.f32"
        assert fname not in self.files, fname
        self.files.add(fname)
        (ARR / fname).write_bytes(a.tobytes())
        return {"file": fname, "len": int(a.shape[0])}

    def put_bytes(self, fname: str, b: bytes) -> str:
        assert fname not in self.files, fname
        self.files.add(fname)
        (ARR / fname).write_bytes(b)
        return fname


def outcome(fn, *args, **kw):
    try:
        return True, fn(*args, **kw)
    except Exception as e:  # noqa: BLE001 - the type name is the recorded outcome
        return False, type(e).__name__


def margin(levels, *thresholds) -> float:
    """Smallest |level - threshold| in dB over all levels and thresholds (inf when there are none)."""
    lv = np.asarray(levels, dtype=np.float64).ravel()
    best = math.inf
    for t in thresholds:
        if lv.size:
            best = min(best, float(np.min(np.abs(lv - float(t)))))
    return best


KNIFE: set[str] = set()  # signals that produced a knife-edge case in this round


def check_margin(case: dict, m: float) -> dict:
    if m < MIN_MARGIN_DB:
        KNIFE.add(case.get("signal", "?"))
    case["margin_db"] = num(m)
    return case


# ---------------------------------------------------------------- synthetic signals


def harmonic_word(sr, dur, f0, rng, amp=0.3, attack=0.03, decay=0.08):
    """Amplitude-modulated harmonics with a soft onset and decay (a vowel-ish 'word')."""
    n = int(dur * sr)
    t = np.arange(n) / sr
    vib = 1 + 0.02 * np.sin(2 * np.pi * 5 * t)
    sig = np.zeros(n)
    for h in range(1, 9):
        sig += np.sin(2 * np.pi * h * f0 * vib * t + rng.uniform(0, 2 * np.pi)) / h
    env = 0.65 + 0.35 * np.sin(2 * np.pi * 4.0 * t + rng.uniform(0, np.pi)) ** 2
    env *= np.minimum(1.0, t / attack) ** 2
    env *= np.minimum(1.0, (dur - t) / decay) ** 2
    return amp * sig / np.max(np.abs(sig)) * env


def utterance(sr, parts, floor_db, seed, f0=150.0, amp=0.3):
    """parts: list of ('w', dur) words and ('p', dur) pauses, over a white noise floor (dBFS RMS)."""
    rng = np.random.default_rng(seed)
    out = []
    for kind, dur in parts:
        if kind == "w":
            out.append(harmonic_word(sr, dur, f0 * rng.uniform(0.9, 1.1), rng, amp))
        else:
            out.append(np.zeros(int(dur * sr)))
    a = np.concatenate(out)
    if floor_db is not None:
        a = a + rng.standard_normal(len(a)) * 10 ** (floor_db / 20)
    return a


def pink(n, rng):
    spec = np.fft.rfft(rng.standard_normal(n))
    f = np.arange(len(spec))
    spec[1:] /= np.sqrt(f[1:])
    spec[0] = 0
    p = np.fft.irfft(spec, n)
    return p / np.sqrt(np.mean(p**2))


def hiss(n, sr, rng):
    """Band-limited noise (about 4.5 to 9 kHz), like an 's'."""
    spec = np.fft.rfft(rng.standard_normal(n))
    f = np.fft.rfftfreq(n, 1 / sr)
    spec[(f < 4500) | (f > 9000)] = 0
    h = np.fft.irfft(spec, n)
    return h / np.sqrt(np.mean(h**2))


def bursts(sr, gap, seed):
    """Two 0.2 s tone bursts separated by `gap` seconds of digital silence, with 0.1 s edges."""
    rng = np.random.default_rng(seed)
    n = int(0.2 * sr)
    t = np.arange(n) / sr
    tone = 0.4 * np.sin(2 * np.pi * 330 * t + rng.uniform(0, 1)) * np.minimum(1, np.minimum(t, 0.2 - t) / 0.005)
    z = np.zeros(int(gap * sr))
    edge = np.zeros(int(0.1 * sr))
    return np.concatenate([edge, tone, z, tone, edge])


def make_signals(bumps: dict[str, int]):
    """name -> (float32 array, sample rate). A signal that gave a knife-edge case is redesigned by
    adding a seeded -85 dBFS dither (seed from its bump count); the recorder repeats until none is left."""
    s = {}
    R = 24000

    def add(name, a, sr):
        a = np.asarray(a, dtype=np.float64)
        b = bumps.get(name, 0)
        if b and len(a):
            a = a + np.random.default_rng(9000 + b).standard_normal(len(a)) * 10 ** (-85 / 20)
        s[name] = (np.asarray(a, dtype=np.float32), sr)

    t1 = np.arange(R // 2) / R
    add("tone440", 0.5 * np.sin(2 * np.pi * 440 * t1), R)
    for g in (0.1, 0.24, 0.26, 0.8):
        add(f"bursts_{int(g * 100):03d}", bursts(R, g, 7), R)
    phrase = [("p", 0.1), ("w", 0.22), ("p", 0.12), ("w", 0.2), ("p", 0.3), ("w", 0.25), ("p", 0.15)]
    add("speech_f70", utterance(R, phrase, -70, 101), R)
    add("speech_f45", utterance(R, phrase, -45, 102), R)
    add("speech_f25", utterance(R, phrase, -25, 103, amp=0.5), R)
    rng = np.random.default_rng(11)
    add("white_m20", rng.standard_normal(R // 2) * 10 ** (-20 / 20), R)
    add("white_m40", rng.standard_normal(R // 2) * 10 ** (-40 / 20), R)
    add("pink_m30", pink(R // 2, rng) * 10 ** (-30 / 20), R)
    add("silence", np.zeros(R // 2), R)
    dc = utterance(R, [("p", 0.1), ("w", 0.4), ("p", 0.3)], -60, 104) + 0.05
    add("dc_offset", dc, R)
    t = np.arange(int(0.6 * R)) / R
    clip = np.clip(1.5 * np.sin(2 * np.pi * 220 * t), -1.0, 1.0).astype(np.float32)
    clip[100:110] = np.float32(0.985)  # exactly on the 0.985 line (float32), compared as float32
    clip[200:205] = -np.float32(0.98500001)
    add("clipped", clip, R)

    # endings (24 kHz)
    rng = np.random.default_rng(21)
    w = harmonic_word(R, 0.5, 160, rng, 0.35, decay=0.01)
    cut = w.copy()
    cut[int(0.42 * R):] = 0  # model stops mid-phoneme: loud, then digital silence
    add("end_hard_cut", np.concatenate([np.zeros(int(0.1 * R)), cut, np.zeros(int(0.45 * R))]), R)
    w = harmonic_word(R, 0.5, 160, rng, 0.35, decay=0.02)
    tt = np.arange(int(0.25 * R)) / R
    tail = harmonic_word(R, 0.25, 160, rng, 0.35, attack=0.001, decay=0.001) * np.exp(-tt / 0.03)
    nat = np.concatenate([np.zeros(int(0.1 * R)), w, tail, np.zeros(int(0.4 * R))])
    add("end_natural", nat + rng.standard_normal(len(nat)) * 10 ** (-70 / 20), R)
    w = harmonic_word(R, 0.45, 170, rng, 0.35, decay=0.03)
    rel = hiss(int(0.03 * R), R, rng) * 0.05 * np.hanning(int(0.03 * R))
    stop = np.concatenate([np.zeros(int(0.1 * R)), w, np.zeros(int(0.07 * R)), rel, np.zeros(int(0.4 * R))])
    add("end_stop_release", stop + rng.standard_normal(len(stop)) * 10 ** (-72 / 20), R)
    w = harmonic_word(R, 0.5, 150, rng, 0.35)
    blob = harmonic_word(R, 0.15, 210, rng, 0.3, attack=0.01, decay=0.02)
    stray = np.concatenate([np.zeros(int(0.1 * R)), w, np.zeros(int(0.35 * R)), blob, np.zeros(int(0.3 * R))])
    add("end_stray", stray + rng.standard_normal(len(stray)) * 10 ** (-70 / 20), R)

    # lead bursts (24 kHz): hiss, a clear pause, then the word; and hiss that runs into the word
    rng = np.random.default_rng(31)
    h = hiss(int(0.1 * R), R, rng) * 0.08 * np.minimum(1, np.minimum(np.arange(int(0.1 * R)), int(0.1 * R) - np.arange(int(0.1 * R))) / 48)
    w = harmonic_word(R, 0.6, 150, rng, 0.3)
    lb = np.concatenate([np.zeros(int(0.05 * R)), h, np.zeros(int(0.25 * R)), w, np.zeros(int(0.2 * R))])
    add("lead_hiss_pause", lb + rng.standard_normal(len(lb)) * 10 ** (-65 / 20), R)
    w = harmonic_word(R, 0.6, 150, rng, 0.3)
    li = np.concatenate([np.zeros(int(0.05 * R)), h, w, np.zeros(int(0.3 * R))])
    add("lead_hiss_into", li + rng.standard_normal(len(li)) * 10 ** (-65 / 20), R)
    add("short_150ms", utterance(R, [("w", 0.15)], -60, 32), R)

    # other rates
    p2 = [("p", 0.08), ("w", 0.25), ("p", 0.27), ("w", 0.2), ("p", 0.1)]
    for sr in (16000, 22050, 44100, 48000):
        add(f"speech_{sr}", utterance(sr, p2, -50, 200 + sr // 1000), sr)
    add("lead_hiss_16000", np.concatenate([np.zeros(800), hiss(1600, 16000, rng) * 0.08, np.zeros(4000),
                                           harmonic_word(16000, 0.5, 150, rng, 0.3), np.zeros(3200)]), 16000)

    # loudness vectors: quiet, loud, gapped at 16000, 24000 and 48000 (0.8 to 1.6 s)
    for sr in (16000, 24000, 48000):
        base = [("p", 0.05), ("w", 0.35), ("p", 0.1), ("w", 0.3), ("p", 0.05)]
        add(f"loud_quiet_{sr}", utterance(sr, base, -80, 300 + sr // 1000, amp=0.01), sr)
        add(f"loud_loud_{sr}", utterance(sr, base, -60, 310 + sr // 1000, amp=0.9), sr)
        gapped = [("w", 0.2), ("p", 0.45), ("w", 0.15), ("p", 0.4), ("w", 0.05)]
        add(f"loud_gapped_{sr}", utterance(sr, gapped, -75, 320 + sr // 1000, amp=0.3), sr)
    add("loud_short_045", utterance(24000, [("w", 0.45)], -60, 330), 24000)
    add("loud_silence_06", np.zeros(int(0.6 * 24000)), 24000)

    # tiny shapes
    add("empty", np.zeros(0), R)
    add("one_sample", np.array([0.25]), R)
    add("frag", utterance(R, [("p", 0.03), ("w", 0.14), ("p", 0.03)], -60, 34), R)
    add("short_100", np.sin(np.arange(100) / 3.0) * 0.3, R)
    add("short_15", np.sin(np.arange(15) / 2.0) * 0.3, R)
    add("short_16", np.sin(np.arange(16) / 2.0) * 0.3, R)
    add("short_27", np.sin(np.arange(27) / 2.0) * 0.3, R)
    add("short_28", np.sin(np.arange(28) / 2.0) * 0.3, R)
    add("one_frame", np.sin(np.arange(480) / 5.0) * 0.3, R)
    add("frames_4", utterance(R, [("w", 0.08)], -60, 33), R)
    add("loud_edges", np.concatenate([np.full(300, 0.4), np.zeros(2400), np.full(1000, -0.3)]), R)

    # limiter inputs
    add("hot", np.concatenate([np.linspace(-1.6, 1.6, 4001), 0.95 * np.sin(np.arange(4000) / 7.0)]), R)
    return s


# ---------------------------------------------------------------- recording


def main() -> None:
    global np
    assert os.environ.get("HF_HUB_OFFLINE") == "1", "run with HF_HUB_OFFLINE=1"
    tmp = Path(tempfile.mkdtemp(prefix="taketwo-golden."))
    try:
        os.environ["TAKETWO_DATA"] = os.environ["VOICEFORGE_DATA"] = str(tmp)
        os.environ["TRANSFORMERS_OFFLINE"] = "1"
        sys.path.insert(0, str(BACKEND))
        from app import config

        assert config.DATA_DIR.resolve().is_relative_to(tmp.resolve()), config.DATA_DIR  # never the real library
        import numpy
        import scipy.signal as ss
        import soundfile as sf

        np = numpy
        from app import audio

        def no_ffmpeg(*a, **k):
            raise RuntimeError("record_audio.py must never run ffmpeg")

        audio.subprocess = type("NoSubprocess", (), {"run": staticmethod(no_ffmpeg)})
        assert "app.ml" not in sys.modules and "mlx" not in sys.modules, "no model import"

        bumps: dict[str, int] = {}
        for _round in range(60):
            KNIFE.clear()
            if ARR.exists():
                shutil.rmtree(ARR)
            ARR.mkdir()
            st = Store()
            sig = make_signals(bumps)
            data = record_all(audio, ss, sf, st, sig, tmp, bumps)
            if not KNIFE:
                break
            assert "?" not in KNIFE, "a knife edge without a signal name"
            for name in KNIFE:
                bumps[name] = bumps.get(name, 0) + 1
            print("redesign", sorted(KNIFE))
        assert not KNIFE, f"knife edges left: {sorted(KNIFE)}"

        total = sum((ARR / f).stat().st_size for f in st.files)
        assert total <= 7_700_000, total  # leaves room for file-system blocks under the 8 MB (du -sk) limit
        OUT.write_text(json.dumps(data, ensure_ascii=True, indent=1, allow_nan=False) + "\n")
        counts = {k: len(v) for k, v in data.items() if isinstance(v, list)}
        margins = [c["margin_db"] for v in data.values() if isinstance(v, list) for c in v
                   if isinstance(c, dict) and isinstance(c.get("margin_db"), float)]
        print(counts)
        print(f"files {len(st.files)}, {total} bytes; smallest knife-edge margin {min(margins):.4f} dB")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def record_all(audio, ss, sf, st, sig, tmp, bumps):
    data = {"note": "Recorded by rust/golden/record_audio.py from backend/app/audio.py. Arrays are raw little-endian float32 files in rust/golden/audio/.",
            "versions": {"numpy": np.__version__, "scipy": __import__("scipy").__version__,
                         "pyloudnorm": __import__("importlib.metadata", fromlist=["x"]).version("pyloudnorm"), "soundfile": sf.__version__},
            "min_margin_db": MIN_MARGIN_DB,
            "redesigned": dict(sorted(bumps.items())),
            "signals": {}}
    for name, (a, sr) in sig.items():
        data["signals"][name] = {**st.put(f"sig_{name}", a), "sr": sr}
    record_dtypes(data, audio, *sig["speech_f45"])
    record_frame_db(data, audio, st, sig)
    record_thresholds(data, audio, st, sig)
    record_silence(data, audio, st, sig)
    record_trim(data, audio, st, sig)
    record_shaping(data, audio, st, sig)
    record_resample(data, audio, st, sig)
    record_filters(data, audio, ss, st, sig)
    record_loudness(data, audio, st, sig)
    record_bursts_snr(data, audio, sig)
    record_regions(data, audio, sig)
    record_lead_burst(data, audio, ss, st, sig)
    record_wav(data, audio, sf, st, sig, tmp)
    return data


def record_dtypes(data, audio, a, sr):
    db = audio.frame_db(a, sr)
    p10, p90 = np.percentile(db, 10), np.percentile(db, 90)
    data["dtypes"] = {
        "frame_db": str(db.dtype),
        "percentile": type(p10).__name__,
        "speech_threshold_mix": type(p10 + 0.35 * (p90 - p10)).__name__,
        "floor_threshold_raw": type(p90 - 48.0).__name__,
        "frame_db_10ms": str(audio.frame_db(a, sr, 10).dtype),
        "resample": str(audio.resample(a, sr, 16000).dtype),
        "highpass": str(audio.highpass(a, sr).dtype),
        "sosfiltfilt_raw": str(__import__("scipy.signal", fromlist=["x"]).sosfiltfilt(
            __import__("scipy.signal", fromlist=["x"]).butter(4, 60.0, btype="highpass", fs=sr, output="sos"), a).dtype),
        "soft_limit": str(audio.soft_limit(a, 0.5).dtype),
        "fade": str(audio.fade(a, sr, 6, 25).dtype),
        "normalize_lufs": str(audio.normalize_lufs(a, sr, -16.0).dtype),
        "loudness": type(audio.loudness(a, sr)).__name__,
        "clipping_ratio": type(audio.clipping_ratio(a)).__name__,
        "noise_bursts": type(audio.noise_bursts(a, sr)).__name__,
        "estimate_snr": type(audio.estimate_snr(a, sr)).__name__,
        "end_analysis_last_level": type(audio.end_analysis(a, sr, 1.5)["last_level"]).__name__,
        "lead_burst_full_level": "float32",
        "lead_burst_high_level": "float64",
    }


def record_frame_db(data, audio, st, sig):
    cases = []
    names = ["tone440", "speech_f45", "speech_f25", "silence", "dc_offset", "bursts_026", "end_natural",
             "speech_16000", "speech_22050", "speech_44100", "speech_48000", "empty", "short_100", "one_frame", "one_sample"]
    for name in names:
        a, sr = sig[name]
        for ms in (20, 10):
            out = audio.frame_db(a, sr, ms)
            cases.append({"signal": name, "sr": sr, "frame_ms": ms, "out": st.put(f"out_frame_db_{name}_{ms}", out)})
    data["frame_db"] = cases


def record_thresholds(data, audio, st, sig):
    """speech_threshold_db and _floor_threshold, fed Python's own frame_db arrays."""
    cases = []
    for c in data["frame_db"]:
        db = np.fromfile(ARR / c["out"]["file"], dtype="<f4")
        ok, thr = outcome(audio.speech_threshold_db, db)
        ok2, flo = outcome(audio._floor_threshold, db)
        cases.append({"db": c["out"], "speech_threshold_db": num(thr) if ok else {"error": thr},
                      "floor_threshold": num(flo) if ok2 else {"error": flo}})
    db = np.zeros(0, dtype=np.float32)
    st.put("db_empty", db)
    ok, thr = outcome(audio.speech_threshold_db, db)
    ok2, flo = outcome(audio._floor_threshold, db)
    assert not ok and not ok2
    cases.append({"db": {"file": "db_empty.f32", "len": 0}, "speech_threshold_db": {"error": thr}, "floor_threshold": {"error": flo}})
    data["thresholds"] = cases


def record_silence(data, audio, st, sig):
    cases = []
    names = ["tone440", "bursts_010", "bursts_024", "bursts_026", "bursts_080", "speech_f70", "speech_f45", "speech_f25",
             "silence", "dc_offset", "end_stray", "speech_16000", "speech_22050", "speech_44100", "speech_48000",
             "empty", "short_100", "one_frame", "white_m40"]
    for name in names:
        a, sr = sig[name]
        for min_gap in (0.25, 0.1):
            r = audio.silence_report(a, sr, min_gap)
            db = audio.frame_db(a, sr)
            thr = audio.speech_threshold_db(db)
            case = {"signal": name, "min_gap": min_gap,
                    "lead": r.lead, "tail": r.tail, "max_internal": r.max_internal,
                    "internal_gaps": [[float(s), float(e)] for s, e in r.internal_gaps], "speech_ratio": r.speech_ratio}
            cases.append(check_margin(case, margin(db, thr)))
    data["silence_report"] = cases

    cases = []
    for name in ["speech_f70", "speech_f45", "speech_f25", "end_natural", "end_hard_cut", "end_stop_release", "tone440",
                 "silence", "frames_4", "short_100", "empty", "dc_offset", "speech_48000", "speech_22050"]:
        a, sr = sig[name]
        db = audio.frame_db(a, sr)
        v = audio.ending_level(a, sr)
        m = margin(db, np.percentile(db, 90) - 60) if len(db) >= 5 else math.inf
        cases.append(check_margin({"signal": name, "ending_level": num(v)}, m))
    data["ending_level"] = cases


def record_trim(data, audio, st, sig):
    cases = []
    for name in ["speech_f70", "speech_f45", "speech_f25", "end_natural", "end_stray", "lead_hiss_pause", "tone440",
                 "silence", "short_100", "empty", "one_frame", "loud_edges", "speech_44100", "speech_22050"]:
        a, sr = sig[name]
        for keep_ms, tail_ms in ((60, 220), (0, 0), (300, 500)):
            out = audio.trim_edges(a, sr, keep_ms, tail_ms)
            # trim_edges returns a slice of its input: record the slice bounds (exact) instead of the samples
            lo = 0 if out is a or len(out) == 0 else (out.__array_interface__["data"][0] - a.__array_interface__["data"][0]) // 4
            assert np.array_equal(out, a[lo: lo + len(out)])
            db = audio.frame_db(a, sr)
            m = margin(db, audio._floor_threshold(db)) if len(db) >= 3 else math.inf
            cases.append(check_margin({"signal": name, "keep_ms": keep_ms, "tail_ms": tail_ms,
                                       "start": int(lo), "len": int(len(out))}, m))
    data["trim_edges"] = cases

    cases = []
    for name in ["speech_f70", "speech_f45", "speech_f25", "end_natural", "lead_hiss_pause", "tone440", "silence",
                 "short_100", "empty", "loud_edges", "speech_16000", "speech_48000"]:
        a, sr = sig[name]
        for keep_ms in (100, 0, 30):
            v = audio.speech_start(a, sr, keep_ms)
            db = audio.frame_db(a, sr, 10)
            cases.append(check_margin({"signal": name, "keep_ms": keep_ms, "speech_start": v},
                                      margin(db, audio._floor_threshold(db))))
    data["speech_start"] = cases


def record_shaping(data, audio, st, sig):
    # fade
    cases = []
    specs = [("frag", 24000, 0, 0), ("frag", 24000, 8, 0), ("frag", 24000, 0, 8), ("frag", 24000, 6, 25), ("frag", 16000, 30, 90),
             ("short_15", 24000, 6, 25), ("one_sample", 24000, 6, 25), ("empty", 24000, 6, 25),
             ("short_100", 1000, 1, 0), ("short_100", 1000, 0, 1), ("short_100", 1000, 1, 1), ("short_100", 1000, 2, 3),
             ("short_100", 24000, 0, 0), ("frag", 22050, 13, 7), ("frag", 44100, 100, 250)]
    for i, (name, sr, fi, fo) in enumerate(specs):
        out = audio.fade(A(sig, name), sr, fi, fo)
        n_in = min(len(A(sig, name)), int(sr * fi / 1000))
        n_out = min(len(A(sig, name)), int(sr * fo / 1000))
        cases.append({"signal": name, "sr": sr, "fade_in_ms": fi, "fade_out_ms": fo, "n_in": n_in, "n_out": n_out,
                      "out": st.put(f"out_fade_{i:02d}", out)})
    assert {c["n_in"] for c in cases} >= {0, 1} and {c["n_out"] for c in cases} >= {0, 1}
    data["fade"] = cases

    # soft_limit
    cases = []
    for i, (name, ceiling) in enumerate([("hot", 10 ** (-1 / 20)), ("hot", 1.0), ("hot", 0.5), ("frag", 0.2), ("empty", 0.9), ("tone440", 0.9)]):
        out = audio.soft_limit(A(sig, name), ceiling)
        cases.append({"signal": name, "ceiling": ceiling, "out": st.put(f"out_soft_limit_{i}", out)})
    data["soft_limit"] = cases

    # clipping_ratio
    data["clipping_ratio"] = [{"signal": n, "clipping_ratio": audio.clipping_ratio(A(sig, n))}
                              for n in ["clipped", "hot", "tone440", "silence", "empty", "one_sample"]]

    # shorten_gaps: gaps from Python's silence_report, and hand-made ones
    cases = []
    specs = []
    for name in ["speech_f45", "bursts_080"]:
        a, sr = sig[name]
        gaps = audio.silence_report(a, sr).internal_gaps
        for mg in ((0.2, 0.5) if name == "speech_f45" else (0.2,)):
            specs.append((name, [list(map(float, g)) for g in gaps], mg))
    specs += [("speech_f45", [], 0.3), ("speech_f45", [[0.1, 0.2], [0.5, 0.55]], 0.3),  # no gap over the limit
              ("tone440", [[0.0, 0.4], [0.1, 0.3]], 0.2), ("tone440", [[0.45, 0.9]], 0.1),  # overlapping, past the end
              ("short_100", [[0.0, 0.003]], 0.001)]
    for i, (name, gaps, mg) in enumerate(specs):
        out = audio.shorten_gaps(A(sig, name), SR(sig, name), [tuple(g) for g in gaps], mg)
        cases.append({"signal": name, "sr": SR(sig, name), "gaps": gaps, "max_gap": mg, "out": st.put(f"out_shorten_{i:02d}", out)})
    data["shorten_gaps"] = cases

    # concat_with_pauses
    cases = []
    q = np.zeros(2400, dtype=np.float32)
    pad = lambda x: np.concatenate([q, x, q]).astype(np.float32)  # noqa: E731
    wa = A(sig, "speech_f45")[3600:9000]  # starts and ends inside sound
    wb = A(sig, "tone440")[:6000]
    chunk_sets = [
        ("none", [], [], 24000),
        ("one_padded", [pad(wa)], [], 24000),
        ("padded", [pad(wa), pad(wb), pad(wa)], [300, 650], 24000),
        ("sound_edges", [wa, wb, wa], [120, 0], 24000),
        ("mixed", [pad(wa), wb, np.zeros(0, np.float32), A(sig, "short_100")], [200, 200, 50], 24000),
        ("rate_22050", [pad(wa), pad(wb)], [333], 22050),
        ("tiny_quiet", [np.full(10, 1e-7, np.float32), np.zeros(200, np.float32)], [7], 24000),
    ]
    for name, chunks, pauses, sr in chunk_sets:
        ins = [st.put(f"in_concat_{name}_{k}", c) for k, c in enumerate(chunks)]
        out = audio.concat_with_pauses(chunks, pauses, sr)
        cases.append({"name": name, "chunks": ins, "pauses_ms": pauses, "sr": sr, "out": st.put(f"out_concat_{name}", out)})
    data["concat_with_pauses"] = cases


def A(sig, name):
    return sig[name][0]


def SR(sig, name):
    return sig[name][1]


def record_resample(data, audio, st, sig):
    cases = []
    for name, to in [("speech_f45", 16000), ("speech_44100", 24000), ("speech_48000", 24000), ("speech_22050", 24000),
                     ("speech_16000", 24000), ("speech_f45", 24000), ("tone440", 16000), ("white_m20", 16000),
                     ("short_15", 16000), ("one_sample", 16000), ("short_100", 22050)]:
        a, sr = sig[name]
        ok, out = outcome(audio.resample, a, sr, to)
        c = {"signal": name, "sr_from": sr, "sr_to": to}
        c["out"] = st.put(f"out_resample_{name}_{to}", out) if ok else {"error": out}
        cases.append(c)
    ok, out = outcome(audio.resample, A(sig, "empty"), 24000, 16000)
    cases.append({"signal": "empty", "sr_from": 24000, "sr_to": 16000, "out": st.put("out_resample_empty", out) if ok else {"error": out}})
    data["resample"] = cases


def record_filters(data, audio, ss, st, sig):
    sos_cases = []
    for sr in (16000, 22050, 24000, 44100, 48000):
        for hz in (60.0, 50.0):
            sos = ss.butter(4, hz, btype="highpass", fs=sr, output="sos")
            sos_cases.append({"kind": "highpass", "sr": sr, "hz": [hz], "sos": sos.tolist()})
        band = [4000, min(10000, sr / 2 * 0.95)]
        sos = ss.butter(4, band, btype="band", fs=sr, output="sos")
        sos_cases.append({"kind": "band", "sr": sr, "hz": [float(x) for x in band], "sos": sos.tolist()})
    data["butter"] = sos_cases

    cases = []
    for name, hz in [("speech_f45", 60.0), ("frag", 50.0), ("white_m20", 60.0),
                     ("speech_16000", 60.0), ("speech_48000", 50.0), ("speech_22050", 60.0),
                     ("short_15", 60.0), ("short_16", 60.0), ("empty", 60.0)]:
        a, sr = sig[name]
        ok, out = outcome(audio.highpass, a, sr, hz)
        c = {"signal": name, "hz": hz}
        c["out"] = st.put(f"out_highpass_{name}_{int(hz)}", out) if ok else {"error": out}
        cases.append(c)
    assert sum("error" in c["out"] for c in cases) >= 2
    data["highpass"] = cases

    cases = []  # the band-pass sosfiltfilt inside lead_burst (float64 output, stored as float32)
    for name in ["lead_hiss_pause", "lead_hiss_16000", "short_27", "short_28"]:
        a, sr = sig[name]
        sos = ss.butter(4, [4000, min(10000, sr / 2 * 0.95)], btype="band", fs=sr, output="sos")
        ok, out = outcome(ss.sosfiltfilt, sos, a)
        c = {"signal": name}
        c["out"] = st.put(f"out_band_{name}", out) if ok else {"error": out}
        cases.append(c)
    data["band_sosfiltfilt"] = cases


def record_loudness(data, audio, st, sig):
    cases = []
    names = [n for n in sig if n.startswith("loud_")] + ["speech_f45", "silence", "short_100", "empty"]
    for name in names:
        a, sr = sig[name]
        cases.append({"signal": name, "loudness": num(audio.loudness(a, sr))})
    data["loudness"] = cases

    cases = []
    for i, (name, target, peak) in enumerate([("loud_quiet_24000", -16.0, -1.0), ("loud_loud_24000", -16.0, -1.0),
                                              ("loud_quiet_16000", -12.0, -1.0), ("loud_gapped_16000", -20.0, -3.0),
                                              ("loud_loud_16000", -23.0, -1.0), ("loud_short_045", -16.0, -1.0),
                                              ("loud_silence_06", -16.0, -1.0), ("frag", -10.0, -0.5)]):
        a, sr = sig[name]
        cur = audio.loudness(a, sr)
        out = audio.normalize_lufs(a, sr, target, peak)
        peak_val = float(np.max(np.abs(a * (10 ** ((target - cur) / 20))))) if np.isfinite(cur) else None
        cases.append({"signal": name, "target": target, "peak_db": peak, "python_loudness": num(cur),
                      "limited": bool(peak_val is not None and peak_val > 10 ** (peak / 20)),
                      "out": st.put(f"out_normalize_{i}", out)})
    assert any(c["limited"] for c in cases) and not all(c["limited"] for c in cases)
    data["normalize_lufs"] = cases


def record_bursts_snr(data, audio, sig):
    cases = []
    for name in ["speech_f70", "speech_f45", "speech_f25", "white_m20", "white_m40", "pink_m30", "silence", "tone440",
                 "lead_hiss_pause", "dc_offset", "bursts_026", "speech_16000", "speech_22050", "speech_44100",
                 "speech_48000", "frames_4", "short_100", "empty"]:
        a, sr = sig[name]
        v = audio.noise_bursts(a, sr)
        n = int(0.03 * sr)
        m_db, m_flat = math.inf, math.inf
        if len(a) >= n * 4:
            frames = a[: len(a) // n * n].reshape(-1, n) * np.hanning(n)
            spec = np.abs(np.fft.rfft(frames, axis=1)) ** 2 + 1e-12
            flat = np.exp(np.mean(np.log(spec), axis=1)) / np.mean(spec, axis=1)
            db = 10 * np.log10(np.mean(spec, axis=1))
            m_db = margin(db, np.percentile(db, 90) - 25)
            m_flat = margin(10 * np.log10(flat), 10 * np.log10(0.1))  # flatness 0.1 as a dB distance
        cases.append(check_margin({"signal": name, "noise_bursts": v, "margin_flat_db": num(m_flat)}, min(m_db, m_flat)))
    data["noise_bursts"] = cases
    data["estimate_snr"] = [{"signal": n, "estimate_snr": audio.estimate_snr(*sig[n])}
                            for n in ["speech_f70", "speech_f45", "speech_f25", "white_m20", "silence", "tone440", "dc_offset",
                                      "speech_16000", "speech_48000", "short_100", "empty"]]


def regions_margin(audio, a, sr, bridge_ms=130):
    db = audio.frame_db(a, sr, 10)
    sp = float(np.percentile(db, 90))
    thr = float(min(np.percentile(db, 10) + 10.0, sp - 20.0))
    return db, sp, margin(db, thr)


def record_regions(data, audio, sig):
    names = ["speech_f70", "speech_f45", "speech_f25", "end_hard_cut", "end_natural", "end_stop_release", "end_stray",
             "bursts_010", "bursts_024", "bursts_080", "tone440", "silence", "dc_offset", "lead_hiss_pause",
             "speech_16000", "speech_22050", "speech_44100", "speech_48000", "short_100", "empty", "one_sample"]
    cases = []
    for name in names:
        a, sr = sig[name]
        for bridge in (130, 60, 5):
            regions, db, sp = audio.sound_regions(a, sr, bridge)
            _, _, m = regions_margin(audio, a, sr, bridge)
            cases.append(check_margin({"signal": name, "bridge_ms": bridge, "regions": [list(map(int, r)) for r in regions],
                                       "n_db": int(len(db)), "speech_level": sp}, m))
    data["sound_regions"] = cases

    cases = []
    for name in names:
        a, sr = sig[name]
        lead, tail = audio.edge_silence(a, sr)
        _, _, m = regions_margin(audio, a, sr, 60)
        cases.append(check_margin({"signal": name, "lead": lead, "tail": tail}, m))
    data["edge_silence"] = cases

    # end_analysis, with word ends before, at and after the sound, and past the end of the file
    cases = []
    for name in names:
        a, sr = sig[name]
        dur = len(a) / sr
        lwes = sorted({0.0, round(dur * 0.3, 3), round(dur * 0.55, 3), round(dur * 0.7, 3), round(dur * 0.85, 3),
                       round(dur + 0.4, 3), 0.85})
        for lwe in lwes:
            for tail_s in (0.12, 0.0, 0.3):
                r = audio.end_analysis(a, sr, lwe, tail_s)
                cases.append(check_margin({"signal": name, "last_word_end": lwe, "tail_s": tail_s,
                                           **{k: (num(v) if isinstance(v, float) else v) for k, v in r.items()}},
                                          end_margin(audio, a, sr, lwe)))
    data["end_analysis"] = cases

    # _terminal_end on Python's own 10 ms levels
    cases = []
    for name in ["speech_f45", "end_stray", "end_stop_release", "silence", "empty"]:
        a, sr = sig[name]
        db = audio.frame_db(a, sr, 10)
        sp = float(np.percentile(db, 90))
        for scan0 in (-5, 0, len(db) // 3, len(db) - 1, len(db) + 3):
            e = audio._terminal_end(db, scan0, sp)
            cases.append(check_margin({"signal": name, "scan0": int(scan0), "speech_level": sp, "end": e},
                                      margin(db, sp - 48.0)))
    data["terminal_end"] = cases


def end_margin(audio, a, sr, lwe):
    """All the dB lines end_analysis compares against, measured on Python's own levels."""
    regions, db, sp = audio.sound_regions(a, sr)
    _, _, m = regions_margin(audio, a, sr)
    if not regions:
        return m
    m = min(m, margin(db, sp - 48.0))
    for s1, e1 in regions:
        if e1 - s1 >= 8:
            m = min(m, abs(float(np.percentile(db[s1:e1], 90) - sp) + 27.0))
    # the release judgement (tail max against sp - 30) and the natural line (-24 dB)
    lw = int(max(0.0, lwe - 0.05) / 0.01)
    idx = next((k for k, (s0, e0) in enumerate(regions) if e0 >= lw), len(regions) - 1)
    s0, e0 = regions[idx]
    end = audio._terminal_end(db, max(s0, lw - 15), sp)
    end = e0 if end is None else end
    audible = np.flatnonzero(db > sp - 48.0)
    if len(audible) and int(audible[-1]) + 1 > end:
        m = min(m, abs(float(db[end: int(audible[-1]) + 1].max()) - (sp - 30.0)))
    last = audio.end_analysis(a, sr, lwe)["last_level"]
    if np.isfinite(last):
        m = min(m, abs(last + 24.0))
    return m


def record_lead_burst(data, audio, ss, st, sig):
    cases = []
    for name in ["lead_hiss_pause", "lead_hiss_into", "lead_hiss_16000", "speech_f45", "speech_f25", "silence", "tone440",
                 "bursts_024", "short_150ms", "short_100", "empty", "end_stray", "white_m20", "speech_48000"]:
        a, sr = sig[name]
        r = audio.lead_burst(a, sr)
        cases.append(check_margin({"signal": name, "result": None if r is None else [float(r[0]), float(r[1])]},
                                  lead_margin(audio, ss, a, sr)))
    assert any(c["result"] for c in cases) and any(c["result"] is None for c in cases)
    data["lead_burst"] = cases


def lead_margin(audio, ss, a, sr):
    n = int(0.01 * sr)
    m = min(len(a), int(1.2 * sr)) // n
    if m < 20:
        return math.inf
    level = lambda x: 20 * np.log10(np.sqrt(np.mean(x[: m * n].reshape(m, n) ** 2, axis=1)) + 1e-9)  # noqa: E731
    full = level(a)
    high = level(ss.sosfiltfilt(ss.butter(4, [4000, min(10000, sr / 2 * 0.95)], btype="band", fs=sr, output="sos"), a))
    line = np.percentile(audio.frame_db(a, sr, 10), 90) - 30
    out = margin(full, line)
    loud = full > line
    if loud.any():
        start = int(np.argmax(loud))
        end = start + int(np.argmin(loud[start:])) if not loud[start:].all() else m
        if end > start:
            out = min(out, abs(float(np.mean(high[start:end] - full[start:end])) + 5.0))
    return out


def record_wav(data, audio, sf, st, sig, tmp):
    # save: a ramp with exactly +1 and -1, values beyond them, and half-LSB values
    k = np.arange(-40, 40)
    vals = np.concatenate([[1.0, -1.0, 1.5, -1.5, 2.0, -7.0, 0.999999, -0.999999, 0.0, -0.0],
                           (k + 0.5) / 32768, k / 32768, (32767 - np.arange(5) + 0.5) / 32768,
                           -(32767 - np.arange(5) + 0.5) / 32768, np.linspace(-1.2, 1.2, 501)]).astype(np.float32)
    ramp = st.put("in_save_ramp", vals)
    cases = []
    for name, a, sr in [("ramp", vals, 24000), ("speech_f25", *sig["speech_f25"]), ("empty", A(sig, "empty"), 16000)]:
        p = tmp / f"save_{name}.wav"
        audio.save(p, a, sr)
        cases.append({"input": ramp if name == "ramp" else data["signals"][name], "sr": sr,
                      "expected": st.put_bytes(f"save_{name}.wav", p.read_bytes())})
    data["save"] = cases

    # load: mono and stereo PCM16, float32 WAV, and a rate change
    rng = np.random.default_rng(41)
    mono = (A(sig, "speech_f45")[2400:7200]).astype(np.float32)
    stereo = np.stack([mono, (rng.standard_normal(len(mono)) * 0.1).astype(np.float32)], axis=1)
    files = []
    for name, arr, sr, sub in [("mono16", mono, 24000, "PCM_16"), ("stereo16", stereo, 24000, "PCM_16"),
                               ("float32", mono * 1.3, 24000, "FLOAT"), ("stereo_float32", stereo, 22050, "FLOAT"),
                               ("mono16_16k", A(sig, "speech_16000")[:4800], 16000, "PCM_16")]:
        if sub == "FLOAT":
            # by hand: libsndfile puts a time stamp (PEAK chunk) in float WAVs; this one carries a LIST chunk as ffmpeg's do
            body = np.ascontiguousarray(arr, dtype="<f4").tobytes()
            ch = 1 if arr.ndim == 1 else arr.shape[1]
            fmt = struct.pack("<HHIIHH", 3, ch, sr, sr * ch * 4, ch * 4, 32)
            lst = b"INFOISFT\x0e\x00\x00\x00Lavf61.7.100\x00\x00"
            chunks = b"fmt " + struct.pack("<I", 16) + fmt + b"LIST" + struct.pack("<I", len(lst)) + lst
            chunks += b"data" + struct.pack("<I", len(body)) + body
            wav = b"RIFF" + struct.pack("<I", 4 + len(chunks)) + b"WAVE" + chunks
        else:
            bio = io.BytesIO()
            sf.write(bio, arr, sr, subtype=sub, format="WAV")
            wav = bio.getvalue()
        files.append((name, st.put_bytes(f"load_{name}.wav", wav)))
    cases = []
    for name, fname in files:
        for sr in (None, 24000, 16000):
            a, got_sr = audio.load(ARR / fname, sr)
            cases.append({"file": fname, "sr": sr, "out_sr": got_sr, "out": st.put(f"out_load_{name}_{sr}", a)})
    data["load"] = cases


if __name__ == "__main__":
    main()
