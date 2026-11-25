"""Long-form generation with per-chunk quality control.

The fixes for "sudden silence mid-way" and "weird sounds":

* Short, sentence-aligned chunks (default <= 220 chars) instead of 800-char blocks.
* A hard length cap per chunk, so a runaway take is cut off early.
* Every take is checked before it is used:
    - Whisper re-transcribes it and the word error rate must be low (catches skipped
      words, babble, repeated phrases, garbled audio).
    - Internal silences longer than the limit are rejected (catches mid-chunk dropouts).
    - Duration must fit the speaker's own speaking rate (catches loops and truncation).
    - Speaker similarity to the voice fingerprint (catches drift into another voice).
    - Noise-burst and clipping detectors (catches static, buzz, crackle).
  A take that fails is regenerated with a new seed (and slightly lower temperature),
  and the best take is kept. Anything still imperfect is flagged in the UI with a
  one-click regenerate.
* Audio after the last recognised word is trimmed (hallucinated tails).
* Chunks are loudness-matched and joined with natural pauses, no overlapping crossfade.
"""
from __future__ import annotations

import copy
import difflib
import json
import os
import secrets
import shutil
import time
from pathlib import Path

import numpy as np

from . import worker

from . import audio as au
from . import db, delivery, ending_guard, engines, ml, studio
from .config import GENERATIONS_DIR, PROFILES_DIR
from .textproc import ABBREVIATIONS, chunk_text, clean_text, fix_entities, split_sentences, word_count


class Cancelled(Exception):
    pass


def profile_assets(profile: dict, variant: int | None = None) -> tuple[Path, str, np.ndarray, float, float]:
    pdir = PROFILES_DIR / profile["id"]
    v = profile["variants"][profile.get("active_variant", 0) if variant is None else variant]
    ref_wav = pdir / v["file"]
    ref_text = (profile.get("ref_text_override") or {}).get(str(v["index"])) or v["text"]
    fp = np.load(pdir / "fingerprint.npy")
    return ref_wav, ref_text, fp, float(profile.get("self_similarity", 0.8)), float(profile.get("speech_rate_wps", 2.6))


def noise_limit(profile: dict) -> float:
    """Noise-burst limit calibrated to the speaker's own recording.

    Real voices have breaths and airy fricatives that look noise-like; a take is only
    flagged when it is clearly noisier than the source the voice was cloned from.
    """
    if "noise_baseline" not in profile:
        pdir = PROFILES_DIR / profile["id"]
        vals = []
        for f in [pdir / "clean.wav", *[pdir / v["file"] for v in profile.get("variants", [])]]:
            if f.exists():
                a, sr = au.load(f)
                vals.append(au.noise_bursts(a, sr))
        profile["noise_baseline"] = round(max(vals, default=0.0), 3)
        db.put_profile(profile)
    return max(0.09, profile["noise_baseline"] * 1.3 + 0.05)


def _internal_sentence_cut(raw: np.ndarray, sr: int, words: list[dict]) -> bool:
    """True when a sentence that ends inside a multi-sentence take stops while still loud.

    The model can stop after the first sentence of a chunk and run on; the last sentence is
    the only one the take-level ending check used to see. Judged on the untouched take."""
    db = au.frame_db(raw, sr, 10)
    quiet = db < np.percentile(db, 90) - 40
    for k in range(len(words) - 1):
        w = words[k]
        token = w["word"].strip()
        if not token.endswith((".", "!", "?")) or token.rstrip(".").lower() in ABBREVIATIONS:
            continue  # Whisper writes "Mister" as "Mr.": not a sentence end
        gap = words[k + 1]["start"] - w["end"]
        if gap < 0.15:  # no pause: nothing to judge (the next word would sit inside the window)
            continue
        # End the segment inside the first real silence (80 ms, 40 dB under speech) after the word. Whisper often
        # starts the next word late, so a cut at 80% of the gap could catch that word's onset and read it as a stop.
        stop = int((w["end"] + gap * 0.8) / 0.01)
        i, j = int(w["end"] / 0.01), int(w["end"] / 0.01)
        while i < min(int(words[k + 1]["start"] / 0.01) + 10, len(quiet)):
            j = i
            while j < len(quiet) and quiet[j]:
                j += 1
            if j - i >= 8:
                stop = i + 4
                break
            i = j + 1
        seg = raw[: int(stop * 0.01 * sr)]
        if len(seg) < sr * 0.2:
            continue
        if not au.end_analysis(seg, sr, w["end"])["natural"]:
            return True
    return False


def _word_coverage(text: str, heard: str) -> list[bool]:
    """Align the script and transcript, consuming each heard token only once.

    A similar preceding word ("come") cannot also stand in for a missing "home".
    Name spellings and splits ("Scadutree" / "Skadu tree") remain tolerated.
    This is token coverage, not proof that ASR heard every phoneme correctly.
    """
    exp = ml.normalize_for_wer(text).split()
    got = ml.normalize_for_wer(heard).split()
    n, m = len(exp), len(got)
    costs = [[float('inf')] * (m + 1) for _ in range(n + 1)]
    prev = {}
    costs[0][0] = 0.0

    def offer(i, j, ni, nj, cost, covered=False):
        value = costs[i][j] + cost
        if value < costs[ni][nj]:
            costs[ni][nj] = value
            prev[ni, nj] = (i, j, covered)

    for i in range(n + 1):
        for j in range(m + 1):
            if i < n and j < m:
                for k in range(1, min(3, m - j) + 1):
                    similarity = difflib.SequenceMatcher(None, exp[i], ''.join(got[j:j + k])).ratio()
                    if k == 1 or similarity >= 0.6:
                        offer(i, j, i + 1, j + k, 1 - similarity + 0.02 * (k - 1), similarity >= 0.6)
            if i < n:
                offer(i, j, i + 1, j, 1.0)
            if j < m:
                offer(i, j, i, j + 1, 1.0)
    covered = [False] * n
    i, j = n, m
    while i or j:
        pi, pj, matched = prev[i, j]
        if i > pi:
            covered[pi] = matched
        i, j = pi, pj
    return covered


def _final_word_missing(text: str, heard: str) -> bool:
    covered = _word_coverage(text, heard)
    return bool(covered) and not covered[-1]


def _first_word_missing(text: str, heard: str) -> bool:
    covered = _word_coverage(text, heard)
    return bool(covered) and not covered[0]


