"""Record Python parity vectors for mic match, mastering and the final-mix edge check (step S11b, gate G1).

    HF_HUB_OFFLINE=1 backend/.venv/bin/python -B rust/golden/record_mastering.py

Writes rust/golden/mastering.json and raw arrays under rust/golden/mastering/ (`.f32` little-endian float32
signals and outputs, `.f64` for firwin2 taps). Functions: delivery.ltas, delivery.mic_match (and the firwin2
taps inside it), studio.expander, studio.de_ess, studio.master_chain, pipeline._processed_edge_issues.
Signals are synthetic and seeded; the Rust tests read these files and never regenerate a signal.
No bare NaN is written: a missing band is `null`, other non-finite numbers are "nan"/"inf"/"-inf" strings.
Knife edges: every frame-level mask a case depends on records `margin_db` (distance of any level to its threshold)
and must be at least 0.01 dB; de_ess's per-sample loud mask must be at least 1e-6 dB (see MIN_SAMPLE_MARGIN_DB);
every mic_match curve value records its distance to a rounding tie (>= 1e-6 dB).
Safety: the data folder is a fresh temporary folder (checked before any app import), Hugging Face is offline,
no model library is loaded (pipeline imports the lazy app.ml, which loads none), and ffmpeg can never run.
"""
from __future__ import annotations

import json
import math
import os
import shutil
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
WT = HERE.parent.parent
BACKEND = WT / "backend"
OUT = HERE / "mastering.json"
ARR = HERE / "mastering"
MIN_MARGIN_DB = 0.01
MIN_TIE_DB = 1e-6
MAX_BYTES = 5_600_000  # du -sk counts blocks; the gate is 6144 KB

np = None  # numpy, imported after the guards


def num(x):
    x = float(x)
    if math.isnan(x):
        return "nan"
    if math.isinf(x):
        return "inf" if x > 0 else "-inf"
    return x


def band(x):
    """A band level: null for NaN (a band Python marks missing)."""
    x = float(x)
    return None if math.isnan(x) else x


class Store:
    def __init__(self):
        self.files: dict[str, int] = {}
        self.signals: dict[str, dict] = {}

    def _put(self, fname, b):
        assert fname not in self.files, fname
        self.files[fname] = len(b)
        (ARR / fname).write_bytes(b)

    def put(self, name, a) -> dict:
        a = np.ascontiguousarray(np.asarray(a), dtype="<f4")
        self._put(f"{name}.f32", a.tobytes())
        return {"file": f"{name}.f32", "len": int(a.shape[0])}

    def put64(self, name, a) -> dict:
        a = np.ascontiguousarray(np.asarray(a), dtype="<f8")
        self._put(f"{name}.f64", a.tobytes())
        return {"file": f"{name}.f64", "len": int(a.shape[0])}

    def signal(self, name, a, sr):
        a = np.asarray(a, dtype=np.float32)
        self.signals[name] = {**self.put(f"sig_{name}", a), "sr": sr}
        return a


def outcome(fn, *args, **kw):
    try:
        return True, fn(*args, **kw)
    except Exception as e:  # noqa: BLE001 - the type name is the recorded outcome
        return False, {"error": type(e).__name__, "message": str(e)}


def margin(levels, thr) -> float:
    lv = np.asarray(levels, dtype=np.float64).ravel()
    return float(np.min(np.abs(lv - float(thr)))) if lv.size else math.inf


def need(m, what, floor=MIN_MARGIN_DB):
    assert m >= floor, f"knife edge {m} dB in {what}: redesign the signal"
    return num(m)


# de_ess's loud mask compares a per-sample envelope (tens of thousands of values on a continuous curve) with
# its threshold, so some sample always lies within 0.01 dB of it. A flip needs a difference far larger than
# the float64 envelope error (about 1e-12 dB), so that mask needs 1e-6 dB, not the frame rule.
MIN_SAMPLE_MARGIN_DB = 1e-6


# ---------------------------------------------------------------- synthetic signals


