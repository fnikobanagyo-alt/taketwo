"""Record Python parity vectors for the Rust take checks, ending guard and pitch measures (step S11a, gate G1).

    HF_HUB_OFFLINE=1 backend/.venv/bin/python -B rust/golden/record_checks.py

Writes rust/golden/checks.json and raw little-endian float32 arrays under rust/golden/checks/ (inputs that
rust/golden/audio/ does not already hold, and outputs). Functions: pipeline.evaluate, _internal_sentence_cut,
HARD_ISSUES, noise_limit; ending_guard (synthesis_text, _tokens, remove_context); delivery.prosody,
expressiveness, DEFAULT_VOICE_SETTINGS, resolve, pick_variant.
Every signal is synthetic and seeded; transcripts and speaker similarities are canned. Each case stores its
distance to every limit it is judged against (`margins`), and the recorder fails when one is closer than float
noise could move it, or when a rounded number sits within 1e-9 of a rounding half (1e-3 for the pitch
decisions). Every case with word times is recorded three times: as given, all times -0.16 s (floored at 0) and
+0.16 s; `changed` says whether Python's own decision moved.
Safety: the data folder is a fresh temporary folder (checked before any app import), Hugging Face is offline,
no model is loaded (ml.stt_model raises; the Whisper normalizer comes from tokenizer files only;
ml.speaker_embedding and ml.cosine are replaced), ffmpeg is never run, and numba's cache goes to the
temporary folder (the venv is read-only).
"""
from __future__ import annotations

import builtins
import copy
import hashlib
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
OUT = HERE / "checks.json"
ARR = HERE / "checks"
AUDIO = HERE / "audio"
MAX_BYTES = 6_100_000  # du -sk of checks/ must stay at or under 6144 (file blocks included)
SHIFT = 0.16
MIN_DB = 0.01  # a level compared with a threshold stays this far from it (dB)
MIN_NUM = 1e-6  # a ratio, rate or similarity stays this far from its limit
HALF = 1e-9  # an unrounded number stays this far from a rounding half
HALF_PITCH = 1e-3

np = None


def num(x):
    x = float(x)
    if math.isnan(x):
        return "nan"
    if math.isinf(x):
        return "inf" if x > 0 else "-inf"
    return x


def dtype(v) -> str:
    return type(v).__name__ if not hasattr(v, "dtype") else str(v.dtype)


class Store:
    """Arrays as files under checks/, one file per distinct content; inputs already in audio/ are referenced."""

    def __init__(self):
        self.by_hash: dict[str, str] = {}
        self.audio_hashes: dict[str, str] = {}
        for f in sorted(AUDIO.glob("*.f32")):
            self.audio_hashes.setdefault(hashlib.sha256(f.read_bytes()).hexdigest(), f.name)
        self.bytes = 0

    def put(self, name: str, a) -> dict:
        b = np.ascontiguousarray(np.asarray(a), dtype="<f4").tobytes()
        h = hashlib.sha256(b).hexdigest()
        n = len(b) // 4
        if h in self.audio_hashes:
            return {"dir": "audio", "file": self.audio_hashes[h], "len": n}
        if h not in self.by_hash:
            fname = f"{name}.f32"
            assert not (ARR / fname).exists(), fname
            (ARR / fname).write_bytes(b)
            self.bytes += len(b)
            self.by_hash[h] = fname
        return {"dir": "checks", "file": self.by_hash[h], "len": n}