@worker.phase("quality_check")
def evaluate(a: np.ndarray, sr: int, text: str, fp: np.ndarray, self_sim: float, wps: float, s: dict, language: str, noise_max: float = 0.09, transcript: dict | None = None) -> dict:
    """Score one take. Returns dict with cleaned audio + metrics + issues."""
    issues: list[str] = []
    if len(a) < sr * 0.3:
        return {"audio": a, "sr": sr, "issues": ["empty"], "score": 99.0, "wer": 1.0, "sim": 0.0, "max_gap": 0.0, "dur": 0.0}

    # --- ASR on the untouched take.
    tr = transcript if transcript is not None else ml.transcribe(a, sr, language=language, words=True)
    words = [w for seg in tr["segments"] for w in seg["words"]]
    raw_audio = a
    raw_dur = len(a) / sr
    last_word_end = words[-1]["end"] if words else raw_dur
    end = au.end_analysis(raw_audio, sr, last_word_end)
    if not end["natural"]:
        issues.append("cut_off")
    if au.lead_burst(raw_audio, sr):
        issues.append("lead_burst")  # the take opens with stray hiss before its first word
    if words:
        # Endings: keep the whole tail. Trim only after the last sound has really ended
        # (short closures inside words are bridged); never fade over speech. The fade at
        # the very end is a 25 ms declick, not a fade-out.
        if end["junk"]:
            issues.append("junk_tail")  # stray extra sound after the last word
        # A quiet first consonant may precede the louder region. Use a low acoustic
        # threshold and the first ASR onset, then put the fade inside added silence.
        start = min(au.speech_start(a, sr), max(0.0, words[0]["start"] - 0.10))
        a = a[int(start * sr) : int(end["cut"] * sr)]
        head_pad = 0.02
        a = np.pad(a, (int(head_pad * sr), int(0.12 * sr)))
        end.update(trim_start=start, head_pad_s=head_pad)
        a = au.fade(a, sr, fade_in_ms=15, fade_out_ms=25)
        # unnatural mid-sentence hesitation, e.g. "to ey... final battle". Whisper stretches
        # word timings across pauses, so find real silences acoustically and check which
        # word each one follows: a long pause after a word with no punctuation is odd.
        for gs, ge in au.silence_report(a, sr).internal_gaps:
            if ge - gs < 0.5:
                continue
            t0 = gs + start - head_pad
            before = [w for w in words if w["start"] <= t0 + 0.05]
            if before and not before[-1]["word"].strip().endswith((",", ".", "?", "!", ";", ":", "—", "-", "…")):
                issues.append("odd_pause")
                break
        if _internal_sentence_cut(raw_audio, sr, words):
            issues.append("cut_off")  # a sentence in the middle of the take stops while still loud
    else:
        a = np.pad(au.trim_edges(a, sr), (0, int(0.12 * sr)))
    w = ml.wer(text, tr["text"])
    coverage = _word_coverage(text, tr["text"])
    if coverage and not coverage[-1]:
        issues.append("end_word_missing")  # WER alone accepts a dropped last word
    if coverage and not coverage[0]:
        issues.append("start_word_missing")

    # --- silences
    rep = au.silence_report(a, sr)
    if rep.max_internal > s["max_internal_silence"]:
        issues.append("long_silence")

    # --- duration vs the speaker's own rate
    dur = len(a) / sr
    expected = max(0.8, word_count(text) / wps)
    ratio = dur / expected
    if ratio > 1.75:
        issues.append("too_long")
    elif ratio < 0.5:
        issues.append("too_short")

    # --- voice match
    sim = ml.cosine(ml.speaker_embedding(a, sr), fp) if dur > 1.0 else self_sim
    rel_sim = sim / max(self_sim, 1e-3)
    if rel_sim < s["min_speaker_similarity"]:
        issues.append("voice_drift")

    # --- artifacts
    nb = au.noise_bursts(a, sr)
    if nb >= noise_max:
        issues.append("noise")
    if au.clipping_ratio(a) > 0.003:
        issues.append("clipping")
    if w > s["max_wer"]:
        issues.append("mispronounced" if w < 0.35 else "wrong_words")

    score = (
        4.0 * w
        + 3.0 * max(0.0, s["min_speaker_similarity"] - rel_sim)
        + (1.5 if "long_silence" in issues else 0)
        + (1.0 if "too_long" in issues or "too_short" in issues else 0)
        + 4.0 * max(0.0, nb - noise_max + 0.09)
        + (0.5 if "clipping" in issues else 0)
        + (2.5 if "cut_off" in issues else 0)
        + (1.5 if "odd_pause" in issues else 0)
        + (2.5 if "lead_burst" in issues else 0)
        + (3.0 if "junk_tail" in issues else 0)
        + (2.5 if "start_word_missing" in issues or "end_word_missing" in issues else 0)
    )
    return {
        "audio": a, "sr": sr, "issues": issues, "score": round(float(score), 4), "wer": round(w, 3),
        "sim": round(float(rel_sim), 3), "max_gap": round(rep.max_internal, 2), "dur": round(dur, 2),
        "gaps": rep.internal_gaps, "heard": tr["text"], "raw_audio": raw_audio, "words": words,
        "ending": {**end, "raw_duration": raw_dur, "last_word_end": last_word_end},
    }


def widen_sentence_pauses(a: np.ndarray, sr: int, text: str, words: list[dict], target_s: float) -> tuple[np.ndarray, list[dict]]:
    """Give every full stop inside a part at least `target_s` of quiet, filled with the take's own room tone.

    The model rushes stops inside a part (0.06-0.13 s) while part joins get the sentence pause setting.
    Sentence ends come from the script, matched to the heard words: Whisper may hear a rushed "half." as "half,".
    Only an existing quiet gap is widened, in its middle, so speech is never moved over or cut.
    Returns the audio and the word times moved by the added quiet."""
    norm = lambda w: "".join(c for c in w.lower() if c.isalnum() or c == "'")
    ends, n = set(), 0
    for sentence in split_sentences(text)[:-1]:  # the part's last stop is set when parts are joined
        n += len(sentence.split())
        ends.add(n - 1)
    sm = difflib.SequenceMatcher(None, [norm(w) for w in text.split()], [norm(w["word"]) for w in words], autojunk=False)
    spans = sorted((words[j + k]["end"], words[j + k + 1]["start"] if j + k + 1 < len(words) else words[j + k]["end"] + 0.4)
                   for i, j, size in sm.get_matching_blocks() for k in range(size) if i + k in ends)
    db = au.frame_db(a, sr)
    # The audible pause: 35 dB under the speech level. The adaptive speech threshold (-33 dB here) counted
    # the word's decay and breath as quiet and read these stops as 0.22 s; mastering makes that tail audible.
    quiet = db <= np.percentile(db, 90) - 35
    hop = sr * au.FRAME_MS // 1000
    runs, start = [], None
    for f, q in enumerate(quiet):
        if q and start is None:
            start = f
        elif not q and start is not None:
            runs.append((start * hop, f * hop))  # runs touching the end are not between words
            start = None
    out, last = [], 0
    for t, nxt in spans:
        # the longest quiet between this word and the next: a closure inside the word ("knocking") is shorter
        near = [r for r in runs if r[0] < (nxt + 0.1) * sr and r[1] > (t - 0.25) * sr and r[0] > 0]
        if not near:
            continue
        g0, g1 = max(near, key=lambda r: r[1] - r[0])
        need = int(target_s * sr) - (g1 - g0)
        mid = (g0 + g1) // 2
        if need <= 0 or mid < last:
            continue
        w = int(0.01 * sr)
        out += [a[last:mid], np.resize(a[max(g0, mid - w):min(g1, mid + w)], need)]
        last = mid
        words = [{**x, "start": x["start"] + need / sr, "end": x["end"] + need / sr} if x["start"] * sr >= mid else x for x in words]
    return (np.concatenate(out + [a[last:]]) if out else a), words