def harmonic_word(sr, dur, f0, rng, amp=0.3, attack=0.03, decay=0.08, harmonics=8):
    n = int(dur * sr)
    t = np.arange(n) / sr
    vib = 1 + 0.02 * np.sin(2 * np.pi * 5 * t)
    sig = np.zeros(n)
    for h in range(1, harmonics + 1):
        sig += np.sin(2 * np.pi * h * f0 * vib * t + rng.uniform(0, 2 * np.pi)) / h
    env = 0.65 + 0.35 * np.sin(2 * np.pi * 4.0 * t + rng.uniform(0, np.pi)) ** 2
    env *= np.minimum(1.0, t / attack) ** 2
    env *= np.minimum(1.0, (dur - t) / decay) ** 2
    return amp * sig / np.max(np.abs(sig)) * env


def hiss(n, sr, rng, lo=4500, hi=9000):
    spec = np.fft.rfft(rng.standard_normal(n))
    f = np.fft.rfftfreq(n, 1 / sr)
    spec[(f < lo) | (f > hi)] = 0
    h = np.fft.irfft(spec, n)
    return h / np.sqrt(np.mean(h**2))


def utterance(sr, parts, floor_db, seed, f0=150.0, amp=0.3, sib=0.0):
    """parts: ('w', dur) words, ('s', dur) a word with a sibilant tail, ('p', dur) pauses.
    `sib` is the RMS of the band noise added on 's' words (loud: a harsh 's')."""
    rng = np.random.default_rng(seed)
    out = []
    for kind, dur in parts:
        if kind in "ws":
            w = harmonic_word(sr, dur, f0 * rng.uniform(0.9, 1.1), rng, amp, harmonics=14)
            if kind == "s":
                k = int(0.4 * len(w))
                env = np.hanning(2 * k)[k:] ** 0.5
                w[-k:] = w[-k:] * 0.3 + sib * hiss(k, sr, rng, 4800, min(9000, sr / 2 * 0.9)) * env[::-1]
            out.append(w)
        else:
            out.append(np.zeros(int(dur * sr)))
    a = np.concatenate(out)
    if floor_db is not None:
        a = a + rng.standard_normal(len(a)) * 10 ** (floor_db / 20)
    return a


def shaped(a, sr, kind):
    """Colour a signal for a mic reference: boomy (low shelf up), thin (lows cut), bright (highs up)."""
    spec = np.fft.rfft(a)
    f = np.fft.rfftfreq(len(a), 1 / sr)
    if kind == "boomy":
        g = 1 + 2.5 / (1 + (f / 250) ** 2)
    elif kind == "thin":
        g = (f / 600) ** 2 / (1 + (f / 600) ** 2)
    else:  # bright
        g = 1 + 3.0 * (f / 4000) ** 2 / (1 + (f / 4000) ** 2)
    return np.fft.irfft(spec * g, len(a))


def tone_audit(seconds, amp=0.3, sr=24000):
    """backend/tests/test_audit_fixes.py `tone(seconds, 0, amp)` and test_repeat_cutoff.py `tone(d, amp)`."""
    return (amp * np.sin(2 * np.pi * 200 * np.arange(int(seconds * sr)) / sr)).astype(np.float32)


PHRASE = [("p", 0.08), ("w", 0.18), ("p", 0.08), ("s", 0.22), ("p", 0.12), ("w", 0.16), ("p", 0.06)]  # 0.9 s