def sha(a) -> dict:
    """A shifted-timing output, stored as the SHA-256 of its float32 bytes (budget): Rust must be bit-equal there."""
    b = np.ascontiguousarray(np.asarray(a), dtype="<f4").tobytes()
    return {"sha256": hashlib.sha256(b).hexdigest(), "len": len(b) // 4}


def audio_signal(name: str):
    """A signal rust/golden/audio/ already holds (24 kHz)."""
    b = (AUDIO / f"sig_{name}.f32").read_bytes()
    return np.frombuffer(b, dtype="<f4").copy()


# ---------------------------------------------------------------- synthetic signals (as record_audio.py)


def harmonic_word(sr, dur, f0, rng, amp=0.3, attack=0.03, decay=0.08):
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


def hiss(n, sr, rng):
    spec = np.fft.rfft(rng.standard_normal(n))
    f = np.fft.rfftfreq(n, 1 / sr)
    spec[(f < 4500) | (f > 9000)] = 0
    h = np.fft.irfft(spec, n)
    return h / np.sqrt(np.mean(h**2))


def take(sr, parts, seed, floor_db=-70.0, f0=150.0, amp=0.3):
    """parts: ("w", text, dur, opts) spoken words, ("p", dur) pauses, ("a", array) raw pieces.
    Returns (float32 signal, words with Whisper-like times). opts: decay, cut (stop hard at this fraction), amp."""
    rng = np.random.default_rng(seed)
    out, words, t = [], [], 0.0
    for p in parts:
        if p[0] == "w":
            _, text, dur, *rest = p
            o = rest[0] if rest else {}
            w = harmonic_word(sr, dur, f0 * rng.uniform(0.92, 1.08), rng, o.get("amp", amp), decay=o.get("decay", 0.08))
            if "cut" in o:
                w[int(o["cut"] * len(w)):] = 0
            out.append(w)
            if text:
                words.append({"word": " " + text, "start": round(t + 0.02, 2), "end": round(t + dur - 0.02, 2), "p": 0.9})
            t += len(w) / sr
        elif p[0] == "p":
            out.append(np.zeros(int(p[1] * sr)))
            t += int(p[1] * sr) / sr
        else:
            out.append(np.asarray(p[1], dtype=np.float64))
            t += len(p[1]) / sr
    a = np.concatenate(out)
    if floor_db is not None:
        a = a + rng.standard_normal(len(a)) * 10 ** (floor_db / 20)
    return a.astype(np.float32), words


def transcript(words, text=None):
    heard = " ".join(w["word"].strip() for w in words) if text is None else text
    segs = []
    if words:
        segs.append({"start": words[0]["start"], "end": words[-1]["end"], "text": heard, "avg_logprob": -0.2,
                     "no_speech_prob": 0.01, "words": words})
    return {"text": heard, "segments": segs, "language": "en"}


def shifted(words, d):
    return [{**w, "start": max(0.0, w["start"] + d), "end": max(0.0, w["end"] + d)} for w in words]


def shifted_tr(tr, d):
    tr = copy.deepcopy(tr)
    for s in tr["segments"]:
        s["words"] = shifted(s["words"], d)
    return tr


# ---------------------------------------------------------------- rounding guard


ROUNDS: list[tuple[float, int | None, float]] = []


def guarded_round(x, n=None):
    """builtins.round, recording each float and its distance to the nearest rounding half."""
    if isinstance(x, float) or (hasattr(x, "dtype") and np.issubdtype(np.asarray(x).dtype, np.floating)):
        s = 10.0 ** (n or 0)
        v = float(x) * s
        ROUNDS.append((float(x), n, abs(v - math.floor(v) - 0.5) / s))
    return builtins.round(x, n) if n is not None else builtins.round(x)


def take_rounds(limit=HALF):
    out = [r for r in ROUNDS]
    ROUNDS.clear()
    # an exact binary half (2.625) is a deliberate tie case (round-half-even); a value a few ulps off a half is not
    bad = [r for r in out if 0 < r[2] < limit]
    return out, bad


# ---------------------------------------------------------------- evaluate


def evaluate_cases(st, pipeline, au, ml, textproc, config):
    S = dict(config.DEFAULT_SETTINGS)
    R = 16000  # most takes at 16 kHz to fit the 6 MB budget; clean_24k and speech_f45 run at 24 kHz
    cases = []

    def run(name, a, sr, text, tr, *, sim=0.8, self_sim=0.8, wps=2.6, s=None, noise_max=0.09, expect=(), absent=(),
            variants=True, signal=None):
        s = dict(S if s is None else s)
        sig = signal or st.put(f"in_{name}", a)
        trs = [("given", tr)]
        has_words = any(seg["words"] for seg in tr["segments"])
        if variants and has_words:
            trs += [("minus", shifted_tr(tr, -SHIFT)), ("plus", shifted_tr(tr, SHIFT))]
        runs = []
        for label, t in trs:
            handed = []

            def emb(x, r):
                handed.append((np.array(x, copy=True), r))
                return np.ones(1, np.float32)

            ml.speaker_embedding = emb
            ml.cosine = lambda e, fp: float(sim)
            ev = pipeline.evaluate(a.copy(), sr, text, np.ones(1, np.float32), self_sim, wps, s, "en", noise_max, transcript=copy.deepcopy(t))
            rounds, bad = take_rounds()
            assert not bad, (name, label, bad)
            out = {
                "timing": label, "transcript": t, "issues": ev["issues"],
                "score": ev["score"], "wer": ev["wer"], "sim": ev["sim"], "max_gap": ev["max_gap"], "dur": ev["dur"],
                "sr": ev["sr"], "audio": st.put(f"out_{name}_{label}", ev["audio"]) if label == "given" else sha(ev["audio"]),
                "keys": list(ev.keys()), "dtypes": {k: dtype(v) for k, v in ev.items() if k not in ("issues", "gaps", "words", "heard", "ending")},
                "similarity_called": bool(handed),
                "similarity_len": len(handed[0][0]) if handed else None,
                "similarity_input_is_audio": bool(handed) and np.array_equal(handed[0][0], ev["audio"]) and handed[0][1] == sr,
            }
            assert len(handed) <= 1
            if "gaps" in ev:
                out["gaps"] = [[float(x), float(y)] for x, y in ev["gaps"]]
                out["heard"] = ev["heard"]
                out["words"] = ev["words"]
                out["raw_is_input"] = bool(np.array_equal(ev["raw_audio"], a))
                out["ending"] = {k: (num(v) if isinstance(v, float) else (bool(v) if isinstance(v, (bool, np.bool_)) else num(v)))
                                 for k, v in ev["ending"].items()}
                out["ending_dtypes"] = {k: dtype(v) for k, v in ev["ending"].items()}
                out["cut_sample"] = int(ev["ending"]["cut"] * sr)
            out["margins"] = margins(pipeline, au, ml, textproc, a, sr, text, t, ev, s, sim, self_sim, wps, noise_max, handed)
            runs.append(out)
        base = runs[0]
        for r in runs[1:]:
            r["changed"] = {k: r.get(k) != base.get(k) for k in ("issues", "cut_sample")}
            r["changed"]["trim_start"] = r.get("ending", {}).get("trim_start") != base.get("ending", {}).get("trim_start")
        for e in expect:
            assert e in base["issues"], (name, e, base["issues"])
        for e in absent:
            assert e not in base["issues"], (name, e, base["issues"])
        case = {"name": name, "signal": sig, "sr": sr, "text": text, "self_sim": self_sim, "wps": wps,
                "limits": {k: s[k] for k in ("max_wer", "min_speaker_similarity", "max_internal_silence")},
                "noise_max": noise_max, "similarity": sim, "runs": runs}
        cases.append(case)
        return base

    # clean takes
    a, w = take(24000, [("p", 0.12), ("w", "Hello", 0.3), ("p", 0.08), ("w", "there,", 0.28), ("p", 0.2), ("w", "friend.", 0.4, {"decay": 0.15}), ("p", 0.3)], 1)
    b = run("clean_24k", a, 24000, "Hello there, friend.", transcript(w), expect=())
    assert b["issues"] == [], b["issues"]
    a, w = take(16000, [("p", 0.1), ("w", "We", 0.2), ("p", 0.05), ("w", "walked", 0.32), ("p", 0.06), ("w", "home", 0.3), ("p", 0.05),
                        ("w", "slowly.", 0.42, {"decay": 0.15}), ("p", 0.3)], 2)
    b = run("clean_16k", a, 16000, "We walked home slowly.", transcript(w))
    assert b["issues"] == [], b["issues"]
    clean = (a, w)

    # empty: under 0.3 s
    a, w = take(R, [("w", "Hi.", 0.25)], 3)
    b = run("empty", a, R, "Hi.", transcript(w), expect=("empty",))
    assert b["issues"] == ["empty"]
    a, w = take(R, [("p", 0.05), ("w", "Hi.", 0.26)], 4)  # just over the 0.3 s line
    run("over_empty", a, R, "Hi.", transcript(w), absent=("empty",))

    # cut_off from the ending: the last word stops hard
    a, w = take(R, [("p", 0.1), ("w", "Come", 0.3), ("p", 0.06), ("w", "back", 0.45, {"cut": 0.8, "decay": 0.01}), ("p", 0.4)], 5)
    run("cut_off_end", a, R, "Come back", transcript(w), expect=("cut_off",))

    # cut_off from inside: the first sentence stops loud, the second ends naturally
    a, w = take(R, [("p", 0.1), ("w", "Stop", 0.3), ("p", 0.05), ("w", "here.", 0.4, {"cut": 0.75, "decay": 0.01}), ("p", 0.35),
                    ("w", "Then", 0.3), ("p", 0.05), ("w", "go.", 0.4, {"decay": 0.15}), ("p", 0.3)], 6)
    b = run("cut_off_inside", a, R, "Stop here. Then go.", transcript(w), expect=("cut_off",))
    assert b["issues"].count("cut_off") == 1
    # both: a second cut_off in the list
    a, w = take(R, [("p", 0.1), ("w", "Stop", 0.3), ("p", 0.05), ("w", "here.", 0.4, {"cut": 0.75, "decay": 0.01}), ("p", 0.35),
                    ("w", "Then", 0.3), ("p", 0.05), ("w", "go", 0.4, {"cut": 0.8, "decay": 0.01}), ("p", 0.35)], 7)
    b = run("cut_off_twice", a, R, "Stop here. Then go", transcript(w))
    assert b["issues"].count("cut_off") == 2, b["issues"]

    # lead_burst: hiss, a pause, then the words
    rng = np.random.default_rng(8)
    n = int(0.12 * R)
    h = hiss(n, R, rng) * 0.08 * np.minimum(1, np.minimum(np.arange(n), n - np.arange(n)) / 48)
    a, w = take(R, [("p", 0.05), ("a", h), ("p", 0.3), ("w", "Then", 0.3), ("p", 0.05), ("w", "over.", 0.4, {"decay": 0.15}), ("p", 0.3)], 8)
    run("lead_burst", a, R, "Then over.", transcript(w), expect=("lead_burst",))

    # junk_tail: a stray sound after the last word
    a, w = take(R, [("p", 0.1), ("w", "All", 0.3), ("p", 0.05), ("w", "done.", 0.4, {"decay": 0.15}), ("p", 0.33), ("w", "", 0.15), ("p", 0.3)], 9)
    run("junk_tail", a, R, "All done.", transcript(w), expect=("junk_tail",))

    # odd_pause: a long pause after a word without punctuation; then the same audio with each accepted mark
    parts = [("p", 0.1), ("w", "Into", 0.3), ("p", 0.05), ("w", "the", 0.2), ("p", 0.62), ("w", "final", 0.3), ("p", 0.05),
             ("w", "battle.", 0.4, {"decay": 0.15}), ("p", 0.3)]
    a, w = take(R, parts, 10)
    b = run("odd_pause", a, R, "Into the final battle.", transcript(w), expect=("odd_pause",))
    sig = st.put("in_odd_pause", a)
    for i, mark in enumerate([",", ".", "?", "!", ";", ":", "—", "-", "…"]):
        w2 = copy.deepcopy(w)
        w2[1]["word"] = " the" + mark
        run(f"odd_pause_mark_{i}", a, R, "Into the" + mark + " final battle.", transcript(w2), absent=("odd_pause",), signal=sig)
    # a pause just under 0.5 s is not judged
    a, w = take(R, [("p", 0.1), ("w", "Into", 0.3), ("p", 0.05), ("w", "the", 0.2), ("p", 0.43), ("w", "final", 0.3), ("p", 0.05),
                    ("w", "battle.", 0.4, {"decay": 0.15}), ("p", 0.3)], 11)
    run("short_pause", a, R, "Into the final battle.", transcript(w), absent=("odd_pause",))

    # end_word_missing / start_word_missing: the heard text drops a word
    a, w = clean
    run("end_word_missing", a, 16000, "We walked home slowly.", transcript(w[:3], "We walked home."), expect=("end_word_missing",),
        signal=st.put("in_clean_16k", a))
    run("start_word_missing", a, 16000, "We walked home slowly.", transcript(w[1:], "walked home slowly."), expect=("start_word_missing",),
        signal=st.put("in_clean_16k", a))

    # long_silence: a pause over 0.9 s after a comma (not odd)
    a, w = take(16000, [("p", 0.1), ("w", "Wait,", 0.35), ("p", 1.05), ("w", "then", 0.3), ("p", 0.05), ("w", "run.", 0.4, {"decay": 0.15}), ("p", 0.3)], 12)
    run("long_silence", a, 16000, "Wait, then run.", transcript(w), expect=("long_silence",), absent=("odd_pause",))
    # under the line: 0.8 s
    a, w = take(16000, [("p", 0.1), ("w", "Wait,", 0.35), ("p", 0.8), ("w", "then", 0.3), ("p", 0.05), ("w", "run.", 0.4, {"decay": 0.15}), ("p", 0.3)], 13)
    run("under_long_silence", a, 16000, "Wait, then run.", transcript(w), absent=("long_silence",))

    # too_long (expected floored at 0.8 s for one word) and too_short
    a, w = take(16000, [("p", 0.1), ("w", "Yes.", 1.3, {"decay": 0.15}), ("p", 0.3)], 14)
    run("too_long", a, 16000, "Yes.", transcript(w), expect=("too_long",))
    text = "One two three four five six seven eight nine ten eleven twelve."
    a, w = take(16000, [("p", 0.1), ("w", "One", 0.4), ("p", 0.05), ("w", "two", 0.4), ("p", 0.05), ("w", "twelve.", 0.5, {"decay": 0.15}), ("p", 0.3)], 15)
    run("too_short", a, 16000, text, transcript(w, text), expect=("too_short",))

    # voice_drift, and the self-similarity floor of 1e-3
    a, w = clean
    sig = st.put("in_clean_16k", a)
    run("voice_drift", a, 16000, "We walked home slowly.", transcript(w), sim=0.6, expect=("voice_drift",), signal=sig)
    run("self_sim_floor", a, 16000, "We walked home slowly.", transcript(w), sim=0.0009, self_sim=0.0005, signal=sig, absent=("voice_drift",))
    run("drift_edge_above", a, 16000, "We walked home slowly.", transcript(w), sim=0.6811, self_sim=0.8, signal=sig, absent=("voice_drift",))
    run("drift_edge_below", a, 16000, "We walked home slowly.", transcript(w), sim=0.6789, self_sim=0.8, signal=sig, expect=("voice_drift",))

    # noise: loud noise bursts between the words; the same take with a larger limit
    rng = np.random.default_rng(16)
    nz = rng.standard_normal(int(0.3 * 16000)) * 0.1
    a, w = take(16000, [("p", 0.1), ("w", "Static", 0.4), ("a", nz), ("w", "noise.", 0.4, {"decay": 0.15}), ("p", 0.3)], 16)
    sig = st.put("in_noise", a)
    run("noise", a, 16000, "Static noise.", transcript(w), expect=("noise",), signal=sig)
    run("noise_limit_high", a, 16000, "Static noise.", transcript(w), noise_max=0.5, absent=("noise",), signal=sig)

    # clipping: a loud word driven past 0.985
    a, w = take(16000, [("p", 0.1), ("w", "Loud", 0.4, {"amp": 1.6}), ("p", 0.05), ("w", "word.", 0.4, {"decay": 0.15}), ("p", 0.3)], 17)
    a = np.clip(a, -1.0, 1.0).astype(np.float32)
    run("clipping", a, 16000, "Loud word.", transcript(w), expect=("clipping",))

    # mispronounced (max_wer < wer < 0.35) and wrong_words (>= 0.35); the same audio
    a, w = take(16000, [("p", 0.1), ("w", "The", 0.2), ("p", 0.05), ("w", "quick", 0.3), ("p", 0.05), ("w", "brown", 0.3), ("p", 0.05),
                        ("w", "fox", 0.3), ("p", 0.05), ("w", "jumps.", 0.4, {"decay": 0.15}), ("p", 0.3)], 18)
    sig = st.put("in_words", a)
    text = "The quick brown fox jumps."
    w2 = copy.deepcopy(w)
    w2[2]["word"] = " green"
    run("mispronounced", a, 16000, text, transcript(w2), expect=("mispronounced",), signal=sig)
    w3 = copy.deepcopy(w)
    w3[1]["word"], w3[2]["word"] = " big", " red"
    run("wrong_words", a, 16000, text, transcript(w3), expect=("wrong_words",), signal=sig)
    run("wer_under_limit", a, 16000, text, transcript(w), absent=("mispronounced", "wrong_words"), signal=sig)

    # the no-words branch (trim_edges) and a take of 1.0 s or less (similarity not called)
    a, w = clean
    run("no_words", a, 16000, "We walked home slowly.", {"text": "", "segments": [], "language": "en"}, signal=st.put("in_clean_16k", a))
    a, w = take(R, [("p", 0.1), ("w", "Yes.", 0.4, {"decay": 0.15}), ("p", 0.25)], 19)
    b = run("short_take", a, R, "Yes.", transcript(w))
    assert not b["similarity_called"] and b["dur"] <= 1.0, b
    # a reused S10 signal: speech_f45 with canned words
    a = audio_signal("speech_f45")
    w = [{"word": " One", "start": 0.12, "end": 0.3, "p": 0.9}, {"word": " more", "start": 0.44, "end": 0.62, "p": 0.9},
         {"word": " time.", "start": 0.92, "end": 1.15, "p": 0.9}]
    run("speech_f45", a, 24000, "One more time.", transcript(w), signal=st.put("speech_f45", a))

    # every issue evaluate can emit fires somewhere
    fired = {i for c in cases for r in c["runs"][:1] for i in r["issues"]}
    every = {"empty", "cut_off", "lead_burst", "junk_tail", "odd_pause", "end_word_missing", "start_word_missing", "long_silence",
             "too_long", "too_short", "voice_drift", "noise", "clipping", "mispronounced", "wrong_words"}
    assert every <= fired, every - fired
    assert sum(1 for c in cases if c["runs"][0]["issues"] == []) >= 2
    return cases


def margins(pipeline, au, ml, textproc, a, sr, text, tr, ev, s, sim, self_sim, wps, noise_max, handed):
    """Distance of this take to every limit evaluate judges it against; fails when one is knife-edge."""
    m = {"len_samples": len(a) - sr * 0.3}
    assert abs(m["len_samples"]) >= 0.5
    if "gaps" not in ev:
        return m
    out = ev["audio"]
    dur = len(out) / sr
    words_n = textproc.word_count(text) / wps
    expected = max(0.8, words_n)
    ratio = dur / expected
    used = sim if handed else self_sim
    rel = used / max(self_sim, 1e-3)
    rep = au.silence_report(out, sr)
    w = ml.wer(text, tr["text"])
    m.update({
        "ratio_long": ratio - 1.75, "ratio_short": ratio - 0.5, "expected_floor": words_n - 0.8,
        "clipping": au.clipping_ratio(out) - 0.003, "noise": au.noise_bursts(out, sr) - noise_max,
        "max_wer": w - s["max_wer"], "wer_035": w - 0.35, "rel_sim": rel - s["min_speaker_similarity"],
        "self_sim_floor": self_sim - 1e-3, "max_internal": rep.max_internal - s["max_internal_silence"],
    })
    words = ev["words"]
    if words:
        start, pad = ev["ending"]["trim_start"], ev["ending"]["head_pad_s"]
        gm, bm = math.inf, math.inf
        for gs, ge in rep.internal_gaps:
            gm = min(gm, abs((ge - gs) - 0.5))
            t0 = gs + start - pad
            bm = min([bm] + [abs(x["start"] - (t0 + 0.05)) for x in words])
        m["gap_05"], m["gap_word"] = gm, bm
    for k in ("ratio_long", "ratio_short", "rel_sim", "max_internal", "gap_05", "gap_word"):
        if k in m:
            assert abs(m[k]) >= MIN_NUM, (text, k, m[k])
    for k in ("expected_floor", "max_wer", "wer_035", "clipping", "noise"):
        assert m[k] != 0, (text, k)  # exact rationals or step counts: only equality is a knife edge
    return {k: num(v) for k, v in m.items()}


# ---------------------------------------------------------------- _internal_sentence_cut


def cut_margin(au, raw, sr):
    db = au.frame_db(raw, sr, 10)
    thr = np.percentile(db, 90) - 40
    return float(np.min(np.abs(db.astype(np.float64) - float(thr))))


def internal_cases(st, pipeline, au):
    R = 16000
    cases = []

    def run(name, a, sr, words, expect):
        sig = st.put(f"in_cut_{name}", a)
        runs = []
        for label, d in (("given", 0.0), ("minus", -SHIFT), ("plus", SHIFT)):
            ww = shifted(words, d) if d else copy.deepcopy(words)
            got = bool(pipeline._internal_sentence_cut(a, sr, ww))
            runs.append({"timing": label, "words": ww, "cut": got})
        for r in runs[1:]:
            r["changed"] = r["cut"] != runs[0]["cut"]
        assert runs[0]["cut"] == expect, (name, runs[0]["cut"])
        m = cut_margin(au, a, sr)
        assert m >= MIN_DB, (name, m)
        cases.append({"name": name, "signal": sig, "sr": sr, "margin_db": m, "runs": runs})

    def wd(t, s, e):
        return {"word": t, "start": s, "end": e, "p": 0.9}

    a, w = take(R, [("p", 0.1), ("w", "Stop", 0.3), ("p", 0.05), ("w", "here.", 0.4, {"cut": 0.75, "decay": 0.01}), ("p", 0.35),
                    ("w", "Then", 0.3), ("p", 0.05), ("w", "go.", 0.4, {"decay": 0.15}), ("p", 0.3)], 6)
    run("loud_stop", a, R, w, True)
    a_loud, loud = a, w
    a, w = take(R, [("p", 0.1), ("w", "Stop", 0.3), ("p", 0.05), ("w", "here.", 0.4, {"decay": 0.15}), ("p", 0.35),
                    ("w", "Then", 0.3), ("p", 0.05), ("w", "go.", 0.4, {"decay": 0.15}), ("p", 0.3)], 20)
    run("decays", a, R, w, False)
    # the same loud stop after "Mr." is not a sentence end (the loud_stop take, its word renamed)
    w = copy.deepcopy(loud)
    w[1]["word"] = " Mr."
    run("abbreviation", a_loud, R, w, False)
    w2 = copy.deepcopy(loud)
    # a gap under 0.15 s between the words: nothing to judge
    w3 = copy.deepcopy(w2)
    w3[2]["start"] = w3[1]["end"] + 0.1
    run("small_gap", a_loud, R, w3, False)
    # a segment under 0.2 s: the sentence ends before 0.2 s of audio
    a, w = take(R, [("w", "Go.", 0.12, {"cut": 0.9, "decay": 0.01}), ("p", 0.3), ("w", "Now", 0.3), ("p", 0.05),
                    ("w", "run.", 0.4, {"decay": 0.15}), ("p", 0.3)], 22)
    w[0]["start"], w[0]["end"] = 0.0, 0.1
    run("short_segment", a, R, w, False)
    # the next word is written late by Whisper: the stop is found inside the first real silence
    a, w = take(R, [("p", 0.1), ("w", "She", 0.3), ("p", 0.05), ("w", "said.", 0.4, {"decay": 0.15}), ("p", 0.15),
                    ("w", "But", 0.3), ("p", 0.05), ("w", "more.", 0.4, {"decay": 0.15}), ("p", 0.3)], 23)
    w[2]["start"] = w[2]["start"] + 0.1
    run("late_next_word", a, R, w, False)
    return cases


# ---------------------------------------------------------------- ending guard


def guard_cases(st, ending_guard, au):
    R = 16000
    out = {"continuation": ending_guard.CONTINUATION}
    syn = []
    for text, fam, lang, en in [("Hello there.", "qwen3", "en", True), ("Hello there", "qwen3", "English", True),
                                ("Wait!", "qwen3", "EN", True), ("Why?", "qwen3", "en", True), ("Stop,  ", "qwen3", "en", True),
                                ("Hello there.", "qwen3", "en", False), ("Hello there.", "voxcpm2", "en", True),
                                ("Hallo.", "qwen3", "de", True), ("Done;", "qwen3", "english", True), ("", "qwen3", "en", True)]:
        r = ending_guard.synthesis_text(text, fam, lang, enabled=en)
        syn.append({"text": text, "family": fam, "language": lang, "enabled": en, "out": r[0], "guarded": r[1]})
    out["synthesis_text"] = syn
    toks = ["Hello, world!", "snake_case_word", "Straße STRASSE", "ſun", "İstanbul", "ΟΔΟΣ",
            "café naïve", "你好世界", "x2 3d 4_5", "— … '", "don't", "ＡＢＣ", "½ ①"]
    out["tokens"] = [{"text": t, "out": ending_guard._tokens(t)} for t in toks]

    cont = ["Meanwhile,", "another", "chapter", "begins."]

    def build(seed, target, pause, noisy=False, cont_words=cont):
        parts = [("p", 0.1)]
        for i, t in enumerate(target):
            parts.append(("w", t, 0.32, {"decay": 0.12} if i == len(target) - 1 else {}))
            parts.append(("p", 0.05))
        parts[-1] = ("p", pause)
        if noisy:
            rng = np.random.default_rng(seed)
            parts[-1] = ("a", rng.standard_normal(int(pause * R)) * 0.03)
        for t in cont_words:
            parts.append(("w", t, 0.3))
            parts.append(("p", 0.04))
        parts.append(("p", 0.12))
        return take(R, parts, seed, floor_db=-75)

    cases = []

    def run(name, a, text, words, expect):
        sig = st.put(f"in_guard_{name}", a)
        runs = []
        for label, d in (("given", 0.0), ("minus", -SHIFT), ("plus", SHIFT)):
            ww = shifted(words, d) if d else copy.deepcopy(words)
            tr = {"text": " ".join(x["word"].strip() for x in ww), "segments": [{"start": 0.0, "end": 1.0, "text": "", "avg_logprob": -0.2,
                                                                                 "no_speech_prob": 0.01, "words": ww}], "language": "en"}
            try:
                cut, tr2, info = ending_guard.remove_context(a.copy(), R, text, tr)
                rounds, bad = take_rounds()
                assert not bad, (name, label, bad)
                db = au.frame_db(a, R, 10)
                lo_hi = (int(np.ceil(max(float(tr2["segments"][0]["words"][-1]["start"]) + .06, info["last_word_end"] - .15) / .01)),
                         int(np.floor((info["context_start"] - .02) / .01)))
                mg = float(np.min(np.abs(db[lo_hi[0]:lo_hi[1]].astype(np.float64) - float(np.float32(info["threshold_db"])))))
                assert mg >= MIN_DB, (name, label, mg)
                runs.append({"timing": label, "words": ww, "ok": True, "len": len(cut), "text": tr2["text"],
                             "words_kept": tr2["segments"][0]["words"], "info": {k: num(v) if not isinstance(v, str) else v for k, v in info.items()},
                             "info_dtypes": {k: dtype(v) for k, v in info.items()}, "margin_db": mg})
            except ending_guard.UnsafeBoundary as e:
                take_rounds()
                runs.append({"timing": label, "words": ww, "ok": False, "error": str(e)})
        for r in runs[1:]:
            r["changed"] = (r["ok"], r.get("len"), r.get("error")) != (runs[0]["ok"], runs[0].get("len"), runs[0].get("error"))
        got = runs[0]["error"] if not runs[0]["ok"] else "ok"
        assert got == expect, (name, got)
        cases.append({"name": name, "signal": sig, "sr": R, "text": text, "runs": runs})

    a, w = build(30, ["The", "old", "village."], 0.3)
    run("ok", a, "The old village.", w, "ok")
    wu = copy.deepcopy(w)
    wu[2]["word"] = " village"
    run("ok_unpunctuated", a, "The old village", wu, "ok")
    run("not_recognized", a, "The old village.", w[:-1], "Continuation was not fully recognized")
    # first_context <= 0: the transcript is only the continuation
    wf = copy.deepcopy(w[3:])
    wf[0]["word"] = " village. Meanwhile,"
    run("share_first", a, "The old village.", wf, "Requested ending and continuation share a word timestamp")
    ws = copy.deepcopy(w)
    ws[2]["word"] = " village.Meanwhile,"  # one Whisper word holding the end and the continuation's first token
    ws = ws[:2] + [ws[2]] + ws[4:]
    run("share_word", a, "The old village.", ws, "Requested ending and continuation share a word timestamp")
    wt = copy.deepcopy(w)
    wt[2]["word"] = " town."
    run("final_word", a, "The old village.", wt, "Requested final word was not recognized before the continuation")
    wr = copy.deepcopy(w)
    wr[3]["start"] = wr[2]["end"] - 0.2  # context starts before lo
    run("no_room", a, "The old village.", wr, "No room to separate the requested ending")
    wp = copy.deepcopy(w)
    wp[3]["start"] = wp[2]["end"] - 0.087  # lo < hi but under 6 frames
    run("no_pause", a, "The old village.", wp, "Word timestamps leave no measurable pause")
    a3, w3 = build(32, ["The", "old", "village."], 0.3, noisy=True)
    run("noisy_pause", a3, "The old village.", w3, "No verified quiet pause before the continuation")
    errs = {r["runs"][0].get("error") for r in cases}
    assert len(errs - {None}) == 6, errs
    out["remove_context"] = cases
    return out


# ---------------------------------------------------------------- delivery


def delivery_cases(st, delivery, engines, au, librosa_version):
    out = {"librosa": librosa_version, "default_voice_settings": delivery.DEFAULT_VOICE_SETTINGS}
    # resolve: every built-in spec's parameter keys, several settings, advanced params winning
    specs = [{"id": e.id, "keys": [p.key for p in e.params]} for e in engines.BUILTIN_ENGINES]
    vss = [None, {}, {"stability": 0.0}, {"stability": 1.0, "style": 1.0}, {"similarity": 0.0, "style": 0.55},
           {"stability": 0.28, "similarity": 1.0, "style": 0.31}, {"stability": 1, "similarity": 0, "style": 0}, {"stability": 0.9, "style": 0.0},
           {"stability": 0.15, "style": 0.95}, {"similarity": 0.34, "style": 0.77}]
    bases = [{}, {"temperature": 0.9, "top_k": None}, {"top_p": 0.8, "seed": 3}, None]
    qcs = [{"max_wer": 0.12, "min_speaker_similarity": 0.85, "max_internal_silence": 0.9}, {"max_wer": 0.2}]
    res = []
    for sp in specs:
        spec = type("Spec", (), {"params": [type("P", (), {"key": k}) for k in sp["keys"]]})
        for vs in vss:
            for base in bases:
                for qc in qcs:
                    p, q = delivery.resolve(copy.deepcopy(vs), spec, copy.deepcopy(base), qc)
                    rounds, bad = take_rounds()
                    assert not bad, (sp, vs, bad)
                    res.append({"keys": sp["keys"], "vs": vs, "base": base, "qc": qc, "params": p, "qc_out": q,
                                "types": {k: type(v).__name__ for k, v in p.items()}})
    out["resolve"] = res
    out["qwen_param_keys"] = [p.key for p in engines.QWEN_PARAMS]

    def var(i, score, ex=None):
        v = {"index": i, "score": score}
        if ex is not None:
            v["expressiveness"] = ex
        return v

    profiles = [
        {"active_variant": 1, "variants": [var(0, 0.9, 0.1), var(1, 0.95, 0.4), var(2, 0.8, 0.95), var(3, 0.6, 1.0)]},
        {"variants": [var(0, 0.9, 0.5), var(1, 0.9, 0.7), var(2, 0.9, 0.7), var(3, 0.9, 0.2)]},  # ties: first wins
        {"active_variant": 2, "variants": [var(0, 0.9, 0.1), var(1, 0.9, 0.2), var(2, 0.9, 0.9)]},  # moderate, nothing livelier but itself
        {"active_variant": 0, "variants": [var(0, 0.9), var(1, 0.9)]},  # no expressiveness tags
        {"active_variant": 0, "variants": []},
        {},
        {"active_variant": 1, "variants": [var(0, 0.5, 0.0), var(1, 0.74, 0.25), var(2, 0.75, 0.75), var(3, 0.76, 0.5)]},
        {"active_variant": 0, "variants": [var(0, 1.0, 0.3), var(1, 1.0, 0.65), var(2, 1.0, 0.0)]},  # equal distances to the middle
    ]
    pv = []
    for prof in profiles:
        for style in (0.0, 0.1999, 0.2, 0.35, 0.4999, 0.5, 0.8, 1.0):
            pv.append({"profile": prof, "style": style, "out": delivery.pick_variant(copy.deepcopy(prof), style)})
    out["pick_variant"] = pv
    ex = [{"pitch_st": 0.0, "pitch_range": 0.0, "dyn_db": 0.0}, {"pitch_st": 2.5, "pitch_range": 8.0, "dyn_db": 6.0},
          {"pitch_st": 1.234567, "pitch_range": 3.3, "dyn_db": 9.87}, {"pitch_st": 0.1, "pitch_range": 0.7, "dyn_db": 0.3}]
    out["expressiveness"] = [{"in": p, "out": delivery.expressiveness(p)} for p in ex]
    return out


def prosody_cases(st, delivery, au, librosa):
    from librosa.core import pitch as lp

    cases = []

    class Knife(Exception):
        pass

    def one(name, a, sr, expect_zero):
        a = np.asarray(a, dtype=np.float32)
        p = delivery.prosody(a, sr)
        x = au.resample(a, sr, 16000)
        c = {"name": name, "signal": a, "sr": sr, "x": x,
             "out": {k: num(v) for k, v in p.items()}, "dtypes": {k: dtype(v) for k, v in p.items()}}
        if len(x) >= 16000 * 0.8:
            f0 = librosa.yin(x, fmin=60, fmax=420, sr=16000, frame_length=1024, hop_length=320)
            db = au.frame_db(x, 16000)
            n = min(len(f0), len(db))
            f0, db = f0[:n], db[:n]
            thr = np.percentile(db, 90) - 20
            voiced = (db > thr) & (f0 > 62) & (f0 < 410)
            c.update(f0=f0, f0_dtype=str(f0.dtype), db_dtype=str(db.dtype), voiced=[bool(v) for v in voiced],
                     n_frames=int(n))
            # margins: db against its threshold, f0 against 62/410, the trough threshold, |st| against 12
            m = {"db": float(np.min(np.abs(db.astype(np.float64) - float(thr)))),
                 "f0": float(np.min(np.minimum(np.abs(f0 - 62), np.abs(f0 - 410))))}
            yp = np.pad(x, (512, 512))
            fr = librosa.util.frame(yp, frame_length=1024, hop_length=320)
            yf = lp._cumulative_mean_normalized_difference(fr, int(np.floor(16000 / 420)), min(int(np.ceil(16000 / 60)), 1023))
            c["yin_dtype"] = str(yf.dtype)
            trough = librosa.util.localmin(yf, axis=-2)
            m["trough"] = float(np.min(np.abs(yf[trough] - 0.1))) if trough.any() else math.inf
            if voiced.sum() >= 10:
                st_ = 12 * np.log2(f0[voiced] / np.median(f0[voiced]))
                m["octave"] = float(np.min(np.abs(np.abs(st_) - 12)))
                c["kept"] = int((np.abs(st_) < 12).sum())
            c["margins"] = m
            if not (m["db"] >= MIN_DB and m["f0"] >= 1e-3 and m["trough"] >= 1e-5 and m.get("octave", 1) >= 1e-4):
                raise Knife(m)
        if p["pitch_hz"]:
            dec = {"pitch_hz_1": guarded_round(p["pitch_hz"], 1)}
            for r in (0.5, 1.0, 2.0):
                dec[f"st_over_{r}"] = guarded_round(p["pitch_st"] / r, 2)
            rounds, bad = take_rounds(HALF_PITCH)
            if bad:
                raise Knife(bad)
            c["decisions"] = dec
        else:
            take_rounds()
        assert (p["pitch_hz"] == 0.0) == expect_zero, (name, p)
        return c

    def run(name, make, sr, expect_zero=False):
        """make(k) builds the signal; k counts redesigns of a case that sat on a knife edge."""
        for k in range(40):
            try:
                c = one(name, make(k), sr, expect_zero)
            except Knife:
                take_rounds()
                continue
            c["redesigns"] = k
            c["signal"] = st.put(f"in_pitch_{name}", c["signal"])
            c["x"] = st.put(f"x_pitch_{name}", c["x"])
            if "f0" in c:
                c["f0"] = st.put(f"f0_pitch_{name}", c["f0"])
            cases.append(c)
            return
        raise AssertionError(f"{name}: knife edges after 40 redesigns")

    T = lambda d, sr=24000: np.arange(int(d * sr)) / sr
    for f in (100, 220, 400):
        run(f"tone_{f}", lambda k, f=f: 0.3 * np.sin(2 * np.pi * f * (1 + 0.0007 * k) * T(0.9, 16000) + 0.3) * np.minimum(1, T(0.9, 16000) / 0.02), 16000)
    def glide_up(k):  # at 24 kHz: the resampled path
        t = T(1.2)
        f = 120 * (1 + 0.001 * k) * (2 ** (np.log2(300 / 120) * t / 1.2))
        return 0.3 * np.sin(2 * np.pi * np.cumsum(f) / 24000)
    run("glide_up", glide_up, 24000)
    run("glide_down", lambda k: 0.3 * np.sin(2 * np.pi * np.cumsum(280 - (120 + k) * T(1.0, 16000) / 1.0) / 16000), 16000)
    run("vibrato", lambda k: 0.3 * np.sin(2 * np.pi * np.cumsum(200 * (1 + (0.04 + 0.0005 * k) * np.sin(2 * np.pi * 5.5 * T(1.0, 16000)))) / 16000), 16000)
    run("speech_noise", lambda k: take(16000, [("p", 0.1), ("w", "a", 0.4), ("p", 0.1), ("w", "b", 0.45), ("p", 0.12), ("w", "c", 0.4), ("p", 0.1)],
                                       51 + 100 * k, floor_db=-40, f0=140)[0], 16000)
    run("speech_16k", lambda k: take(16000, [("p", 0.05), ("w", "a", 0.5), ("p", 0.1), ("w", "b", 0.5), ("p", 0.1)], 52 + 100 * k, floor_db=-60, f0=180)[0], 16000)
    run("speech_24k", lambda k: take(24000, [("p", 0.05), ("w", "a", 0.4), ("p", 0.05), ("w", "b", 0.4), ("p", 0.05)], 53 + 100 * k, floor_db=-60, f0=110)[0], 24000)
    run("under_08", lambda k: 0.3 * np.sin(2 * np.pi * 200 * T(0.7, 16000)), 16000, expect_zero=True)
    def few(k):  # one 180 ms tone in a second of quiet noise
        q = np.random.default_rng(50 + k).standard_normal(int(1.1 * 16000)) * 1e-4
        q[8000:10880] += 0.3 * np.sin(2 * np.pi * 200 * np.arange(2880) / 16000)  # 9 frames: loud enough to set the 90th percentile
        return q
    run("few_voiced", few, 16000, expect_zero=True)
    def octave(k):  # 150 Hz with stretches an octave and more apart
        seg = []
        for fq, d in ((150, 0.3), (75 * 0.98, 0.2), (150, 0.25), (330, 0.2), (150, 0.25)):
            seg.append(0.3 * np.sin(2 * np.pi * fq * (1 + 0.001 * k) * T(d, 16000)))
        return np.concatenate(seg)
    run("octave_jumps", octave, 16000)
    return cases


# ---------------------------------------------------------------- noise_limit


def noise_limit_cases(pipeline, au, config, tmp):
    import soundfile as sf

    out = []
    for b in (0.0, 0.01, 0.03, 0.0307, 0.031, 0.05, 0.1, 0.25, 0.7):
        out.append({"profile": {"id": "x", "noise_baseline": b}, "limit": pipeline.noise_limit({"id": "x", "noise_baseline": b})})
    # the baseline arithmetic on files in the temporary data folder (synthetic WAVs)
    rng = np.random.default_rng(60)
    saved = pipeline.db.put_profile
    pipeline.db.put_profile = lambda p: None
    try:
        for k, kinds in enumerate([[], ["speech"], ["speech", "noisy"], ["noisy", "noisy2", "speech"]]):
            pid = f"nl{k}"
            pdir = config.PROFILES_DIR / pid
            pdir.mkdir(parents=True, exist_ok=True)
            vals, variants = [], []
            for j, kind in enumerate(kinds):
                if kind == "speech":
                    a, _ = take(16000, [("p", 0.1), ("w", "a", 0.4), ("p", 0.1), ("w", "b", 0.4), ("p", 0.1)], 61 + k)
                else:
                    a, _ = take(16000, [("p", 0.1), ("w", "a", 0.4), ("a", rng.standard_normal(int((0.2 if kind == "noisy" else 0.35) * 16000)) * 0.1),
                                        ("w", "b", 0.4), ("p", 0.1)], 70 + j + k)
                fname = "clean.wav" if j == 0 else f"v{j}.wav"
                sf.write(pdir / fname, a, 16000, subtype="FLOAT")
                vals.append(au.noise_bursts(*au.load(pdir / fname)))
                if j:
                    variants.append({"file": fname})
            prof = {"id": pid, "variants": variants + [{"file": "missing.wav"}]}
            lim = pipeline.noise_limit(prof)
            rounds, bad = take_rounds()
            assert not bad, bad
            out.append({"vals": vals, "baseline": prof["noise_baseline"], "limit": lim})
    finally:
        pipeline.db.put_profile = saved
    return out


# ---------------------------------------------------------------- main


def main() -> None:
    global np
    assert os.environ.get("HF_HUB_OFFLINE") == "1", "run with HF_HUB_OFFLINE=1"
    tmp = Path(tempfile.mkdtemp(prefix="taketwo-golden."))
    try:
        os.environ["TAKETWO_DATA"] = os.environ["VOICEFORGE_DATA"] = str(tmp)
        os.environ["TRANSFORMERS_OFFLINE"] = "1"
        os.environ["NUMBA_CACHE_DIR"] = str(tmp / "numba")
        sys.path.insert(0, str(BACKEND))
        from app import config

        assert config.DATA_DIR.resolve().is_relative_to(tmp.resolve()), config.DATA_DIR  # never the real library
        import numpy

        np = numpy
        from transformers import WhisperTokenizer

        from app import audio, delivery, ending_guard, engines, ml, pipeline, textproc

        def no_model(*a, **k):
            raise RuntimeError("record_checks.py must never load a model")

        def no_ffmpeg(*a, **k):
            raise RuntimeError("record_checks.py must never run ffmpeg")

        ml.stt_model = no_model
        ml.transcribe = no_model
        audio.subprocess = type("NoSubprocess", (), {"run": staticmethod(no_ffmpeg)})
        tok = WhisperTokenizer.from_pretrained("openai/whisper-large-v3-turbo", local_files_only=True)
        ml._normalizer = tok.normalize
        pipeline.round = delivery.round = ending_guard.round = guarded_round
        import librosa

        if ARR.exists():
            shutil.rmtree(ARR)
        ARR.mkdir()
        st = Store()
        data = {
            "hard_issues": sorted(pipeline.HARD_ISSUES),
            "default_settings": {k: config.DEFAULT_SETTINGS[k] for k in ("max_wer", "min_speaker_similarity", "max_internal_silence")},
            "shift_s": SHIFT,
        }
        data["evaluate"] = evaluate_cases(st, pipeline, audio, ml, textproc, config)
        data["internal_sentence_cut"] = internal_cases(st, pipeline, audio)
        data["ending_guard"] = guard_cases(st, ending_guard, audio)
        data["noise_limit"] = noise_limit_cases(pipeline, audio, config, tmp)
        data["delivery"] = delivery_cases(st, delivery, engines, audio, librosa.__version__)
        data["prosody"] = prosody_cases(st, delivery, audio, librosa)
        assert "app.ml" in sys.modules and "mlx" not in sys.modules and "mlx.core" not in sys.modules, "no model import"
        assert st.bytes <= MAX_BYTES, st.bytes

        # the timing sensitivity table
        rows = []
        for key, field in (("evaluate", None), ("internal_sentence_cut", None)):
            cs = [c for c in data[key] if len(c["runs"]) == 3]
            ch = [(c["name"], r["timing"], [k for k, v in (r["changed"].items() if isinstance(r["changed"], dict) else [("cut", r["changed"])]) if v])
                  for c in cs for r in c["runs"][1:]]
            rows.append((key, len(cs), [x for x in ch if x[2]]))
        cs = data["ending_guard"]["remove_context"]
        rows.append(("remove_context", len(cs), [(c["name"], r["timing"], ["outcome"]) for c in cs for r in c["runs"][1:] if r["changed"]]))
        data["timing_sensitivity"] = [{"function": k, "cases": n, "changed": [{"case": a, "timing": b, "fields": f} for a, b, f in ch]}
                                      for k, n, ch in rows]
        text = json.dumps(data, indent=1, ensure_ascii=False, allow_nan=False) + "\n"
        assert str(tmp) not in text and "/Users/" not in text
        OUT.write_text(text)
        print(f"checks.json: {len(data['evaluate'])} evaluate, {len(data['internal_sentence_cut'])} internal cut, "
              f"{len(cs)} remove_context, {len(data['prosody'])} prosody cases; arrays {st.bytes} bytes")
        print("timing sensitivity (all word times -0.16 s / +0.16 s):")
        for k, n, ch in rows:
            print(f"  {k}: {n} cases x 2 shifts, {len(ch)} changed")
            for a, b, f in ch:
                print(f"    {a} {b}: {', '.join(f)}")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