_pitch_cache: dict[str, float] = {}


def _ref_pitch(ref_wav: Path) -> float | None:
    st = ref_wav.stat()  # reference clips are re-cut in place: key on the file revision
    k = f"{ref_wav}:{st.st_mtime_ns}:{st.st_size}"
    if k not in _pitch_cache:
        a, sr = au.load(ref_wav)
        _pitch_cache[k] = delivery.prosody(a, sr)["pitch_st"]
    return _pitch_cache[k] or None


HARD_ISSUES = {"cut_off", "junk_tail", "lead_burst", "ending_unverified", "start_word_missing", "end_word_missing", "flat", "off_pitch", "odd_pause", "empty", "long_silence", "too_long", "too_short", "voice_drift", "noise", "wrong_words", "mispronounced", "clipping"}


def _guarded_take(text, engine, ref_wav, ref_text, language, params, seed, wps, budget, pause_ms, cancel):
    """Protect every sentence, including several inside an existing saved part."""
    sentences = split_sentences(text)
    parts, contexts, words, diagnostics, reports = [], [], [], [], []
    sr_out, offset = None, 0.0
    for si, sentence in enumerate(sentences):
        if cancel():
            raise Cancelled()
        speak, _ = ending_guard.synthesis_text(sentence, "qwen3", language, enabled=True)
        expected = (max(1., word_count(speak) / wps) + .4) * budget
        boundary_attempts = []
        # Within an old multi-sentence part, retain already separated sentences.
        # Retaking the whole part for a later bad pause multiplied failure rates.
        local_limit = 4 if len(sentences) > 1 else 1
        for retry in range(local_limit):
            if cancel():
                raise Cancelled()
            sentence_seed = (seed + si * 2654435761 + retry * 2246822519) % (2**32)
            raw, sr = engines.synthesize(engine, speak, ref_wav, ref_text, language, params, sentence_seed, expected_seconds=expected)
            report = engines.synthesis_report()
            if cancel():
                raise Cancelled()
            tr = ml.transcribe(raw, sr, language=language, words=True)
            try:
                retained, tr, info = ending_guard.remove_context(raw, sr, sentence, tr)
            except ending_guard.UnsafeBoundary as exc:
                boundary_attempts.append({"seed": sentence_seed, "error": str(exc), "synthesis": report})
                if retry == local_limit - 1:
                    exc.audio, exc.sr, exc.transcript, exc.synthesis = raw, sr, tr, report
                    raise
            else:
                boundary_attempts.append({"seed": sentence_seed, "synthesis": report})
                break
        if sr_out is None:
            sr_out = sr
        # No crossfade or fade is allowed over either side of the sentence.
        retained = au.resample(retained, sr, sr_out)
        full = au.resample(raw, sr, sr_out)
        diagnostics.append(full)
        if si:
            prev_tail = au.edge_silence(parts[-1], sr_out)[1]
            head = au.edge_silence(retained, sr_out)[0]
            gap = np.zeros(round(max(0., pause_ms / 1000 - prev_tail - head) * sr_out), np.float32)
            parts.append(gap)
            offset += len(gap) / sr_out
        words.extend({**w, "start": w["start"] + offset, "end": w["end"] + offset}
                     for seg in tr["segments"] for w in seg["words"])
        contexts.append({**info, "sentence": si, "offset": offset, "seed": sentence_seed, "expected_seconds": expected,
                         "boundary_attempts": boundary_attempts})
        reports.append(report)
        parts.append(retained)
        offset += len(retained) / sr_out
    context = {**contexts[-1], "sentences": contexts} if len(contexts) > 1 else contexts[0]
    report = {**reports[-1], "sentences": reports} if len(reports) > 1 else reports[0]
    if any(r.get("stop_reason") == "length_limit_reached" for r in reports):
        report["stop_reason"] = "length_limit_reached"
    return np.concatenate(parts), sr_out, {"text": " ".join(w["word"].strip() for w in words), "segments": [{"words": words}]}, context, np.concatenate(diagnostics), report