def make_signals(st: Store) -> dict:
    s = {}

    def add(name, a, sr):
        s[name] = (st.signal(name, a, sr), sr)

    for sr in (24000, 44100, 48000):
        add(f"speech{sr // 1000}", utterance(sr, PHRASE if sr == 24000 else PHRASE[:5], -65, 11, sib=0.02), sr)
    add("harsh24", utterance(24000, PHRASE, -65, 12, sib=0.25), 24000)
    add("harsh48", utterance(48000, PHRASE, -65, 13, sib=0.25), 48000)
    add("harsh44", utterance(44100, PHRASE[:5], -65, 14, sib=0.25), 44100)
    add("harsh16", utterance(16000, PHRASE[:5], -65, 15, sib=0.25), 16000)
    add("clean24", utterance(24000, [("p", 0.1), ("w", 0.3), ("p", 0.25), ("w", 0.3), ("p", 0.1)], None, 16, amp=0.4), 24000)
    rng = np.random.default_rng(17)
    add("edge24", utterance(24000, [("p", 0.1), ("w", 1.0), ("p", 0.1)], None, 21, amp=0.4), 24000)
    gap = utterance(24000, [("w", 0.15), ("p", 0.9), ("w", 0.15)], None, 22)
    gap[int(0.15 * 24000): int(1.05 * 24000)] += 10 ** (-80 / 20) * np.random.default_rng(25).standard_normal(int(0.9 * 24000))
    add("gap_noise24", gap, 24000)  # 900 ms of -80 dB noise (-65 dB put noise frames on the -42 dB line) between two words: the 300 ms guard leaves its middle open
    add("noise24", 0.05 * rng.standard_normal(12000), 24000)
    base = utterance(24000, PHRASE, -70, 18)
    for kind in ("boomy", "thin", "bright"):
        add(f"{kind}24", shaped(base, 24000, kind), 24000)
    add("bright44", shaped(utterance(44100, PHRASE[:5], -70, 19), 44100, "bright"), 44100)
    half = utterance(24000, PHRASE[:4], -70, 20)
    add("silent_half24", np.concatenate([half, np.zeros(len(half))]), 24000)
    add("short24", s["speech24"][0][:8191], 24000)
    # The three backend fixtures, exactly as the tests build them (SR = 24000).
    add("audit_quiet_tail", np.concatenate([tone_audit(2), tone_audit(0.4, amp=0.3 * 10 ** (-45 / 20)), np.zeros(24000 // 2, np.float32)]), 24000)
    t = np.arange(int(1.0 * 24000)) / 24000
    vowel = 0.3 * np.sin(2 * np.pi * 180 * t[: int(0.8 * 24000)])
    fric = 0.03 * np.sin(2 * np.pi * 7000 * t[int(0.8 * 24000):]) * np.hanning(int(0.2 * 24000))
    add("audit_fricative", np.concatenate([vowel, fric]).astype(np.float32), 24000)
    for d in (0.1, 0.3, 0.55, 0.6, 0.61):
        add(f"audit_short_{int(d * 100):03d}", tone_audit(d), 24000)
    return s


# ---------------------------------------------------------------- per-function recorders


def ltas_frame_margin(a):
    n = 2048
    frames = a[: len(a) // n * n].reshape(-1, n)
    lvl = 10 * np.log10(np.mean(frames**2, axis=1) + 1e-12)
    return margin(lvl, np.percentile(lvl, 90) - 20)


def record_ltas(delivery, sig):
    out = []
    for name in ("speech24", "speech44", "speech48", "harsh24", "boomy24", "thin24", "bright24", "bright44",
                 "silent_half24", "short24", "clean24"):
        a, sr = sig[name]
        bands = delivery.ltas(a, sr)
        c = {"signal": name, "bands": [band(x) for x in bands]}
        if bands:
            c["margin_db"] = need(ltas_frame_margin(a), f"ltas {name}")
        out.append(c)
    return out


def tie_margin(sm) -> float:
    """Smallest distance (dB) of any curve value to a one-decimal rounding tie."""
    v = np.asarray(sm, dtype=np.float64) * 10
    return float(np.min(np.abs(v - np.floor(v) - 0.5))) / 10 if v.size else math.inf


def mic_fir_inputs(sm, sr):
    """The lines of delivery.mic_match between the curve and firwin2, copied."""
    nyq = sr / 2
    B = __import__("app.delivery", fromlist=["BANDS"]).BANDS
    fpts = [0.0] + [min(f, nyq * 0.999) / nyq for f in B if f < nyq * 0.999] + [1.0]
    gains_db = [sm[0]] + [g for f, g in zip(B, sm) if f < nyq * 0.999] + [0.0]
    fpts, idx = np.unique(fpts, return_index=True)
    gains = 10 ** (np.array(gains_db)[idx] / 20)
    return fpts, gains


def record_mic(delivery, ss, st, sig):
    cases, firs = [], []
    L = lambda name: delivery.ltas(*sig[name])  # noqa: E731
    gen24 = np.array(L("speech24"))
    tilt = gen24 + np.linspace(15, -15, len(gen24))
    few = [x if i < 5 else float("nan") for i, x in enumerate(L("boomy24"))]
    six = [x if i < 6 else float("nan") for i, x in enumerate(L("thin24"))]
    plan = [
        ("boomy", "speech24", L("boomy24")),
        ("thin", "speech24", L("thin24")),
        ("bright", "speech24", L("bright24")),
        ("capped", "speech24", list(tilt)),
        ("ref24_on_48k", "speech48", L("boomy24")),
        ("bright44_on_44k", "speech44", L("bright44")),
        ("six_finite", "speech24", six),
        ("exit_short_clip", "short24", L("thin24")),
        ("exit_band_count", "speech24", L("thin24")[:23]),
        ("exit_few_finite", "speech24", few),
    ]
    for case, name, ref in plan:
        a, sr = sig[name]
        out, curve = delivery.mic_match(a, sr, ref)
        c = {"case": case, "signal": name, "ref_bands": [band(x) for x in ref], "curve": curve}
        if curve:
            # the smoothed curve before rounding, recomputed as mic_match does, for the tie margin
            gen = np.array(delivery.ltas(a, sr))
            diff = np.array(ref, dtype=float) - gen
            ok = np.isfinite(diff)
            diff[ok] -= np.median(diff[ok])
            sm = diff.copy()
            for i in range(len(diff)):
                w = diff[max(0, i - 1): i + 2]
                w = w[np.isfinite(w)]
                sm[i] = w.mean() if len(w) else 0.0
            sm = np.clip(np.nan_to_num(sm, nan=0.0), -6.0, 6.0)
            assert [round(float(g), 1) for g in sm] == curve
            tm = tie_margin(sm)
            assert tm >= MIN_TIE_DB, f"rounding tie {tm} dB in mic_match {case}: redesign"
            c["tie_margin_db"] = tm
            c["capped"] = int(np.sum(np.abs(sm) == 6.0))
            fpts, gains = mic_fir_inputs(sm, sr)
            fir = ss.firwin2(1025, fpts, gains)
            assert np.array_equal(ss.fftconvolve(a, fir, mode="same").astype(np.float32), out)
            firs.append({"case": case, "sr": sr, "fpts": [float(x) for x in fpts], "gains": [float(x) for x in gains],
                         "taps": st.put64(f"fir_{case}", fir)})
            c["out"] = st.put(f"mic_{case}", out)
        else:
            assert out is a, case  # the input array itself
        cases.append(c)
    return cases, firs


def expander_internals(a, sr, guard_ms=300):
    hop = max(1, int(0.005 * sr))
    n = len(a) // hop
    if n < 20:
        return None, None
    fr = a[: n * hop].reshape(n, hop)
    lvl = 10 * np.log10(np.mean(fr**2, axis=1) + 1e-12)
    p90 = np.percentile(lvl, 90)
    guard = max(1, int(guard_ms / 5))
    near = lvl > p90 - 42
    prot = np.convolve(np.pad(near.astype(np.float32), (guard, guard)), np.ones(2 * guard + 1), mode="valid") > 0
    return "".join("1" if p else "0" for p in prot), margin(lvl, p90 - 42)


def record_expander(studio, st, sig):
    out = []
    plan = ["speech24", "gap_noise24", "noise24", "audit_quiet_tail", "harsh24"]  # 48 kHz: master_chain harsh48
    plan += [f"audit_short_{d:03d}" for d in (10, 30, 55, 60, 61)]
    # 19, 20 and 21 frames (hop 120 at 24 kHz, with a remainder), a clip shorter than the guard kernel (121 frames)
    t = sig["clean24"][0]
    extra = {"frames19": t[2400: 2400 + 19 * 120 + 37], "frames20": t[2400: 2400 + 20 * 120 + 37],
             "frames21": t[2400: 2400 + 21 * 120 + 37], "under_guard_kernel": t[2400: 2400 + 50 * 120]}
    for name in plan + list(extra):
        if name in extra:
            a, sr = extra[name].copy(), 24000
            src = {"input": st.put(f"exp_in_{name}", a), "sr": sr}
        else:
            a, sr = sig[name]
            src = {"signal": name}
        y = studio.expander(a, sr)
        prot, m = expander_internals(a, sr)
        c = {"case": name, **src, "unchanged": y is a, "protected": prot}
        if prot is not None:
            c["margin_db"] = need(m, f"expander {name}")
            c["out"] = st.put(f"exp_{name}", y)
        out.append(c)
    return out


def de_ess_margin(y, sr):
    """Distance of the loud-frame levels to their threshold inside de_ess (input that de_ess accepts)."""
    from scipy.signal import butter, sosfiltfilt  # noqa: F401 - only for symmetry with de_ess
    n = int(0.004 * sr)
    env_a = np.sqrt(np.convolve(y**2, np.ones(n) / n, mode="same") + 1e-12)
    loud_db = 20 * np.log10(env_a)
    return margin(loud_db, np.percentile(loud_db, 90) - 14)


def record_de_ess(studio, st, sig):
    out = []
    plan = [("harsh24", None), ("harsh48", None), ("harsh44", None), ("audit_fricative", None), ("clean24", None),
            ("harsh16", None)]
    h = sig["harsh24"][0]
    # sosfiltfilt padlen (27 for this band-pass) < 60 < n (96) <= 200 < 3n (288)
    for ln in (20, 60, 200, 288, 300):
        plan.append((f"len{ln}", (h[6000: 6000 + ln].copy(), 24000)))
    plan.append(("len20_16k", (sig["harsh16"][0][:20].copy(), 16000)))
    for name, extra in plan:
        if extra is not None:
            a, sr = extra
            src = {"input": st.put(f"de_in_{name}", a), "sr": sr}
        else:
            a, sr = sig[name]
            src = {"signal": name}
        ok, y = outcome(studio.de_ess, a, sr)
        c = {"case": name, **src}
        if not ok:
            c["result"] = y
        else:
            c["unchanged"] = y is a
            c["out"] = st.put(f"de_{name}", y)
            if sr >= 20000:
                c["margin_db"] = need(de_ess_margin(a, sr), f"de_ess {name}", MIN_SAMPLE_MARGIN_DB)
        out.append(c)
    return out


def record_master(studio, au, st, sig):
    out = []
    plan = [("speech24", None), ("harsh24", None), ("clean24", None), ("harsh48", None), ("harsh16", None)]
    h = sig["harsh24"][0]
    # highpass padlen, de_ess (under 3n = 288), compression window (under n = 720), expander under 20 frames, 25 frames
    for ln in (20, 200, 500, 800, 3000):
        plan.append((f"len{ln}", (h[6000: 6000 + ln].copy(), 24000)))
    for name, extra in plan:
        if extra is not None:
            a, sr = extra
            src = {"input": st.put(f"mc_in_{name}", a), "sr": sr}
        else:
            a, sr = sig[name]
            src = {"signal": name}
        ok, y = outcome(studio.master_chain, a, sr)
        c = {"case": name, **src}
        if not ok:
            c["result"] = y
        else:
            c["out"] = st.put(f"mc_{name}", y)
            # stage intermediates, recomputed with the same calls master_chain makes
            hp = au.highpass(a, sr, 75)
            de = studio.de_ess(hp, sr)
            n = int(0.03 * sr)
            rms = np.sqrt(np.convolve(de**2, np.ones(n) / n, mode="same") + 1e-12)
            g = 10 ** (-(np.maximum(0, 20 * np.log10(rms) - (-24)) * 0.5) / 20)
            comp = (de * np.convolve(g, np.ones(n) / n, mode="same")).astype(np.float32)
            assert np.array_equal(studio.expander(comp, sr), y)
            if sr >= 20000:
                c["de_ess_margin_db"] = need(de_ess_margin(hp, sr), f"master de_ess {name}", MIN_SAMPLE_MARGIN_DB)
            prot, m = expander_internals(comp, sr)
            if prot is not None:
                c["expander_margin_db"] = need(m, f"master expander {name}")
            if name == "harsh24":
                c["stages"] = {"highpass": st.put(f"mc_{name}_hp", hp), "de_ess": st.put(f"mc_{name}_de", de),
                               "compressed": st.put(f"mc_{name}_comp", comp)}
        out.append(c)
    return out


def edge_internals(au, reference, processed, sr):
    """Margins of the two level masks and the longest boundary run of `missing` (the function's loop)."""
    ref = au.frame_db(reference, sr, 10)
    got = au.frame_db(np.pad(processed, (0, max(0, len(reference) - len(processed))))[:len(reference)], sr, 10)
    nearby = np.max(np.lib.stride_tricks.sliding_window_view(np.pad(got, (5, 5), constant_values=-120), 11), axis=1)
    expected = ref > np.max(ref) - 35
    missing = expected & (nearby < np.max(got) - 60)
    live = np.flatnonzero(expected)
    run = best = 0
    for i in range(len(missing)):
        at_edge = len(live) and (i <= live[0] + 40 or i >= live[-1] - 40)
        run = run + 1 if at_edge and missing[i] else 0
        best = max(best, run)
    return min(margin(ref, np.max(ref) - 35), margin(nearby, np.max(got) - 60)), best


def record_edge(pipeline, au, st, sig):
    """The processed side is stored as an operation on the reference (zeros and slices are exact in Rust too);
    only the compressed and the lengthened copies are stored as arrays."""
    sr = 24000
    ref = sig["edge24"][0]  # 0.1 s digital silence, one 1.0 s word, 0.1 s digital silence
    s0, s1 = int(0.1 * sr), int(0.1 * sr) + int(1.0 * sr)  # speech start / end samples
    ms = lambda x: int(x * sr)  # noqa: E731
    plan = [
        ("empty_reference", {"op": "empty_reference"}),
        ("silent_reference", {"op": "silent_reference"}),
        ("empty_output", {"op": "slice", "lo": 0, "hi": 0}),
        ("silent_output", {"op": "zero", "lo": 0, "hi": len(ref)}),
        ("identical", {"op": "slice", "lo": 0, "hi": len(ref)}),
        ("compressed_copy", {"op": "array", "a": (np.tanh(4 * ref) / 4).astype(np.float32)}),
        ("shorter_output_padded", {"op": "slice", "lo": 0, "hi": len(ref) - ms(0.08)}),
        ("shorter_output_cuts_speech", {"op": "slice", "lo": 0, "hi": s1 - ms(0.2)}),
        ("longer_output_cut", {"op": "array", "a": np.concatenate([ref, 0.2 * np.ones(ms(0.1), np.float32)])}),
        ("loss_at_start", {"op": "zero", "lo": s0, "hi": s0 + ms(0.2)}),
        ("loss_at_end", {"op": "zero", "lo": s1 - ms(0.2), "hi": s1}),
        ("boundary_loss_30ms", {"op": "zero", "lo": s0, "hi": s0 + ms(0.03)}),
        ("boundary_loss_40ms", {"op": "zero", "lo": s0, "hi": s0 + ms(0.04)}),
        ("boundary_loss_80ms_run3", {"op": "zero", "lo": s0, "hi": s0 + ms(0.08)}),
        ("boundary_loss_90ms_run4", {"op": "zero", "lo": s0, "hi": s0 + ms(0.09)}),
        ("end_loss_80ms_run2", {"op": "zero", "lo": s1 - ms(0.08), "hi": s1}),
        ("end_loss_90ms_run3", {"op": "zero", "lo": s1 - ms(0.09), "hi": s1}),
        ("end_loss_100ms_run4", {"op": "zero", "lo": s1 - ms(0.1), "hi": s1}),
        ("mid_clip_loss", {"op": "zero", "lo": ms(0.55), "hi": ms(0.65)}),
        ("shifted_40ms", {"op": "shift", "n": ms(0.04)}),
    ]
    out = []
    for case, op in plan:
        r = ref
        kind = op["op"]
        if kind == "empty_reference":
            r, p = np.zeros(0, np.float32), ref
        elif kind == "silent_reference":
            r, p = np.zeros_like(ref), ref
        elif kind == "slice":
            p = ref[op["lo"]:op["hi"]]
        elif kind == "zero":
            p = ref.copy()
            p[op["lo"]:op["hi"]] = 0
        elif kind == "shift":
            p = np.concatenate([np.zeros(op["n"], np.float32), ref])[: len(ref)]
        else:
            p = op.pop("a")
            op["a"] = st.put(f"edge_out_{case}", p)
        p = np.asarray(p, np.float32)
        issues = sorted(pipeline._processed_edge_issues(r, p, sr))
        c = {"case": case, "sr": sr, "processed": op, "issues": issues}
        if len(r) and np.max(np.abs(r)) >= 1e-5 and len(p) and np.max(np.abs(p)) >= 1e-5:
            m, run = edge_internals(au, r, p, sr)
            c["margin_db"] = need(m, f"edge {case}")
            c["max_edge_run"] = run
        out.append(c)
    return out


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

        np = numpy
        from app import audio as au
        from app import delivery, pipeline, studio

        def no_ffmpeg(*a, **k):
            raise RuntimeError("record_mastering.py must never run ffmpeg")

        au.subprocess = type("NoSubprocess", (), {"run": staticmethod(no_ffmpeg)})
        assert not {"mlx", "mlx.core", "onnxruntime", "torch"} & set(sys.modules), "no model import"

        if ARR.exists():
            shutil.rmtree(ARR)
        ARR.mkdir()
        st = Store()
        sig = make_signals(st)
        data = {"ltas": record_ltas(delivery, sig)}
        data["mic_match"], data["firwin2"] = record_mic(delivery, ss, st, sig)
        data["expander"] = record_expander(studio, st, sig)
        data["de_ess"] = record_de_ess(studio, st, sig)
        data["master_chain"] = record_master(studio, au, st, sig)
        data["edge"] = record_edge(pipeline, au, st, sig)
        # pipeline imports app.ml (lazy: it loads no model at import, as in record_checks.py); no model library loaded
        assert not {"mlx", "mlx.core", "onnxruntime", "torch"} & set(sys.modules), "no model import"
        data = {"bands": [float(x) for x in delivery.BANDS], "signals": st.signals, **data}

        total = sum(st.files.values())
        assert total <= MAX_BYTES, total
        text = json.dumps(data, ensure_ascii=True, indent=1, allow_nan=False) + "\n"
        assert "NaN" not in text
        OUT.write_text(text)
        print({k: len(v) for k, v in data.items() if isinstance(v, list)})
        ms = [c[k] for v in data.values() if isinstance(v, list) for c in v if isinstance(c, dict)
              for k in ("margin_db", "expander_margin_db") if isinstance(c.get(k), float) and v is not data["de_ess"]]
        ds = [c[k] for name in ("de_ess", "master_chain") for c in data[name] for k in ("margin_db", "de_ess_margin_db")
              if isinstance(c.get(k), float) and (name == "master_chain") == (k == "de_ess_margin_db")]
        ties = [c["tie_margin_db"] for c in data["mic_match"] if "tie_margin_db" in c]
        print(f"files {len(st.files)}, {total} bytes; smallest frame margin {min(ms):.4f} dB; smallest de_ess sample margin {min(ds):.3e} dB; smallest tie margin {min(ties):.3e} dB")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
