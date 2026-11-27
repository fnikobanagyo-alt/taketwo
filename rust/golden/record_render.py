"""Record Python parity vectors for the Rust render pipeline, takes and retakes (step S15, gate G1).

    HF_HUB_OFFLINE=1 backend/.venv/bin/python -B rust/golden/record_render.py

Drives the real pipeline.render_chunk and _guarded_take on a synthetic voice with every model call scripted, and
writes rust/golden/render.json plus arrays under rust/golden/render/ (take inputs as float32, saved parts as the
int16 samples of chunk_NNN.wav, the voice folder's files; float WAV outputs as the SHA-256 of their float32
samples, which Rust must equal bit for bit). Also widen_sentence_pauses alone on the signals of
backend/tests/test_narration_quality.py.
Patched: engines.synthesize and engines.synthesis_report (one scripted take and report per call),
ml.transcribe (scripted transcripts; the call's audio length, rate and 16 kHz length are recorded),
ml.speaker_embedding (scripted 2-component integer vectors, so float32 sums are exact in any order),
ml.cosine (recording pass-through to the real one), pipeline.secrets.randbits (a fixed nonce per case),
pipeline.time.time (t0 + n * step on the n-th reading), db.put_profile (recorded, never written), and the
cancel callable (reads a flag the script sets after the n-th call of a kind).
Safety: the data folder is a fresh temporary folder (checked before any app import), Hugging Face is offline,
no model is loaded (ml.stt_model raises; the Whisper normalizer comes from tokenizer files only), ffmpeg is never
run, numba's cache goes to the temporary folder, and builtins.round in pipeline is guarded (a rounded number
within 1e-9 of a half fails the run; the pitch numbers Rust measures to 4e-5 must stay 2e-4 off a half).
Each case asserts in this recorder that its branch fired.
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
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent
WT = HERE.parent.parent
BACKEND = WT / "backend"
OUT = HERE / "render.json"
ARR = HERE / "render"
MAX_BYTES = 6_000_000
SR = 16000
HALF = 1e-9
HALF_PITCH = 2e-4  # Rust's prosody is within 3.8e-5 of librosa's (S11a); its rounded results must not move
T0, STEP = 1_000_000.0, 1.3  # the clock: secs = round(n * 1.3, 1), never near a half
PID = "rvoice"
FP = [1.0, 0.0]
GOOD, CLOSE, DRIFT = [4.0, 3.0], [12.0, 5.0], [3.0, 4.0]  # cosine 0.8, 12/13, 0.6 against FP

np = None


# ---------------------------------------------------------------- rounding guard

ROUNDS: list[tuple[float, int | None, float]] = []


def guarded_round(x, n=None):
    if isinstance(x, float) or (hasattr(x, "dtype") and np.issubdtype(np.asarray(x).dtype, np.floating)):
        s = 10.0 ** (n or 0)
        v = float(x) * s
        ROUNDS.append((float(x), n, abs(v - math.floor(v) - 0.5) / s))
    return builtins.round(x, n) if n is not None else builtins.round(x)


# ---------------------------------------------------------------- arrays


class Store:
    def __init__(self):
        self.names: dict[str, str] = {}
        self.bytes = 0

    def put(self, data: bytes, ext: str, n: int) -> dict:
        h = hashlib.sha256(data).hexdigest()
        if h not in self.names:
            name = f"{ext}_{h[:16]}.{ext}"
            (ARR / name).write_bytes(data)
            self.bytes += len(data)
            self.names[h] = name
        return {"file": self.names[h], "len": n}

    def f32(self, a) -> dict:
        a = np.ascontiguousarray(np.asarray(a), dtype="<f4")
        return self.put(a.tobytes(), "f32", len(a))

    def i16(self, a) -> dict:
        a = np.ascontiguousarray(np.asarray(a), dtype="<i2")
        return self.put(a.tobytes(), "i16", len(a))


def sha(a) -> dict:
    b = np.ascontiguousarray(np.asarray(a), dtype="<f4").tobytes()
    return {"sha256": hashlib.sha256(b).hexdigest(), "len": len(b) // 4}


# ---------------------------------------------------------------- synthetic speech (as record_checks.py, with pitch controls)


def word_sig(sr, dur, f0, rng, amp=0.3, attack=0.03, decay=0.08, vib=0.02):
    n = int(dur * sr)
    t = np.arange(n) / sr
    v = 1 + vib * np.sin(2 * np.pi * 5 * t)
    sig = np.zeros(n)
    for h in range(1, 9):
        sig += np.sin(2 * np.pi * h * f0 * v * t + rng.uniform(0, 2 * np.pi)) / h
    env = 0.65 + 0.35 * np.sin(2 * np.pi * 4.0 * t + rng.uniform(0, np.pi)) ** 2
    env *= np.minimum(1.0, t / attack) ** 2
    env *= np.minimum(1.0, (dur - t) / decay) ** 2
    return amp * sig / np.max(np.abs(sig)) * env


def take(parts, seed, f0=150.0, jitter=0.08, vib=0.02, sr=SR, floor_db=-70.0):
    """parts: ("w", text, dur, opts) words, ("p", dur) pauses. Returns (float32 signal, Whisper-like words)."""
    rng = np.random.default_rng(seed)
    out, words, t = [], [], 0.0
    for p in parts:
        if p[0] == "w":
            _, text, dur, *rest = p
            o = rest[0] if rest else {}
            w = word_sig(sr, dur, f0 * rng.uniform(1 - jitter, 1 + jitter), rng, o.get("amp", 0.3), decay=o.get("decay", 0.08), vib=vib)
            if "cut" in o:
                w[int(o["cut"] * len(w)):] = 0
            out.append(w)
            if text:
                words.append({"word": " " + text, "start": round(t + 0.02, 2), "end": round(t + dur - 0.02, 2), "p": 0.9})
            t += len(w) / sr
        else:
            out.append(np.zeros(int(p[1] * sr)))
            t += int(p[1] * sr) / sr
    a = np.concatenate(out)
    a = a + rng.standard_normal(len(a)) * 10 ** (floor_db / 20)
    return a.astype(np.float32), words


def transcript(words, text=None):
    heard = " ".join(w["word"].strip() for w in words) if text is None else text
    segs = [{"start": words[0]["start"], "end": words[-1]["end"], "text": heard, "avg_logprob": -0.2, "no_speech_prob": 0.01,
             "words": copy.deepcopy(words)}] if words else []
    return {"text": heard, "segments": segs, "language": "en"}


def spoken(text, last=None, gap=0.05, lead=0.12, tail=0.3, cut=False):
    """Word parts for a sentence: about 0.3 s per word, the last one with a natural (or cut) ending."""
    ws = text.split()
    parts = [("p", lead)]
    for i, w in enumerate(ws):
        dur = 0.18 + 0.03 * (len(w) % 5)
        if i == len(ws) - 1:
            parts.append(("w", w, 0.45, {"cut": 0.8, "decay": 0.01} if cut else {"decay": 0.15}))
        else:
            parts += [("w", w, dur), ("p", gap)]
    parts.append(("p", tail))
    return parts


CONT = ["Meanwhile,", "another", "chapter", "begins."]


def with_context(text, pause=0.35):
    return spoken(text, tail=pause)[:-1] + [("p", pause)] + spoken(" ".join(CONT), lead=0.0)[1:]


# ---------------------------------------------------------------- the harness


class Harness:
    def __init__(self, st, pipeline, au, ml, db, config, sf, ending_guard):
        self.st, self.pipeline, self.au, self.ml, self.db, self.config, self.sf = st, pipeline, au, ml, db, config, sf
        self.ending_guard = ending_guard
        self.cases = []
        self.real_cosine = ml.cosine
        self.voice_files = {}

    def voice(self, variants, files):
        """The voice folder: `files` name -> float32 signal at SR (saved as PCM16 like a real clip) plus fingerprint.npy."""
        pdir = self.config.PROFILES_DIR / PID
        if pdir.exists():
            shutil.rmtree(pdir)
        pdir.mkdir(parents=True)
        stored = {}
        for name, a in files.items():
            self.au.save(pdir / name, a, SR)
            stored[name] = self.st.put((pdir / name).read_bytes(), "wav", len(a))
        np.save(pdir / "fingerprint.npy", np.array(FP, np.float32))
        stored["fingerprint.npy"] = self.st.put((pdir / "fingerprint.npy").read_bytes(), "npy", len(FP))
        return stored

    def run(self, name, *, text, takes, transcripts, embeddings, voice, profile=None, s=None, chunk=None, others=(),
            ci=0, nonce=12345, regen_round=0, gen_extra=None, cancel_after=None, pre_files=None, check=None,
            fired=None):
        """takes: (signal, report) per synthesis call; transcripts and embeddings in call order.
        cancel_after: (kind, n) sets the cancel flag after the n-th call of kind (n 0: set before the start)."""
        pl, au = self.pipeline, self.au
        pl._pitch_cache.clear()
        stored_voice = self.voice(*voice)
        prof = {"id": PID, "name": "Render voice", "variants": copy.deepcopy(voice[0]), "self_similarity": 0.8, "speech_rate_wps": 2.6, **(profile or {})}
        settings = {**self.config.DEFAULT_SETTINGS, **(s or {})}
        part = {"i": ci, "text": text, "pause_after": "end", "status": "pending", **(chunk or {})}
        chunks = [dict(o) for o in others]
        chunks.insert(ci, part)
        gen = {"id": "g_" + name, "profile_id": PID, "engine": "qwen3-tts-1.7b", "language": "en", "params": {},
               "chunks": chunks, **(gen_extra or {})}
        gdir = self.config.GENERATIONS_DIR / gen["id"]
        if gdir.exists():
            shutil.rmtree(gdir)
        gdir.mkdir(parents=True)
        pre = {}
        for fname, data in (pre_files or {}).items():
            (gdir / fname).write_bytes(data)
            pre[fname] = data.hex()
        inputs = {"gen": copy.deepcopy(gen), "ci": ci, "profile": copy.deepcopy(prof), "settings": settings, "nonce": nonce,
                  "regen_round": regen_round, "clock": [T0, STEP], "cancel_after": list(cancel_after) if cancel_after else None,
                  "pre_files": pre}
        flag = [False]
        counts = {"synthesize": 0, "transcribe": 0, "embedding": 0, "clock": 0}
        calls, puts = [], []
        take_q = [(np.asarray(a, np.float32), sr, rep) for a, sr, rep in takes]
        tr_q, emb_q = [copy.deepcopy(t) for t in transcripts], [list(e) for e in embeddings]
        script = {"takes": [{"audio": self.st.f32(a), "sr": sr, "report": rep} for a, sr, rep in take_q],
                  "transcripts": copy.deepcopy(tr_q), "embeddings": copy.deepcopy(emb_q)}

        def count(kind):
            counts[kind] += 1
            if cancel_after and cancel_after[0] == kind and cancel_after[1] == counts[kind]:
                flag[0] = True

        if cancel_after and cancel_after[1] == 0:
            flag[0] = True
        last = {}

        def synth(engine, text_, ref_wav, ref_text, language, params, seed, expected_seconds):
            calls.append({"synthesize": {"engine": engine, "text": text_, "ref_file": Path(ref_wav).name, "ref_text": ref_text,
                                         "language": language, "params": copy.deepcopy(params), "seed": seed,
                                         "expected_seconds": expected_seconds}})
            a, sr, rep = take_q.pop(0)
            last.clear()
            last.update(copy.deepcopy(rep))
            count("synthesize")
            return a.copy(), sr

        def transcribe(a, sr, language="en", words=False):
            calls.append({"transcribe": {"len": len(a), "sr": sr, "len_16k": len(au.resample(a, sr, 16000)), "language": language,
                                         "words": words}})
            t = tr_q.pop(0)
            count("transcribe")
            return t

        def embedding(a, sr):
            calls.append({"embedding": {"len": len(a), "sr": sr}})
            e = emb_q.pop(0)
            count("embedding")
            return np.array(e, np.float32)

        cos_calls = []

        def cosine(a, b):
            v = self.real_cosine(a, b)
            cos_calls.append(v)
            return v

        def clock():
            t = T0 + counts["clock"] * STEP
            count("clock")
            return t

        pl.engines.synthesize = synth
        pl.engines.synthesis_report = lambda: dict(last)
        pl.ml.transcribe = transcribe
        pl.ml.speaker_embedding = embedding
        pl.ml.cosine = cosine
        pl.secrets = types.SimpleNamespace(randbits=lambda k: nonce if k == 32 else 1 / 0)
        pl.time = types.SimpleNamespace(time=clock)
        pl.db.put_profile = lambda p: puts.append(copy.deepcopy(p))
        ROUNDS.clear()
        pros = []
        real_prosody = pl.delivery.prosody
        in_ref = [False]
        real_ref_pitch = REAL["_ref_pitch"]

        def ref_pitch(path):
            in_ref[0] = True
            try:
                v = real_ref_pitch(path)
            finally:
                in_ref[0] = False
            pros.append(("ref", v))
            return v

        def prosody(a, sr):
            r = real_prosody(a, sr)
            if not in_ref[0]:
                pros.append(("take", r))
            return r

        pl._ref_pitch = ref_pitch
        pl.delivery.prosody = prosody
        result = {}
        try:
            ch = pl.render_chunk(gen, ci, prof, settings, regen_round=regen_round, cancel=lambda: flag[0])
            assert ch is gen["chunks"][ci]
            result["part"] = json.loads(json.dumps(ch))
        except Exception as e:  # noqa: BLE001 - the error is the vector
            result["error"] = {"type": type(e).__name__, "message": str(e)}
            assert type(e).__name__ in ("Cancelled", "ValueError"), (name, repr(e))
            assert gen["chunks"][ci] == inputs["gen"]["chunks"][ci], "nothing is written to the part on an error"
        finally:
            pl.delivery.prosody = real_prosody
            pl._ref_pitch = real_ref_pitch
        bad = [r for r in ROUNDS if 0 < r[2] < HALF]
        assert not bad, (name, bad)
        self.pitch_margins(name, pros, prof, settings)
        assert not take_q and not tr_q and not emb_q, (name, "script not used up", len(take_q), len(tr_q), len(emb_q))
        files = {}
        for f in sorted(gdir.iterdir()):
            if f.name in pre and f.read_bytes() == bytes.fromhex(pre[f.name]):
                files[f.name] = {"unchanged": True}
            elif f.suffix == ".json":
                files[f.name] = {"json": json.loads(f.read_text())}
            elif f.name.startswith("chunk_"):
                a, r = self.sf.read(str(f), dtype="int16")
                info = self.sf.info(str(f))
                assert info.subtype == "PCM_16"
                files[f.name] = {"pcm16": self.st.i16(a), "sr": r}
            else:
                a, r = self.sf.read(str(f), dtype="float32")
                assert self.sf.info(str(f)).subtype == "FLOAT", f.name
                files[f.name] = {"float": sha(a), "sr": r}
        result.update(calls=calls, cosines=cos_calls, files=files, profile_after=json.loads(json.dumps(prof)), put_profile=puts,
                      clock_reads=counts["clock"])
        case = {"name": name, "inputs": inputs, "script": script, "voice": stored_voice, "result": result}
        if check:
            check(result)
        assert fired, name
        case["fired"] = fired
        self.cases.append(case)
        return result

    def pitch_margins(self, name, pros, prof, s):
        """The pitch numbers Rust measures within 4e-5 must keep their rounded value and decision."""
        ref = None
        for kind, v in pros:
            if kind == "ref":
                ref = v
                continue
            if ref:
                x = v["pitch_st"] / ref * 100
                assert abs(x - math.floor(x) - 0.5) / 100 > HALF_PITCH, (name, "liveliness near a half", v, ref)
            if v["pitch_hz"]:
                x = v["pitch_hz"] * 10
                assert abs(x - math.floor(x) - 0.5) / 10 > HALF_PITCH * 10, (name, "pitch_hz near a half", v)


REAL = {}


def rep(a, stop="eos"):
    return {"tokens": len(a) // 1280, "token_limit": 120, "raw_samples": len(a), "sample_rate": SR, "stop_reason": stop,
            "sampler": {"temperature": 0.7, "top_p": 0.9, "top_k": 40}, "generation_route": "scripted", "decoder": "scripted"}


def attempts_issues(r):
    return [a["issues"] for a in r["part"]["attempts"]]


def cases(h):
    pl = h.pipeline
    A = "The river ran past the old mill."
    B = "Wait for it, then run home."
    C = "Pull half. One nudge more."
    D = "Come back soon."
    ref0, _ = take(spoken("Our voice reads a calm line of text here."), 101, jitter=0.10)
    ref1, _ = take(spoken("A second clip of the same calm voice."), 102, f0=145.0, jitter=0.14)
    ref2, _ = take(spoken("A third and quieter clip for the voice."), 103, f0=152.0, jitter=0.06)
    clean, _ = take(spoken("A clean room tone sample for the voice."), 104)
    V1 = ([{"index": 0, "file": "ref_0.wav", "text": "Our voice reads a calm line of text here.", "score": 0.9}],
          {"clean.wav": clean, "ref_0.wav": ref0})
    V3 = ([{"index": 0, "file": "ref_0.wav", "text": "Our voice reads a calm line of text here.", "score": 0.9},
           {"index": 1, "file": "ref_1.wav", "text": "A second clip of the same calm voice.", "score": 0.7},
           {"index": 2, "file": "ref_2.wav", "text": "A third and quieter clip for the voice.", "score": 0.8}],
          {"clean.wav": clean, "ref_0.wav": ref0, "ref_1.wav": ref1, "ref_2.wav": ref2})
    a_ok, w_ok = take(spoken(A), 1)
    a_ok2, w_ok2 = take(spoken(A), 2)
    a_cut, w_cut = take(spoken(A, cut=True), 3)
    a_flat, w_flat = take(spoken(A), 4, jitter=0.0, vib=0.0)
    a_hi, w_hi = take(spoken(A), 5, f0=190.0)
    a_long, w_long = take([("p", 0.12), ("w", "Wait", 0.25), ("p", 0.05), ("w", "for", 0.2), ("p", 0.05), ("w", "it,", 0.22),
                           ("p", 1.05), ("w", "then", 0.25), ("p", 0.05), ("w", "run", 0.22), ("p", 0.05),
                           ("w", "home.", 0.45, {"decay": 0.15}), ("p", 0.3)], 6)
    a_two, w_two = take([("p", 0.3), ("w", "Pull", 0.4), ("p", 0.04), ("w", "half.", 0.5, {"decay": 0.08}), ("p", 0.08),
                         ("w", "One", 0.35), ("p", 0.04), ("w", "nudge", 0.4), ("p", 0.04), ("w", "more.", 0.5, {"decay": 0.15}),
                         ("p", 0.3)], 7)
    a_short, w_short = take(spoken(D), 8)
    a_empty = take([("w", "Hi", 0.2)], 9)[0]
    T = lambda w, text=None: transcript(w, text)
    wrong = T(w_ok, "The river ran past the cold hill.")
    Rok = lambda a: (a, SR, rep(a))
    Rlim = lambda a: (a, SR, rep(a, "length_limit_reached"))

    # --- plain path (ending guard off)
    r = h.run("clean_first", text=A, takes=[Rok(a_ok)], transcripts=[T(w_ok)], embeddings=[GOOD], voice=V1,
              chunk={"audio_revision": 2, "accepted": True, "attempts": [{"seed": 1}], "used_seeds": [7]},
              pre_files={"context_chunk_000.wav": b"old context"}, fired="one clean take")
    p = r["part"]
    assert p["status"] == "ok" and attempts_issues(r) == [[]] and p["audio_revision"] == 3 and p["accepted"] is None
    assert p["words"] and p["context_file"] is None and "context_chunk_000.wav" not in r["files"] and len(r["put_profile"]) == 1
    assert p["attempts"][0]["liveliness"] is not None and p["pitch_hz"] is None

    r = h.run("cutoff_then_clean", text=A, takes=[Rok(a_cut), Rok(a_ok)], transcripts=[T(w_cut), T(w_ok)], embeddings=[GOOD, GOOD],
              voice=V1, profile={"noise_baseline": 0.01}, fired="cut_off then clean")
    assert attempts_issues(r)[0] == ["cut_off"] and attempts_issues(r)[1] == [] and r["put_profile"] == []
    assert r["part"]["attempts"][0]["temperature"] == r["part"]["attempts"][1]["temperature"]  # ending-only: no decay

    r = h.run("all_flagged_one_clip", text=A, takes=[Rok(a_ok), Rok(a_ok), Rok(a_ok), Rok(a_cut)],
              transcripts=[T(w_ok), wrong, T(w_ok), T(w_cut)], embeddings=[DRIFT, GOOD, DRIFT, GOOD], voice=V1, fired="all flagged")
    p = r["part"]
    assert p["status"] == "flagged" and all(i for i in attempts_issues(r))
    sc = [a["score"] for a in p["attempts"]]
    assert sc[0] == sc[2] and p["chosen_seed"] == p["attempts"][0]["seed"]  # equal scores: the first is kept
    assert len({a["ref"] for a in p["attempts"]}) == 1  # one clip: no switch
    temps = [a["temperature"] for a in p["attempts"]]
    assert temps[1] < temps[0] and temps[2] < temps[1] and temps[3] < temps[2]  # decay on non-ending failures

    r = h.run("compare_two", text=A, takes=[Rok(a_ok), Rok(a_ok2)], transcripts=[T(w_ok), T(w_ok2)], embeddings=[GOOD, CLOSE],
              voice=V1, s={"compare_takes": 2}, fired="compare 2: closer voice kept")
    assert attempts_issues(r) == [[], []] and r["part"]["chosen_seed"] == r["part"]["attempts"][1]["seed"]

    r = h.run("compare_three", text=A, takes=[Rok(a_ok), Rok(a_cut), Rok(a_ok2), Rok(a_ok)],
              transcripts=[T(w_ok), T(w_cut), T(w_ok2), T(w_ok)], embeddings=[CLOSE, GOOD, GOOD, GOOD], voice=V1,
              s={"compare_takes": 3}, fired="compare 3: stops after the third clean take")
    assert len(r["part"]["attempts"]) == 4 and r["part"]["chosen_seed"] == r["part"]["attempts"][0]["seed"]

    r = h.run("temperature_floor", text=A, takes=[Rok(a_ok)] * 4, transcripts=[wrong] * 4, embeddings=[GOOD] * 4, voice=V1,
              gen_extra={"params": {"temperature": 0.35, "top_k": 30}}, fired="temperature decays to the 0.3 floor")
    temps = [a["temperature"] for a in r["part"]["attempts"]]
    assert temps[0] == 0.35 and temps[-1] == 0.3, temps

    r = h.run("last_take_other_clip", text=A, takes=[Rok(a_ok), Rok(a_ok2), Rok(a_ok)], transcripts=[wrong, wrong, T(w_ok)],
              embeddings=[GOOD, GOOD, GOOD], voice=V3, s={"max_attempts": 3},
              profile={"ref_text_override": {"2": "A third, re-typed clip text."}}, fired="last take switches to the next-best clip")
    refs = [a["ref"] for a in r["part"]["attempts"]]
    synth = [c["synthesize"] for c in r["calls"] if "synthesize" in c]
    assert refs == [0, 0, 2] and synth[-1]["ref_file"] == "ref_2.wav" and synth[-1]["ref_text"] == "A third, re-typed clip text."
    lv = [a["liveliness"] for a in r["part"]["attempts"]]
    assert lv[2] != lv[0], lv  # the reference pitch was measured again on the new clip

    r = h.run("one_take_no_switch", text=A, takes=[Rok(a_ok)], transcripts=[wrong], embeddings=[GOOD], voice=V3,
              s={"max_attempts": 1}, fired="one take: no clip switch")
    assert [a["ref"] for a in r["part"]["attempts"]] == [0] and r["part"]["status"] == "flagged"

    r = h.run("length_limit", text=A, takes=[Rlim(a_cut), Rlim(a_ok), Rok(a_ok2)], transcripts=[T(w_cut), T(w_ok), T(w_ok2)],
              embeddings=[GOOD] * 3, voice=V1, fired="length_limit_reached: cut_off once, budget x 1.3")
    p = r["part"]
    assert attempts_issues(r)[0] == ["cut_off"] and attempts_issues(r)[1] == ["cut_off"] and p["duration_budget"] == 1.3 * 1.3
    es = [a["expected_seconds"] for a in p["attempts"]]
    assert es[1] == es[0] * 1.3 and abs(es[2] - es[0] * 1.69) < 1e-12

    r = h.run("budget_cap", text=A, takes=[Rlim(a_ok), Rok(a_ok2)], transcripts=[T(w_ok), T(w_ok2)], embeddings=[GOOD] * 2,
              voice=V1, chunk={"duration_budget": 3.5}, fired="budget capped at 4.0")
    assert r["part"]["duration_budget"] == 4.0
    r = h.run("budget_stored_high", text=A, takes=[Rok(a_ok)], transcripts=[T(w_ok)], embeddings=[GOOD], voice=V1,
              chunk={"duration_budget": 9}, fired="stored budget clamped to 4.0")
    assert r["part"]["duration_budget"] == 4.0
    r = h.run("budget_stored_low", text=A, takes=[Rok(a_ok)], transcripts=[T(w_ok)], embeddings=[GOOD], voice=V1,
              chunk={"duration_budget": 0.5}, fired="stored budget clamped to 1.0")
    assert r["part"]["duration_budget"] == 1.0

    other = {"i": 0, "text": "Before.", "pause_after": "sentence", "status": "ok"}
    base = (1234 + 4294967000 + 1000 + 2 * 97) % 2**32
    r = h.run("seed_schedule", text=A, takes=[Rok(a_ok), Rok(a_ok2), Rok(a_ok)], transcripts=[wrong, wrong, T(w_ok)],
              embeddings=[GOOD] * 3, voice=V1, ci=1, others=[other], nonce=4294967000, regen_round=2,
              chunk={"used_seeds": [base, 5], "attempts": [{"seed": base + 1}]}, fired="seed schedule with collisions")
    seeds = [a["seed"] for a in r["part"]["attempts"]]
    assert seeds == [base + 2, base + 3, base + 4], (seeds, base)
    assert r["part"]["used_seeds"] == sorted(r["part"]["used_seeds"]) and base in r["part"]["used_seeds"]
    r = h.run("seed_wrap", text=A, takes=[Rok(a_ok)], transcripts=[T(w_ok)], embeddings=[GOOD], voice=V1,
              nonce=2**32 - 1 - 1234, chunk={"used_seeds": [2**32 - 1]}, fired="seed wraps at 2^32")
    assert r["part"]["attempts"][0]["seed"] == 0

    a24, w24 = take(spoken(A), 22, sr=24000)
    r = h.run("clean_24k", text=A, takes=[(a24, 24000, {**rep(a24), "sample_rate": 24000})], transcripts=[T(w24)], embeddings=[GOOD],
              voice=V1, fired="a 24 kHz take is resampled to 16 kHz before transcription")
    t = r["calls"][1]["transcribe"]
    assert t["sr"] == 24000 and t["len"] != t["len_16k"] and r["part"]["sr"] == 24000 and attempts_issues(r) == [[]]
    r = h.run("clean_beats_lower_failed", text=A, takes=[Rok(a_ok), Rok(a_hi)], transcripts=[T(w_ok), T(w_hi)], embeddings=[DRIFT, GOOD],
              voice=V1, profile={"pitch_hz": 150.0}, s={"max_pitch_offset_st": 6.0}, fired="a clean take outranks a failed take with a lower score")
    at = r["part"]["attempts"]
    assert at[0]["issues"] and not at[1]["issues"] and at[1]["score"] > at[0]["score"] and r["part"]["chosen_seed"] == at[1]["seed"]

    # --- pitch
    r = h.run("flat_then_lively", text=A, takes=[Rok(a_flat), Rok(a_ok)], transcripts=[T(w_flat), T(w_ok)], embeddings=[GOOD] * 2,
              voice=V1, gen_extra={"voice_settings": {"style": 0.6}}, fired="flat take retaken; style term in the score")
    assert attempts_issues(r)[0] == ["flat"] and r["part"]["attempts"][0]["liveliness"] < 0.65
    r = h.run("off_pitch_median", text=A, takes=[Rok(a_hi), Rok(a_ok)], transcripts=[T(w_hi), T(w_ok)], embeddings=[GOOD] * 2,
              voice=V1, profile={"pitch_hz": 150.0}, fired="off_pitch against the voice median")
    assert "off_pitch" in attempts_issues(r)[0] and r["part"]["pitch_hz"] is not None and attempts_issues(r)[1] == []
    nb = [{"i": 0, "text": "Before.", "status": "ok", "file": "chunk_000.wav", "pitch_hz": 150.0},
          {"i": 2, "text": "After.", "status": "ok", "file": "chunk_002.wav", "pitch_hz": 118.0}]
    r = h.run("off_pitch_neighbours", text=A, takes=[Rok(a_ok), Rok(a_ok2)], transcripts=[T(w_ok), T(w_ok2)],
              embeddings=[GOOD] * 2, voice=V1, ci=1, others=nb, profile={"pitch_hz": 150.0}, s={"max_attempts": 2},
              fired="off_pitch against a neighbour part")
    assert all("off_pitch" in i for i in attempts_issues(r))
    r = h.run("off_pitch_all_closest", text=A, takes=[Rok(a_hi), Rok(a_ok), Rok(a_hi)], transcripts=[T(w_hi), T(w_ok), T(w_hi)],
              embeddings=[GOOD] * 3, voice=V1, profile={"pitch_hz": 110.0}, s={"max_attempts": 3},
              fired="every take off pitch: the closest pitch is kept")
    assert r["part"]["issues"] == ["off_pitch"] and r["part"]["chosen_seed"] == r["part"]["attempts"][1]["seed"]
    r = h.run("off_pitch_limit_setting", text=A, takes=[Rok(a_hi)], transcripts=[T(w_hi)], embeddings=[GOOD], voice=V1,
              profile={"pitch_hz": 150.0}, s={"max_pitch_offset_st": 6.0}, fired="max_pitch_offset_st from settings")
    assert attempts_issues(r) == [[]] and r["part"]["pitch_hz"]
    r = h.run("no_stored_median", text=A, takes=[Rok(a_hi)], transcripts=[T(w_hi)], embeddings=[GOOD], voice=V1,
              fired="no gate without a stored pitch_hz")
    assert r["part"]["pitch_hz"] is None and attempts_issues(r) == [[]]
    r = h.run("short_no_prosody", text=D, takes=[Rok(a_short)], transcripts=[T(w_short)], embeddings=[GOOD], voice=V1,
              profile={"pitch_hz": 150.0}, fired="no prosody at 1.5 s or shorter")
    assert r["part"]["attempts"][0]["liveliness"] is None and r["part"]["pitch_hz"] is None and r["part"]["attempts"][0]["dur"] <= 1.5

    # --- repairs and word times
    r = h.run("long_silence_repair", text=B, takes=[Rok(a_long)] * 2, transcripts=[T(w_long)] * 2, embeddings=[GOOD] * 2,
              voice=V1, s={"max_attempts": 2}, fired="long_silence repaired by shorten_gaps")
    assert all("long_silence" in i for i in attempts_issues(r)) and r["part"]["words"] is None and "long_silence" not in r["part"]["issues"]
    for pause in (300, 0):
        r = h.run(f"inner_pause_{pause}", text=C, takes=[Rok(a_two)], transcripts=[T(w_two)], embeddings=[GOOD], voice=V1,
                  s={"inner_sentence_pause_ms": pause}, fired=f"inner_sentence_pause_ms {pause}")
        assert r["part"]["words"] and attempts_issues(r) == [[]], attempts_issues(r)
        r_on = r if pause else r_on
    assert r_on["part"]["words"][2]["start"] > r["part"]["words"][2]["start"] + 0.1  # widened: later words moved
    r = h.run("no_word_times", text=A, takes=[Rok(a_ok)], transcripts=[{"text": A, "segments": [], "language": "en"}],
              embeddings=[GOOD], voice=V1, fired="no words without trim_start")
    assert r["part"]["words"] is None and "trim_start" not in r["part"]["ending"]
    r = h.run("empty_only", text=A, takes=[Rok(a_empty)], transcripts=[], embeddings=[], voice=V1, s={"max_attempts": 1},
              fired="empty take: evaluate does not transcribe")
    assert r["part"]["issues"] == ["empty"] and r["part"]["ending"] is None and r["part"]["heard"] == ""

    # --- cancel (plain)
    for nm, ca, ntk, ntr, nem in (("cancel_before_first", ("clock", 0), 0, 0, 0), ("cancel_after_synthesis", ("synthesize", 1), 1, 0, 0),
                                  ("cancel_after_evaluate", ("embedding", 1), 1, 1, 1), ("cancel_before_second", ("clock", 2), 1, 1, 1)):
        r = h.run(nm, text=A, takes=[Rok(a_cut)] * ntk, transcripts=[T(w_cut)] * ntr, embeddings=[GOOD] * nem, voice=V1,
                  cancel_after=ca, fired=f"cancel {ca}")
        assert r["error"]["type"] == "Cancelled"

    # --- ending guard
    G = {"experimental_qwen_ending_guard": True}
    g_ok, gw_ok = take(with_context(A), 11)
    g_ok2, gw_ok2 = take(with_context(A), 12)
    unsafe_tr = T(gw_ok[:-1])  # the continuation is not fully heard
    for nm, val in (("guard_false_string", "false"), ("guard_one_int", 1)):
        r = h.run(nm, text=A, takes=[Rok(a_ok)], transcripts=[T(w_ok)], embeddings=[GOOD], voice=V1,
                  s={"experimental_qwen_ending_guard": val}, fired=f"guard not enabled by {val!r}")
        assert r["part"]["ending_guard"] is None and r["calls"][0]["synthesize"]["text"] == A
    r = h.run("guard_one_sentence", text=A, takes=[Rok(g_ok)], transcripts=[T(gw_ok)], embeddings=[GOOD], voice=V1, s=G,
              pre_files={"context_chunk_000.wav": b"old"}, fired="guarded single sentence")
    p = r["part"]
    assert p["status"] == "ok", p["attempts"]
    assert p["context_file"] == "context_chunk_000.wav" and "float" in r["files"]["context_chunk_000.wav"] and p["ending_guard"]["seed"]
    assert r["calls"][0]["synthesize"]["text"] == A + "\n\n" + pl.ending_guard.CONTINUATION
    s1, sw1 = take(with_context("Pull half."), 13)
    s2, sw2 = take(with_context("One nudge more."), 14)
    s2b, sw2b = take(with_context("One nudge more."), 16)
    r = h.run("guard_two_sentences", text=C, takes=[Rok(s1), Rlim(s2), Rok(s1), Rok(s2b)], transcripts=[T(sw1), T(sw2), T(sw1), T(sw2b)],
              embeddings=[GOOD, GOOD], voice=V1,
              s={**G, "sentence_pause_ms": 700}, fired="guarded two sentences, a pause gap, per-sentence seeds, length limit in one")
    g = r["part"]["ending_guard"]
    assert len(g["sentences"]) == 2 and g["sentences"][1]["offset"] > g["sentences"][0]["cut_seconds"] + 0.1, g["sentences"] and r["part"]["attempts"][0]["synthesis"]["stop_reason"] == "length_limit_reached"
    assert "sentences" in r["part"]["attempts"][0]["synthesis"]
    seeds = [c["synthesize"]["seed"] for c in r["calls"] if "synthesize" in c]
    assert seeds[1] == (seeds[0] + 2654435761) % 2**32
    r = h.run("guard_local_retry", text=C, takes=[Rok(s1), Rok(s2), Rok(s2b)], transcripts=[T(sw1), T(sw2[:-1]), T(sw2b)],
              embeddings=[GOOD], voice=V1, s=G, fired="second sentence passes on a local retry")
    g = r["part"]["ending_guard"]
    assert len(g["sentences"][0]["boundary_attempts"]) == 1 and len(g["sentences"][1]["boundary_attempts"]) == 2
    r = h.run("guard_unsafe_mid", text=A, takes=[Rok(g_ok), Rok(g_ok2)], transcripts=[unsafe_tr, T(gw_ok2)], embeddings=[GOOD],
              voice=V1, s=G, fired="UnsafeBoundary on a middle take, loop goes on")
    assert attempts_issues(r)[0] == ["ending_boundary"] and attempts_issues(r)[1] == [] and "rejected_context_000.wav" not in r["files"]
    r = h.run("guard_unsafe_last", text=A, takes=[Rok(g_ok), Rlim(g_ok2)], transcripts=[T(gw_ok), unsafe_tr],
              embeddings=[DRIFT], voice=V1, s={**G, "max_attempts": 2}, fired="UnsafeBoundary on the last take: rejected files")
    assert attempts_issues(r)[1] == ["ending_boundary"] and "rejected_context_000.json" in r["files"] and r["part"]["duration_budget"] == 1.3
    r = h.run("guard_all_unsafe_fallback", text=A, takes=[Rok(g_ok), Rok(g_ok2), Rlim(a_ok)], transcripts=[unsafe_tr, unsafe_tr, T(w_ok)],
              embeddings=[GOOD], voice=V1, s={**G, "max_attempts": 2}, fired="every take unsafe: plain fallback take")
    p = r["part"]
    assert p["status"] == "flagged" and "ending_unverified" in p["issues"] and p["attempts"][-1]["ending_guard_fallback"]
    assert p["attempts"][-1]["seed"] == p["attempts"][-2]["seed"] + 1 and p["context_file"] is None and "cut_off" in p["issues"]
    r = h.run("guard_all_unsafe_old_file", text=A, takes=[Rok(g_ok), Rok(g_ok2)], transcripts=[unsafe_tr, unsafe_tr], embeddings=[],
              voice=V1, s={**G, "max_attempts": 2}, pre_files={"chunk_000.wav": b"previous clip"},
              fired="every take unsafe with an old part: ValueError")
    assert r["error"]["type"] == "ValueError" and r["files"]["chunk_000.wav"] == {"unchanged": True} and "raw_chunk_000.wav" not in r["files"]

    # --- cancel (guarded)
    for nm, ca, tk, trs in (("cancel_guard_before_first_sentence", ("clock", 1), [], []),
                            ("cancel_guard_before_sentence", ("transcribe", 1), [s1], [sw1]),
                            ("cancel_guard_before_retry", ("transcribe", 2), [s1, s2], [sw1, sw2[:-1]]),
                            ("cancel_guard_after_synthesis", ("synthesize", 1), [s1], [])):
        r = h.run(nm, text=C, takes=[Rok(x) for x in tk], transcripts=[T(w) for w in trs], embeddings=[], voice=V1, s=G,
                  cancel_after=ca, fired=f"guarded cancel {ca}")
        assert r["error"]["type"] == "Cancelled"
    r = h.run("cancel_before_fallback", text=A, takes=[Rok(g_ok)], transcripts=[unsafe_tr], embeddings=[], voice=V1,
              s={**G, "max_attempts": 1}, cancel_after=("clock", 2), fired="cancel before the fallback take")
    assert r["error"]["type"] == "Cancelled" and "rejected_context_000.wav" in r["files"]
    r = h.run("cancel_after_fallback_synthesis", text=A, takes=[Rok(g_ok), Rok(a_ok)], transcripts=[unsafe_tr], embeddings=[],
              voice=V1, s={**G, "max_attempts": 1}, cancel_after=("synthesize", 2), fired="cancel after the fallback synthesis")
    assert r["error"]["type"] == "Cancelled"


def widen_cases(st, pipeline):
    """backend/tests/test_narration_quality.py SentencePauseTests, at 24 kHz, as vectors."""
    R = 24000

    def voiced(seconds):
        a = (0.4 * np.sin(2 * np.pi * 200 * np.arange(int(seconds * R)) / R)).astype(np.float32)
        a[-4800:] *= np.geomspace(1.0, 1e-5, 4800)
        return a

    def parts(gap_s):
        rng = np.random.default_rng(0)
        quiet = (0.0005 * rng.standard_normal(int(gap_s * R))).astype(np.float32)
        word = lambda sec: np.concatenate([voiced(sec)[:-4800], voiced(sec)[-4800:-4320] * np.geomspace(1, 1e-4, 480)])
        return word(1.0), quiet, word(1.2)

    def words(first_end="half."):
        return [{"word": " Pull", "start": 0.0, "end": 0.5}, {"word": " " + first_end, "start": 0.5, "end": 0.97},
                {"word": " One", "start": 1.08, "end": 1.5}, {"word": " nudge.", "start": 1.5, "end": 2.2}]

    out = []

    def run(name, pieces, text, ws, target, changed):
        a = np.concatenate(pieces)
        got, wout = pipeline.widen_sentence_pauses(a, R, text, copy.deepcopy(ws), target)
        assert (not np.array_equal(got, a)) == changed, name
        out.append({"name": name, "pieces": [st.f32(x) for x in pieces], "sr": R, "text": text, "words": ws, "target_s": target,
                    "out": sha(got), "words_out": wout, "changed": changed})

    s1, q, s2 = parts(0.08)
    run("short_full_stop", [s1, q, s2], "Pull half. One nudge.", words(), 0.30, True)
    run("misheard_comma", [s1, q, s2], "Pull half. One nudge.", words("half,"), 0.30, True)
    run("half_second_target", [s1, q, s2], "Pull half. One nudge.", words(), 0.50, True)
    run("comma_in_script", [s1, q, s2], "Pull half, one nudge.", words("half,"), 0.30, False)
    s1, q, s2 = parts(0.06)
    dip = len(s1) - int(0.12 * R)
    s1 = s1.copy()
    s1[dip:dip + int(0.02 * R)] *= 1e-4
    ws = words()
    ws[1] = {**ws[1], "end": len(s1) / R - 0.09}
    ws[2] = {**ws[2], "start": len(s1) / R + 0.08}
    run("dip_inside_word", [s1, q, s2], "Pull half. One nudge.", ws, 0.30, True)
    s1, q, s2 = parts(0.40)
    run("long_enough", [s1, q, s2], "Pull half. One nudge.", words(), 0.30, False)
    return out


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
        import soundfile as sf
        from transformers import WhisperTokenizer

        from app import audio, db, delivery, ending_guard, ml, pipeline

        def no_model(*a, **k):
            raise RuntimeError("record_render.py must never load a model")

        def no_ffmpeg(*a, **k):
            raise RuntimeError("record_render.py must never run ffmpeg")

        ml.stt_model = no_model
        ml._voice_encoder = no_model
        audio.subprocess = type("NoSubprocess", (), {"run": staticmethod(no_ffmpeg)})
        tok = WhisperTokenizer.from_pretrained("openai/whisper-large-v3-turbo", local_files_only=True)
        ml._normalizer = tok.normalize
        pipeline.round = delivery.round = ending_guard.round = guarded_round
        REAL["_ref_pitch"] = pipeline._ref_pitch
        if ARR.exists():
            shutil.rmtree(ARR)
        ARR.mkdir()
        st = Store()
        # a float WAV as libsndfile writes it (fact and PEAK chunks), so Rust's reader is checked on one
        probe = tmp / "probe.wav"
        sf.write(str(probe), np.array([0.25, -0.5, 1.5, 1e-7], np.float32), SR, subtype="FLOAT")
        pb = bytearray(probe.read_bytes())
        k = pb.find(b"PEAK")
        assert k > 0
        pb[k + 12:k + 16] = bytes(4)  # the PEAK chunk's write time: zeroed so a second run is byte-identical
        data = {"sndfile_float_wav": {**st.put(bytes(pb), "wav", 4), "samples": [0.25, -0.5, 1.5, 1e-7], "sr": SR}}
        h = Harness(st, pipeline, audio, ml, db, config, sf, ending_guard)
        cases(h)
        data["render"] = h.cases
        data["widen"] = widen_cases(st, pipeline)
        assert "mlx" not in sys.modules and "mlx.core" not in sys.modules, "no model import"
        assert st.bytes <= MAX_BYTES, st.bytes
        text = json.dumps(data, indent=1, ensure_ascii=False, allow_nan=False) + "\n"
        assert str(tmp) not in text and str(Path.home()) not in text  # no temporary or home path in the vectors
        OUT.write_text(text)
        print(f"render.json: {len(h.cases)} render cases, {len(data['widen'])} widen cases; arrays {st.bytes} bytes")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