def render_chunk(gen: dict, ci: int, profile: dict, s: dict, regen_round: int = 0, cancel=lambda: False) -> dict:
    ch = gen["chunks"][ci]
    vs = {**delivery.DEFAULT_VOICE_SETTINGS, **(gen.get("voice_settings") or {})}
    spec = engines.get_spec(gen["engine"])
    params, qc = delivery.resolve(vs, spec, gen.get("params") or {}, s)
    s = {**s, **qc}
    vidx = delivery.pick_variant(profile, vs["style"])
    ref_wav, ref_text, fp, self_sim, wps = profile_assets(profile, vidx)
    nmax = noise_limit(profile)
    ref_pitch = _ref_pitch(ref_wav)  # always: monotone takes are retaken, lively ones preferred
    # Pitch level must stay near the voice's own median and the neighbouring kept parts.
    pitch_targets = [profile["pitch_hz"]] if profile.get("pitch_hz") else []
    if pitch_targets:
        pitch_targets += [c["pitch_hz"] for c in gen["chunks"] if abs(c["i"] - ch["i"]) == 1 and c.get("file") and c.get("pitch_hz")]
    gdir = GENERATIONS_DIR / gen["id"]
    base_temp = params.get("temperature")
    best = None
    attempts = []
    passed = 0
    # Keep learned tail room on a later Re-speak/Resume instead of repeating a
    # duration cap known to have stopped this part. Still bounded against runaways.
    budget = min(4.0, max(1.0, float(ch.get("duration_budget", 1.0))))
    compare = max(1, int(s.get("compare_takes", 1)))
    max_takes = int(s["max_attempts"]) + compare - 1
    # Generate again, Resume, and Re-speak must not replay the same deterministic
    # seed schedule. Keep the actual seeds in attempts for diagnosis/reproduction.
    nonce = secrets.randbits(32)
    used_seeds = set(ch.get("used_seeds", [])) | {a["seed"] for a in ch.get("attempts", [])}
    speak, guarded = ending_guard.synthesis_text(
        ch["text"], spec.family, gen["language"], enabled=s.get("experimental_qwen_ending_guard") is True)
    for k in range(max_takes):
        if cancel():
            raise Cancelled()
        seed = (int(s["seed"]) + nonce + ci * 1000 + regen_round * 97 + k) % (2**32)
        while seed in used_seeds:
            seed = (seed + 1) % (2**32)
        used_seeds.add(seed)
        p = dict(params)
        if engines.get_spec(gen["engine"]).family == "omnivoice":
            # fixed-length engine: a retry tries a slightly different length (too tight clips
            # the last word, too loose adds pauses), not just a new seed
            p["length_scale"] = (1.0, 1.07, 0.94, 1.14)[k % 4]
        # Ending-only failures (cut-off / stray tail) just need a new seed; anything else
        # steadies the model a little on each retry.
        END_ONLY = ("cut_off", "junk_tail")
        n_decay = sum(1 for a_ in attempts if not all(i in END_ONLY for i in a_["issues"] if i in HARD_ISSUES))
        if base_temp is not None and n_decay:
            p["temperature"] = max(0.3, float(base_temp) * (0.9**n_decay))
        others = sorted((v for v in profile.get("variants") or [] if v["index"] != vidx), key=lambda v: -v.get("score", 0))
        if k == max_takes - 1 and k and others and all(any(i in HARD_ISSUES for i in a_["issues"]) for a_ in attempts):
            # Every take so far failed: a new seed alone is not a real change, so the last take uses the
            # voice's next-best clip (step 6: another clean clip, 0 of 20 flagged vs 3 of 20). Recorded in "ref".
            vidx = others[0]["index"]
            ref_wav, ref_text, fp, self_sim, wps = profile_assets(profile, vidx)
            ref_pitch = _ref_pitch(ref_wav)
        t0 = time.time()
        expected_seconds = (max(1.0, word_count(speak) / wps) + (.4 if guarded else 0.0)) * budget
        transcript, context, full_raw = None, None, None
        if guarded:
            try:
                raw, sr, transcript, context, full_raw, synthesis = _guarded_take(
                    ch["text"], gen["engine"], ref_wav, ref_text, gen["language"], p, seed, wps, budget,
                    s["sentence_pause_ms"], cancel,
                )
                expected_seconds = sum(c["expected_seconds"] for c in context.get("sentences", [context]))
            except ending_guard.UnsafeBoundary as exc:
                synthesis = exc.synthesis
                # Never save/export a spoken continuation or cut at an ASR guess.
                attempts.append({"seed": seed, "temperature": p.get("temperature"), "issues": ["ending_boundary"],
                                 "context_error": str(exc), "synthesis": synthesis, "expected_seconds": expected_seconds,
                                 "secs": round(time.time() - t0, 1)})
                if synthesis.get("stop_reason") == "length_limit_reached":
                    budget = min(4.0, budget * 1.3)
                if k == max_takes - 1:
                    # Diagnostic only: never assigned to ch['file'] or assembled.
                    import soundfile as sf
                    sf.write(str(gdir / f"rejected_context_{ci:03d}.wav"), exc.audio, exc.sr, subtype="FLOAT")
                    save_json(gdir / f"rejected_context_{ci:03d}.json", {"attempts": attempts, "transcript": exc.transcript})
                continue
        else:
            raw, sr = engines.synthesize(gen["engine"], speak, ref_wav, ref_text, gen["language"], p, seed, expected_seconds=expected_seconds)
            synthesis = engines.synthesis_report()
        if cancel():
            raise Cancelled()
        ev = evaluate(raw, sr, ch["text"], fp, self_sim, wps, s, gen["language"], noise_max=nmax, transcript=transcript)
        ev.update(context=context, full_raw=full_raw if guarded else None, seed=seed)
        if cancel():  # a stop during a long take must not wait for the whole part
            raise Cancelled()
        if synthesis.get("stop_reason") == "length_limit_reached":
            budget = min(4.0, budget * 1.3)
            if "cut_off" not in ev["issues"]:
                ev["issues"].append("cut_off")
                ev["score"] += 2.5
        pro = delivery.prosody(ev["audio"], sr) if (ref_pitch or pitch_targets) and ev["dur"] > 1.5 else None
        if pro and ref_pitch:
            # keep the reference's pitch movement: flat, "just reading" takes are retaken,
            # and among good takes the livelier one wins (more so with Style turned up)
            ev["liveliness"] = round(pro["pitch_st"] / ref_pitch, 2)
            want = 0.85 + 0.15 * vs["style"]
            ev["score"] = round(ev["score"] + 1.5 * max(0.0, want - ev["liveliness"]), 4)
            if ev["liveliness"] < 0.65:
                ev["issues"].append("flat")
        if pro and pitch_targets and pro.get("pitch_hz"):
            # pitch level: takes of one voice drifted by up to 5.5 semitones and all passed
            ev["pitch_hz"] = round(pro["pitch_hz"], 1)
            off = max(abs(12 * np.log2(ev["pitch_hz"] / t)) for t in pitch_targets)
            ev["score"] = round(ev["score"] + 0.5 * off, 4)  # among failing takes the closest pitch wins
            if off > float(s.get("max_pitch_offset_st", 2.0)):
                ev["issues"].append("off_pitch")
        hard = [i for i in ev["issues"] if i in HARD_ISSUES]
        attempts.append(
            {"seed": seed, "temperature": p.get("temperature"), "wer": ev["wer"], "sim": ev["sim"], "max_gap": ev["max_gap"],
             "dur": ev["dur"], "issues": ev["issues"], "score": ev["score"], "secs": round(time.time() - t0, 1),
             "liveliness": ev.get("liveliness"), "pitch_hz": ev.get("pitch_hz"), "ref": vidx,
             "heard": ev.get("heard", ""), "ending": ev.get("ending"), "synthesis": synthesis,
             "expected_seconds": expected_seconds, "ending_guard": context}
        )
        # among clean takes, prefer the one closest to the real voice
        ev["pick"] = ev["score"] - (0.6 * ev["sim"] if not hard else 0.0)
        ev["has_hard"] = bool(hard)
        if best is None or (ev["has_hard"], ev["pick"]) < (best["has_hard"], best["pick"]):
            best = ev
        if not hard:
            passed += 1
            if passed >= compare:
                break

    if best is None:
        if (gdir / f"chunk_{ci:03d}.wav").exists():
            raise ValueError("Could not safely separate the sentence ending from its continuation. Try Re-speak again; the previous take was kept.")
        # A rare inseparable pause must not stop a whole long narration. Use a
        # new plain take with an explicit review flag, never the uncut context.
        if cancel():
            raise Cancelled()
        seed = (seed + 1) % (2**32)
        while seed in used_seeds:
            seed = (seed + 1) % (2**32)
        used_seeds.add(seed)
        expected_seconds = max(1., word_count(ch["text"]) / wps) * budget
        t0 = time.time()
        raw, sr = engines.synthesize(gen["engine"], ch["text"], ref_wav, ref_text, gen["language"], p, seed, expected_seconds=expected_seconds)
        if cancel():
            raise Cancelled()
        best = evaluate(raw, sr, ch["text"], fp, self_sim, wps, s, gen["language"], noise_max=nmax)
        best["issues"].append("ending_unverified")
        best["score"] += 2.5
        best.update(context=None, full_raw=None, seed=seed)
        synthesis = engines.synthesis_report()
        if synthesis.get("stop_reason") == "length_limit_reached":
            if "cut_off" not in best["issues"]:
                best["issues"].append("cut_off")
            budget = min(4., budget * 1.3)
        attempts.append({"seed": seed, "issues": best["issues"], "score": best["score"], "wer": best["wer"],
                         "sim": best["sim"], "dur": best["dur"], "max_gap": best["max_gap"], "heard": best.get("heard", ""),
                         "synthesis": synthesis, "expected_seconds": expected_seconds, "ending_guard_fallback": True,
                         "temperature": p.get("temperature"), "secs": round(time.time() - t0, 1)})
    a, sr = best["audio"], best.get("sr", 24000)
    hard = [i for i in best["issues"] if i in HARD_ISSUES]
    ending = best.get("ending") or {}
    words = None  # the kept take's word times inside the saved part, for sentence/word timings
    if best.get("words") and "trim_start" in ending:
        shift = ending["head_pad_s"] - ending["trim_start"]  # Whisper ran on the untrimmed take
        words = [{"word": w["word"], "start": w["start"] + shift, "end": w["end"] + shift} for w in best["words"]]
    # Repair: collapse any over-long internal pause in the chosen take.
    if "long_silence" in hard:
        a = au.shorten_gaps(a, sr, best.get("gaps", []), max_gap=0.45)
        hard = [h for h in hard if h != "long_silence"]
        words = None  # shortcut: the cut moves later words, so this rare part gets estimated timings; shift them if it matters
    elif int(s.get("inner_sentence_pause_ms", 0)) and words:
        a, words = widen_sentence_pauses(a, sr, ch["text"], words, int(s["inner_sentence_pause_ms"]) / 1000)
    a = au.normalize_lufs(a, sr, s["target_lufs"], peak_db=-1.0)
    fname = f"chunk_{ci:03d}.wav"
    au.save(gdir / fname, a, sr)
    raw_fname = f"raw_chunk_{ci:03d}.wav"
    import soundfile as sf

    sf.write(str(gdir / raw_fname), best.get("raw_audio", best["audio"]), sr, subtype="FLOAT")
    context_fname = f"context_chunk_{ci:03d}.wav"
    if best.get("context"):
        sf.write(str(gdir / context_fname), best["full_raw"], sr, subtype="FLOAT")
    else:
        (gdir / context_fname).unlink(missing_ok=True)
    ch.update(
        status="flagged" if hard else "ok", file=fname, sr=sr, attempts=attempts, wer=best["wer"], sim=best["sim"],
        dur=round(len(a) / sr, 2), issues=hard, heard=best.get("heard", ""),
        raw_file=raw_fname, ending=best.get("ending"),
        audio_revision=int(ch.get("audio_revision", 0)) + 1, used_seeds=sorted(used_seeds), duration_budget=budget,
        pitch_hz=best.get("pitch_hz"), words=[{**w, "start": round(w["start"], 3), "end": round(w["end"], 3)} for w in words] if words else None,
        ending_guard=best.get("context"), context_file=context_fname if best.get("context") else None, chosen_seed=best["seed"],
    )
    return ch


@worker.phase("assembly")
def assemble(gen: dict, s: dict) -> None:
    gdir = GENERATIONS_DIR / gen["id"]
    parts, pauses, idx, sr_out = [], [], [], None
    for ch in gen["chunks"]:
        if not ch.get("file"):
            continue
        a, sr = au.load(gdir / ch["file"])
        if sr_out is None:
            sr_out = sr
        parts.append(au.resample(a, sr, sr_out))
        idx.append(ch["i"])
        pauses.append(
            {"sentence": s["sentence_pause_ms"], "paragraph": s["paragraph_pause_ms"], "end": 0}[ch["pause_after"]]
        )
    if not parts:
        return
    lead = 0.15
    pauses = pauses[:-1] + [0]
    # Each part already has a little natural room at both edges; add only what's missing so
    # the audible gap between sentences equals the setting (300 ms sentence / 700 ms paragraph).
    edges = [au.edge_silence(p, sr_out) for p in parts]
    pauses = [
        0 if i == len(parts) - 1 else max(0, int(pauses[i] - 1000 * (edges[i][1] + edges[i + 1][0])))
        for i in range(len(parts))
    ]
    # timeline of each chunk inside the final file (for subtitles)
    timeline, t = [], lead
    for i, (p, pause) in enumerate(zip(parts, pauses)):
        d = len(p) / sr_out
        timeline.append({"i": idx[i], "start": round(t, 3), "end": round(t + d, 3)})
        t += d + pause / 1000
    full = au.concat_with_pauses(parts, pauses, sr_out)
    full = np.concatenate([np.zeros(int(lead * sr_out), np.float32), full, np.zeros(int(0.4 * sr_out), np.float32)])
    vs = {**delivery.DEFAULT_VOICE_SETTINGS, **(gen.get("voice_settings") or {})}
    mic = PROFILES_DIR / gen["profile_id"] / "mic.json"
    gen["mic_eq"] = None
    if vs.get("mic_match") and mic.exists():
        import json as _json

        full, curve = delivery.mic_match(full, sr_out, _json.loads(mic.read_text())["bands"])
        gen["mic_eq"] = curve or None
    if vs.get("studio_finish"):
        full = studio.master_chain(full, sr_out)
    full = au.normalize_lufs(full, sr_out, s["target_lufs"], peak_db=-1.0)
    speed = float(gen.get("speed") or 1.0)
    tmp_master = gdir / "master.tmp.wav"
    au.save(tmp_master, full, sr_out)
    try:
        apply_speed(gdir, speed, master=tmp_master)
    except BaseException:
        tmp_master.unlink(missing_ok=True)
        raise
    os.replace(tmp_master, gdir / "master.wav")  # 1.0x master, kept for re-timing
    gen["audio_rev"] = time.time()
    gen["timeline"] = [{**x, "start": round(x["start"] / speed, 3), "end": round(x["end"] / speed, 3)} for x in timeline]
    gen["file"] = "final.wav"
    gen["duration"] = round(len(full) / sr_out / speed, 2)
    gen["sample_rate"] = sr_out


@worker.phase("speed_export")
def apply_speed(gdir: Path, speed: float, master: Path | None = None) -> None:
    """Pitch-preserving tempo change (ffmpeg atempo) from the 1.0x master.

    WAV and MP3 are both encoded first and only then published, so a failed encode leaves
    the previous pair intact instead of a new WAV beside an old MP3."""
    import subprocess

    master = master or gdir / "master.wav"
    tmp_wav, tmp_mp3 = gdir / "final.tmp.wav", gdir / "final.tmp.mp3"
    try:
        if abs(speed - 1.0) < 1e-3:
            shutil.copyfile(master, tmp_wav)
        else:
            subprocess.run(
                ["ffmpeg", "-y", "-hide_banner", "-loglevel", "error", "-i", str(master),
                 "-af", f"atempo={speed:.3f}", "-c:a", "pcm_s16le", str(tmp_wav)],
                check=True,
            )
        au.export_mp3(tmp_wav, tmp_mp3)
        os.replace(tmp_wav, gdir / "final.wav")
        os.replace(tmp_mp3, gdir / "final.mp3")
    finally:
        tmp_wav.unlink(missing_ok=True)
        tmp_mp3.unlink(missing_ok=True)


def retime(gen: dict, speed: float) -> None:
    """Change playback speed of a finished generation without re-rendering."""
    gdir = GENERATIONS_DIR / gen["id"]
    old = float(gen.get("speed") or 1.0)
    if not (gdir / "master.wav").exists():
        raise ValueError("Re-render this generation to change its speed.")
    apply_speed(gdir, speed)
    k = old / speed
    gen["timeline"] = [{**x, "start": round(x["start"] * k, 3), "end": round(x["end"] * k, 3)} for x in gen.get("timeline", [])]
    gen["duration"] = round((gen.get("duration") or 0) * k, 2)
    gen["speed"] = speed
    gen["audio_rev"] = time.time()


def create_generation(profile: dict, text: str, engine: str, language: str, params: dict, s: dict, speed: float = 1.0, voice_settings: dict | None = None) -> dict:
    import uuid

    cleaned = clean_text(text, s.get("pronunciations"), s.get("strip_citations", True))
    chunks = chunk_text(cleaned, int(s["max_chunk_chars"]))
    if not chunks:
        raise ValueError("Nothing to say: the script is empty after cleanup.")
    gid = uuid.uuid4().hex[:12]
    (GENERATIONS_DIR / gid).mkdir(parents=True, exist_ok=True)
    gen = {
        "id": gid, "profile_id": profile["id"], "profile_name": profile["name"], "engine": engine, "language": language,
        "params": params, "speed": max(0.7, min(1.3, float(speed or 1.0))), "favorite": False,
        "voice_settings": {**delivery.DEFAULT_VOICE_SETTINGS, **{k: v for k, v in (voice_settings or {}).items() if k in delivery.DEFAULT_VOICE_SETTINGS}},
        "title": " ".join(cleaned.split()[:8]) + ("…" if len(cleaned.split()) > 8 else ""),
        "text": text, "cleaned_text": cleaned, "status": "queued", "progress": 0.0, "message": "Queued",
        "chunks": [{"i": i, "text": c.text, "pause_after": c.pause_after, "status": "pending"} for i, c in enumerate(chunks)],
        "settings": {k: s[k] for k in ("max_chunk_chars", "max_attempts", "compare_takes", "max_wer", "min_speaker_similarity", "max_internal_silence", "sentence_pause_ms", "paragraph_pause_ms", "target_lufs", "seed")},
    }
    return db.put_generation(gen)


def _save(gen: dict) -> None:
    """Persist worker progress. Stops the run when the narration was deleted meanwhile."""
    if not db.put_generation_live(gen):
        raise Cancelled()


def run_generation(gen_id: str, cancel, only_chunks: list[int] | None = None) -> None:
    gen = db.get_generation(gen_id)
    profile = db.get_profile(gen["profile_id"])
    if profile is None:
        raise ValueError("Voice profile was deleted.")
    s = {**db.get_settings(), **gen["settings"]}
    if delivery.backfill(profile, PROFILES_DIR / profile["id"]):
        db.put_profile(profile)
    todo = only_chunks if only_chunks is not None else [c["i"] for c in gen["chunks"] if c["status"] != "ok"]
    gen["status"] = "running"
    spec = engines.get_spec(gen["engine"])
    try:
        if not engines.is_downloaded(spec.repo):
            gen["message"] = f"Downloading {spec.name} ({spec.size_gb} GB, first time only)"
            _save(gen)
            engines.download(spec.repo)
        gen["message"] = f"Loading {spec.name}"
        _save(gen)
        engines.load(gen["engine"])
        t0 = time.time()
        for n, ci in enumerate(todo):
            gen["message"] = f"Chunk {ci + 1} of {len(gen['chunks'])}"
            gen["chunks"][ci]["status"] = "running"
            _save(gen)
            round_ = gen["chunks"][ci].get("regen_count", 0)
            render_chunk(gen, ci, profile, s, regen_round=round_, cancel=cancel)
            gen["progress"] = round((n + 1) / len(todo), 4)
            elapsed = time.time() - t0
            gen["eta"] = round(elapsed / (n + 1) * (len(todo) - n - 1))
            _save(gen)
        gen["message"] = "Assembling"
        _save(gen)
        assemble(gen, s)
        # Every render, including one-part replacements, receives a final check.
        gen["message"] = "Final check: checking the finished narration for audio defects"
        _save(gen)
        # A re-speak or fix records only its own parts; anything else the check finds gets flagged below.
        bad = [ci for ci in final_scan(gen) if only_chunks is None or ci in only_chunks]
        if bad:
            for n, ci in enumerate(bad):
                if cancel():
                    raise Cancelled()
                gen["message"] = f"Final check: re-recording part {ci + 1} ({n + 1} of {len(bad)})"
                gen["chunks"][ci]["regen_count"] = gen["chunks"][ci].get("regen_count", 0) + 1
                gen["chunks"][ci]["status"] = "running"
                _save(gen)
                render_chunk(gen, ci, profile, s, regen_round=gen["chunks"][ci]["regen_count"], cancel=cancel)
            if cancel():
                raise Cancelled()
            assemble(gen, s)
        remaining = final_scan(gen)
        if cancel():  # a stop during the final take is honoured, never marked done
            raise Cancelled()
        # A part needs a listen when its take or its final check says so; recomputed every run,
        # so a part that now passes is no longer flagged.
        for ch in gen["chunks"]:
            if ch.get("status") in ("ok", "flagged"):
                ch["status"] = "flagged" if (ch.get("issues") or ch.get("final_issues")) else "ok"
        fixed = [ci for ci in bad if ci not in remaining and gen["chunks"][ci]["status"] == "ok"]
        for ci in bad:
            gen["chunks"][ci]["final_fix"] = ci in fixed
        gen["final_scan"] = {"checked": len(gen["timeline"]), "attempted": bad, "fixed": fixed, "remaining": remaining}
        flagged = [c["i"] for c in gen["chunks"] if c["status"] == "flagged"]
        gen["status"] = "done"
        gen["flagged"] = flagged
        gen["retries"] = sum(max(0, len(c.get("attempts", [])) - 1) for c in gen["chunks"])
        gen["message"] = (
            f"Done, needs a listen: part {', '.join(str(r['part']) for r in review(gen))}." if fix_targets(gen) else "Done. All parts passed the automated checks."
        )
        gen["render_seconds"] = round(time.time() - t0, 1)
    except Cancelled:
        gen["status"] = "cancelled"
        gen["message"] = "Cancelled"
        for c in gen["chunks"]:
            if c["status"] == "running":
                c["status"] = "pending"
    db.put_generation_live(gen)  # False when the narration was deleted: nothing is recreated


def _processed_edge_issues(reference: np.ndarray, processed: np.ndarray, sr: int) -> set[str]:
    """Detect missing speech at the edges after assembly/mastering/speed changes.

    Compare normalized energy envelopes with 50 ms timing tolerance, not raw
    ending levels (compression changes those). This does not judge pronunciation.
    Both arrays represent the same timeline duration at the same sample rate.
    """
    if not len(reference) or np.max(np.abs(reference)) < 1e-5:
        return set()
    if not len(processed) or np.max(np.abs(processed)) < 1e-5:
        return {"processed_audio_missing"}
    ref = au.frame_db(reference, sr, 10)
    got = au.frame_db(np.pad(processed, (0, max(0, len(reference) - len(processed))))[:len(reference)], sr, 10)
    # Nearby output counts as present, accommodating atempo and filter latency.
    nearby = np.max(np.lib.stride_tricks.sliding_window_view(np.pad(got, (5, 5), constant_values=-120), 11), axis=1)
    expected = ref > np.max(ref) - 35
    missing = expected & (nearby < np.max(got) - 60)
    # Only boundary losses, sustained for at least 40 ms, are classified here.
    live = np.flatnonzero(expected)
    run = 0
    for i in range(len(missing)):
        at_edge = len(live) and (i <= live[0] + 40 or i >= live[-1] - 40)
        run = run + 1 if at_edge and missing[i] else 0
        if run >= 4:
            return {"processed_audio_cut_off"}
    return set()


@worker.phase("final_check")
def final_scan(gen: dict) -> list[int]:
    """Check raw endings and retained speech at each processed part's edges.

    Abrupt raw stops, stray tails, missing exports and sustained boundary losses
    fail the check. Energy comparisons tolerate timing and compression changes;
    they do not replace ASR or a person's pronunciation/listening review.
    """
    gdir = GENERATIONS_DIR / gen["id"]
    bad = []
    tl = gen.get("timeline", [])
    chunks = {ch["i"]: ch for ch in gen["chunks"]}
    final_path = gdir / (gen.get("file") or "final.wav")
    final, final_sr = au.load(final_path) if final_path.exists() else (None, None)
    for t in tl:
        ch = chunks[t["i"]]
        issues = set()
        raw_path = gdir / ch["raw_file"] if ch.get("raw_file") else None
        if raw_path and raw_path.exists():
            raw, raw_sr = au.load(raw_path)
            last_word_end = (ch.get("ending") or {}).get("last_word_end", len(raw) / raw_sr)
            ending = au.end_analysis(raw, raw_sr, last_word_end)
            if not ending["natural"]:
                issues.add("cut_off")
            if ending.get("junk"):
                issues.add("junk_tail")
            if au.lead_burst(raw, raw_sr):
                issues.add("lead_burst")
            # A damaged chunk can match its export perfectly. Also inspect
            # sound discarded by its original trim, using the untouched take.
            saved_cut = (ch.get("ending") or {}).get("cut", len(raw) / raw_sr)
            discarded = raw[max(0, round(saved_cut * raw_sr)):round(ending["cut"] * raw_sr)]
            if len(discarded) >= raw_sr * 0.02:
                speech_level = float(np.percentile(au.frame_db(raw, raw_sr, 10), 90))
                if np.count_nonzero(au.frame_db(discarded, raw_sr, 10) > speech_level - 48) >= 2:
                    issues.add("processed_audio_cut_off")
        # Existing known cutoff takes must never become clean just because a
        # fade in the stored WAV hides the original hard stop.
        issues.update(i for i in ch.get("issues", []) if i in ("cut_off", "junk_tail"))
        # Check the actual processed file against each saved chunk, including the
        # silence padded by assembly. Older rows without a chunk file retain raw checks.
        chunk_path = gdir / ch["file"] if ch.get("file") else None
        if chunk_path and chunk_path.exists():
            if final is None:
                issues.add("processed_audio_missing")
            else:
                chunk, chunk_sr = au.load(chunk_path, final_sr)
                speed = float(gen.get("speed") or 1.0)
                expected_samples = max(1, round(len(chunk) / speed))
                # Approximate atempo's energy alignment without another ffmpeg call.
                expected = np.interp(np.arange(expected_samples) * speed, np.arange(len(chunk)), chunk).astype(np.float32)
                start = max(0, round(t["start"] * final_sr))
                segment = final[start:start + expected_samples]
                issues.update(_processed_edge_issues(expected, segment, final_sr))
        ch["final_issues"] = sorted(issues)
        if issues:
            bad.append(t["i"])
    return bad


def clean_replacement(text: str, gen: dict, s: dict) -> str:
    """Cleaned text for a re-spoken part. Raises ValueError when it cannot be spoken as one part."""
    cleaned = clean_text(text, s.get("pronunciations"), s.get("strip_citations", True))
    if not cleaned.strip():
        raise ValueError("The new text is empty after cleanup.")
    limit = int((gen.get("settings") or {}).get("max_chunk_chars") or s["max_chunk_chars"])
    if len(cleaned) > limit:
        raise ValueError(
            f"This part would be {len(cleaned)} characters; a part can be at most {limit}. "
            "Split the text into two parts."
        )
    return cleaned


def _snapshot(gdir: Path, names: list[str]) -> dict[str, Path | None]:
    """Copy the files a re-speak may replace, so a failed run can put them back."""
    snap: dict[str, Path | None] = {}
    for n in names:
        src = gdir / n
        if src.exists():
            bak = gdir / (n + ".bak")
            shutil.copy2(src, bak)
            snap[n] = bak
        else:
            snap[n] = None
    return snap


def _restore(gdir: Path, snap: dict[str, Path | None]) -> None:
    for n, bak in snap.items():
        if bak is not None:
            os.replace(bak, gdir / n)
        else:
            (gdir / n).unlink(missing_ok=True)


def _discard(snap: dict[str, Path | None]) -> None:
    for bak in snap.values():
        if bak is not None:
            bak.unlink(missing_ok=True)


def regenerate_chunk(gen_id: str, ci: int, new_text: str | None, cancel) -> None:
    """Re-speak one part. If the run fails, the previous text, audio, subtitles and export
    are restored together, so a failed edit never pairs new subtitles with old audio."""
    gen = db.get_generation(gen_id)
    ch = gen["chunks"][ci]
    gdir = GENERATIONS_DIR / gen_id
    before = copy.deepcopy(gen)
    if new_text is not None and new_text.strip():
        ch["text"] = clean_replacement(new_text, gen, db.get_settings())
    ch["regen_count"] = ch.get("regen_count", 0) + 1
    ch["status"] = "pending"
    names = [f"chunk_{ci:03d}.wav", f"raw_chunk_{ci:03d}.wav", f"context_chunk_{ci:03d}.wav", "final.wav", "final.mp3", "master.wav"]
    snap = _snapshot(gdir, names)  # before anything is marked running, so a failure here leaves no stuck state
    gen["status"] = "running"
    db.put_generation_live(gen)
    try:
        run_generation(gen_id, cancel, only_chunks=[ci])  # a stop is recorded as a status, not raised
        stopped = (db.get_generation(gen_id) or {}).get("status") == "cancelled"
    except BaseException:
        stopped = True
        _restore(gdir, snap)
        before["message"] = "The re-speak did not finish. The previous text and audio were kept."
        db.put_generation_live(before)
        raise
    else:
        if stopped:
            _restore(gdir, snap)
            before["message"] = "The re-speak was stopped. The previous text and audio were kept."
            db.put_generation_live(before)
    finally:
        _discard(snap)


def fix_targets(gen: dict) -> list[int]:
    """Parts that need a re-take: flagged, failing the final check, or whose script text
    carries a web artifact (such as &39;) that the voice would read aloud."""
    out = set((gen.get("final_scan") or {}).get("remaining") or [])
    for c in gen.get("chunks", []):
        if c.get("status") == "flagged":
            out.add(c["i"])
        if fix_entities(c.get("text") or "") != (c.get("text") or ""):
            out.add(c["i"])
    return sorted(out)


# Plain-English reasons for the API and MCP. Must match ISSUE_LABEL in frontend/src/api.ts (a test checks).
ISSUE_LABEL = {
    "cut_off": "Cut-off ending", "lead_burst": "Stray sound at the start", "end_word_missing": "Last word missing",
    "start_word_missing": "First word missing", "processed_audio_missing": "Audio missing from final mix",
    "processed_audio_cut_off": "Speech lost in final mix", "ending_unverified": "Listen to sentence ending",
    "odd_pause": "Odd pause", "leak": "Stray next word", "junk_tail": "Stray sound at the end", "flat": "Too monotone", "off_pitch": "Pitch off from the voice",
    "long_silence": "Silence gap", "too_long": "Too long / looping", "too_short": "Cut short", "voice_drift": "Voice drift",
    "noise": "Noise burst", "wrong_words": "Wrong words", "mispronounced": "Mispronounced", "clipping": "Clipping",
    "empty": "No audio", "trimmed_tail": "Tail trimmed", "trimmed_head": "Head trimmed",
    "final_check": "Failed the final check", "web_artifact": "Web code in the script (such as &#39;)",
}


def review(gen: dict) -> list[dict]:
    """Parts a person should listen to: the same parts the Fix button re-records, with their text and reasons."""
    out = []
    for i in fix_targets(gen):
        ch = gen["chunks"][i]
        codes = list(dict.fromkeys([*(ch.get("issues") or []), *(ch.get("final_issues") or [])]))
        if fix_entities(ch.get("text") or "") != (ch.get("text") or ""):
            codes.append("web_artifact")
        codes = codes or ["final_check"]
        out.append({"part": i + 1, "text": ch.get("text") or "", "issues": codes, "reasons": [ISSUE_LABEL.get(c, c.replace("_", " ")) for c in codes]})
    return out


def outcome(gen: dict) -> str | None:
    """"clean" or "needs_listen" once a narration is done; None before. "done" alone never means every part passed."""
    if gen.get("status") != "done":
        return None
    return "needs_listen" if fix_targets(gen) else "clean"


def fix_remaining(gen_id: str, cancel) -> None:
    """Re-record every part that still needs attention (see fix_targets). Each gets a fresh set of
    takes; the best take is kept. Web artifacts are corrected in the stored script first."""
    gen = db.get_generation(gen_id)
    targets = fix_targets(gen)
    if not targets:
        raise ValueError("Nothing left to fix.")
    gen["text"] = fix_entities(gen.get("text") or "")
    gen["cleaned_text"] = fix_entities(gen.get("cleaned_text") or "")
    for ci in targets:
        ch = gen["chunks"][ci]
        ch["text"] = fix_entities(ch["text"])
        ch["regen_count"] = ch.get("regen_count", 0) + 1
        ch["status"] = "pending"
    gen["status"] = "running"
    gen["message"] = f"Fixing {len(targets)} remaining part(s)"
    db.put_generation_live(gen)
    run_generation(gen_id, cancel, only_chunks=targets)


def save_json(path: Path, obj) -> None:
    path.write_text(json.dumps(obj, indent=1))


# ------------------------------------------------------------------ voice previews

PREVIEW_TEXT = {
    "en": "Hi, this is my voice. Give me any script, and I'll read it clearly and naturally, just like this.",
}


def _preview_key(profile: dict) -> tuple:
    """What a preview was made from. A change to any of it makes the preview stale."""
    vidx = profile.get("active_variant", 0)
    return (vidx, profile_assets(profile, vidx)[1], profile.get("default_engine"), profile.get("language"))


def make_preview(pid: str, cancel=lambda: False) -> bool | str:
    """A short sample spoken by the clone itself, so choosing a voice means hearing
    what narrations will actually sound like. Same checks as a real narration.

    Returns True when saved, False when there is nothing to do, and "stale" when the voice
    changed while the preview was being made (the caller queues a fresh one)."""
    import json as _json

    profile = db.get_profile(pid)
    if not profile or profile.get("status") != "ready" or not profile.get("variants"):
        return False
    text = PREVIEW_TEXT.get(profile.get("language") or "en")
    s = db.get_settings()
    engine = profile.get("default_engine") or s["default_engine"]
    spec = engines.get_spec(engine)
    if text is None or not engines.is_downloaded(spec.repo):
        return False  # no big downloads just for a preview; the reference clip is used instead
    if delivery.backfill(profile, PROFILES_DIR / pid):
        db.put_profile(profile)
    params, qc = delivery.resolve({}, spec, {}, s)
    s = {**s, **qc}
    key = _preview_key(profile)
    vidx = key[0]
    ref_wav, ref_text, fp, self_sim, wps = profile_assets(profile, vidx)
    nmax = noise_limit(profile)
    best = None
    budget = 1.0
    for k in range(3):
        if cancel():
            return False
        speak = text if not (best and "cut_off" in best["issues"]) else text + " —"
        raw, sr = engines.synthesize(engine, speak, ref_wav, ref_text, profile.get("language") or "en", params, 4242 + k,
                                     expected_seconds=max(1.0, word_count(text) / wps) * budget)
        synthesis = engines.synthesis_report()
        ev = evaluate(raw, sr, text, fp, self_sim, wps, s, profile.get("language") or "en", noise_max=nmax)
        if synthesis.get("stop_reason") == "length_limit_reached":
            budget = min(4.0, budget * 1.3)
            if "cut_off" not in ev["issues"]:
                ev["issues"].append("cut_off")
                ev["score"] += 2.5
        # clean takes first, then the best score (a failed take must never beat a clean one)
        ev["has_hard"] = any(i in HARD_ISSUES for i in ev["issues"])
        if best is None or (ev["has_hard"], ev["score"]) < (best["has_hard"], best["score"]):
            best = ev
        if not ev["has_hard"]:
            break
    if "cut_off" in best["issues"]:
        return False  # Keep the existing preview rather than publish an incomplete take.
    a, sr = best["audio"], best["sr"]
    mic = PROFILES_DIR / pid / "mic.json"
    if mic.exists():
        a, _ = delivery.mic_match(a, sr, _json.loads(mic.read_text())["bands"])
    a = studio.master_chain(a, sr)
    a = au.normalize_lufs(a, sr, -16.0, peak_db=-1.0)
    a = np.concatenate([np.zeros(int(0.1 * sr), np.float32), a, np.zeros(int(0.3 * sr), np.float32)])
    tmp = PROFILES_DIR / pid / "preview.tmp.wav"
    au.save(tmp, a, sr)
    profile = db.get_profile(pid)  # re-read: the user may have edited meanwhile
    if profile is None:
        tmp.unlink(missing_ok=True)
        return False
    if _preview_key(profile) != key:
        tmp.unlink(missing_ok=True)
        return "stale"  # never publish a preview made from an older voice setting
    os.replace(tmp, PROFILES_DIR / pid / "preview.wav")
    profile["preview"] = {"file": "preview.wav", "engine": engine, "variant": vidx, "created": time.time(),
                          "duration": round(len(a) / sr, 2)}
    db.put_profile(profile)
    return True


def drop_preview(profile: dict) -> None:
    (PROFILES_DIR / profile["id"] / "preview.wav").unlink(missing_ok=True)
    profile.pop("preview", None)
