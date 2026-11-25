"""Deep analysis of an uploaded voice sample (up to 5 minutes).

Cloning models condition on a short prompt (~6-20 s); feeding them 5 minutes directly
makes them slower and *less* stable. So we use the whole upload to understand the voice
and then hand the model the best possible prompt:

1. Clean: decode, mono 24 kHz, high-pass, loudness-normalise (optionally denoise).
2. Transcribe everything with Whisper large-v3-turbo, word-level timestamps.
3. Split into utterances; score each for clarity (ASR confidence), noise, clipping,
   level and *speaker consistency* (similarity to the fingerprint of the whole file,
   which filters out other voices, music, laughs, coughs).
4. Build a voice fingerprint (averaged speaker embedding) from all good speech.
   Every generated chunk is later checked against this fingerprint.
5. Choose the best contiguous window (default ~12 s, cut on word boundaries with an
   exact transcript) plus alternates the user can audition and switch between.
"""
from __future__ import annotations

import json
from pathlib import Path

import numpy as np

from . import audio as au
from . import delivery, ml, studio
from .config import MAX_REFERENCE_SECONDS, WORK_SR


def _utterances(segments: list[dict], total: float) -> list[dict]:
    """Word-level regrouping into utterances split at pauses >= 0.35s or sentence ends."""
    words = [w for s in segments for w in s["words"]] if segments else []
    seg_of_word = []
    for si, s in enumerate(segments):
        seg_of_word += [si] * len(s["words"])
    utts, cur = [], []
    for i, w in enumerate(words):
        if cur:
            gap = w["start"] - cur[-1]["end"]
            ends_sentence = cur[-1]["word"].strip().endswith((".", "!", "?"))
            dur = cur[-1]["end"] - cur[0]["start"]
            if gap >= 0.35 or (ends_sentence and gap >= 0.12) or dur > 14:
                utts.append(cur)
                cur = []
        cur.append({**w, "seg": seg_of_word[i]})
    if cur:
        utts.append(cur)
    out = []
    for u in utts:
        s0 = segments[u[0]["seg"]]
        out.append(
            {
                "start": max(0.0, u[0]["start"]),
                "end": min(total, u[-1]["end"]),
                "text": "".join(w["word"] for w in u).strip(),
                "word_p": float(np.mean([w["p"] for w in u])),
                "avg_logprob": s0["avg_logprob"],
                "no_speech_prob": s0["no_speech_prob"],
                "n_words": len(u),
            }
        )
    return out


def pause_threshold(db10: np.ndarray) -> float:
    """Level below which a 10 ms frame counts as a real pause (works for noisy and clean audio)."""
    return float(min(np.percentile(db10, 10) + 10.0, np.percentile(db10, 90) - 20.0))


def _valleys(db10: np.ndarray, thr: float, min_frames: int = 6) -> list[tuple[int, int]]:
    below = db10 < thr
    out, i, n = [], 0, len(below)
    while i < n:
        if below[i]:
            j = i
            while j < n and below[j]:
                j += 1
            if j - i >= min_frames:
                out.append((i, j))
            i = j
        else:
            i += 1
    return out


def _end_pauses(db10: np.ndarray, thr: float, next_start: float | None = None) -> list[tuple[int, int]]:
    """Pauses a clip may end in. A dip of up to 200 ms followed by up to 300 ms of audible sound,
    before the next word starts, is a stop consonant's closure: ending there loses the word's last
    sound ("links" without its "ks"). Whisper still hears the word, so later checks can't catch it.
    Sound more than 30 dB under speech is a breath or room tail, so that dip is a real pause."""
    vs = _valleys(db10, thr)
    audible = float(np.percentile(db10, 90)) - 30.0
    pauses = []
    for k, (a, b) in enumerate(vs):
        resume_end = vs[k + 1][0] if k + 1 < len(vs) else len(db10)
        next_word = next_start is not None and b * 0.01 >= next_start - 0.05
        final_sound = b - a <= 20 and 0 < resume_end - b <= 30 and float(db10[b:resume_end].max()) >= audible
        if next_word or not final_sound:
            pauses.append((a, b))
    return pauses


def snap_window(db10: np.ndarray, start_t: float, end_t: float, next_start: float | None, total: float) -> tuple[float, float] | None:
    """Move a clip's cut points into real pauses so it never starts or ends inside a word.

    Reference clips are copied by the cloning model, including how they end. A clip chopped
    inside a word teaches it to chop its own sentence endings. The end keeps the word's
    natural decay plus a little pause; the start keeps a little pre-roll. Returns None when
    no clean pause exists nearby (the window is then not used)."""
    thr = pause_threshold(db10)
    vs = _valleys(db10, thr)
    fs = 0.01
    # --- end: first real pause after the last word, before the next word begins
    limit = min(end_t + 0.7, (next_start - 0.02) if next_start else total)
    end = None
    for a, b in _end_pauses(db10, thr, next_start):
        if a * fs >= end_t - 0.10 and a * fs <= limit:
            end = a * fs + min(0.22, 0.65 * (b - a) * fs)
            break
    if end is None:
        return None
    # --- start: last real pause before the first word
    start = None
    if start_t <= 0.3:
        start = 0.0
    else:
        for a, b in reversed(vs):
            if b * fs <= start_t + 0.10 and b * fs >= start_t - 0.6:
                start = b * fs - min(0.12, 0.5 * (b - a) * fs)
                break
    if start is None or end - start < 3.0 or end > total:
        return None
    return max(0.0, start), end


def room_echo(a: np.ndarray, sr: int) -> float:
    """How loud the gaps between words are relative to speech (dB). Echo and hiss fill
    the gaps: studio recordings sit below about -40 dB, echoey rooms above -28 dB."""
    db = au.frame_db(a, sr)
    return float(np.percentile(db, 10) - np.percentile(db, 90))


def analyze(src_path: Path, out_dir: Path, language: str | None = "en", denoise: bool = False, target_window: float = 12.0, progress=lambda p, m: None, studio_mode: bool = False) -> dict:
    out_dir.mkdir(parents=True, exist_ok=True)
    progress(0.05, "Decoding audio")
    clean_path = out_dir / "clean.wav"
    au.decode_to_wav(src_path, clean_path, WORK_SR, MAX_REFERENCE_SECONDS)
    a, sr = au.load(clean_path)
    total = len(a) / sr
    if total < 3:
        raise ValueError("Sample is too short. Upload at least 3 seconds (30 s - 5 min recommended).")

    echo_before = room_echo(a, sr)
    echo_after = None
    snr_before = au.estimate_snr(au.highpass(a, sr, 70), sr)
    if studio_mode:
        progress(0.08, "Restoring to studio quality: removing echo, noise and mic colour")
        y, ysr = studio.restore(a, sr, progress=lambda p: progress(0.08 + 0.1 * p, "Restoring to studio quality: removing echo, noise and mic colour"))
        au.save(out_dir / "studio.wav", y, ysr)
        a = au.resample(y, ysr, sr)
        echo_after = room_echo(a, sr)
    mic_bands = delivery.ltas(a, sr)  # tonal fingerprint the output should match (studio or original mic)
    a = au.highpass(a, sr, 70)
    if not studio_mode and (denoise or snr_before < 25):
        progress(0.1, "Reducing background noise")
        import noisereduce as nr

        a = nr.reduce_noise(y=a, sr=sr, stationary=True, prop_decrease=0.75).astype(np.float32)
    a = au.normalize_lufs(a, sr, -20.0, peak_db=-1.5)
    au.save(clean_path, a, sr)

    progress(0.2, "Transcribing the full sample (Whisper large-v3-turbo)")
    tr = ml.transcribe(a, sr, language=language, words=True)
    utts = _utterances(tr["segments"], total)
    if not utts:
        raise ValueError("No speech found in the sample.")

    progress(0.55, "Building voice fingerprint")
    for i, u in enumerate(utts):
        seg = a[int(u["start"] * sr) : int(u["end"] * sr)]
        u["dur"] = u["end"] - u["start"]
        u["snr"] = au.estimate_snr(seg, sr) if len(seg) > sr * 0.3 else 0.0
        u["clip"] = au.clipping_ratio(seg)
        u["emb"] = ml.speaker_embedding(seg, sr) if u["dur"] >= 1.2 else None
        progress(0.55 + 0.25 * (i + 1) / len(utts), "Building voice fingerprint")

    embs = [u["emb"] for u in utts if u["emb"] is not None]
    if not embs:
        embs = [ml.speaker_embedding(a, sr)]
    centroid = np.mean(embs, axis=0)
    centroid /= np.linalg.norm(centroid) + 1e-9
    # second pass: re-centre on the dominant speaker (drop outliers)
    sims = [ml.cosine(e, centroid) for e in embs]
    keep = [e for e, s in zip(embs, sims) if s >= np.median(sims) - 0.1]
    if keep:
        centroid = np.mean(keep, axis=0)
        centroid /= np.linalg.norm(centroid) + 1e-9
    for u in utts:
        u["sim"] = ml.cosine(u["emb"], centroid) if u["emb"] is not None else None

    good_sims = [u["sim"] for u in utts if u["sim"] is not None]
    self_sim = float(np.median(good_sims)) if good_sims else 0.75

    # utterance quality 0..1
    for u in utts:
        clarity = np.clip((u["word_p"] - 0.5) / 0.45, 0, 1)
        noise = np.clip((u["snr"] - 20) / 25, 0, 1)
        spk = np.clip(((u["sim"] if u["sim"] is not None else self_sim - 0.1) - (self_sim - 0.15)) / 0.2, 0, 1)
        clip_pen = 1.0 if u["clip"] > 0.001 else 0.0
        u["score"] = float(0.35 * clarity + 0.25 * noise + 0.4 * spk - 0.5 * clip_pen - 0.5 * (u["no_speech_prob"] > 0.5))

    progress(0.85, "Measuring delivery and choosing reference clips")
    db10 = au.frame_db(a, sr, 10)
    windows = _select_windows(utts, target_window, a, sr, db10=db10, total=total)
    if not windows:
        raise ValueError("Could not find a clean stretch of speech. Try a clearer recording.")
    variants = []
    for w_i, (i, j, score, expr, pro, snapped) in enumerate(windows):
        start, end = snapped
        clip = au.fade(a[int(start * sr) : int(end * sr)], sr, fade_in_ms=15, fade_out_ms=20)
        path = out_dir / f"ref_{w_i}.wav"
        au.save(path, clip, sr)
        variants.append(
            {
                "index": w_i, "file": path.name, "start": round(start, 2), "end": round(end, 2),
                "duration": round(end - start, 2), "score": round(score, 3),
                "text": " ".join(u["text"] for u in utts[i : j + 1]).strip(),
                "expressiveness": round(expr, 3), "pitch_range": round(pro["pitch_range"], 1),
                "delivery": "expressive" if expr >= 0.67 else "calm" if expr <= 0.33 else "balanced",
            }
        )
    # present best quality first, then by delivery so choices are easy to scan
    variants.sort(key=lambda v: -v["score"])
    for k, v in enumerate(variants):
        src = out_dir / v["file"]
        v["index"] = k
        v["file"] = f"ref_{k}.wav"
        src.rename(out_dir / f"tmp_{k}.wav")
    for k in range(len(variants)):
        (out_dir / f"tmp_{k}.wav").rename(out_dir / f"ref_{k}.wav")

    np.save(out_dir / "fingerprint.npy", centroid.astype(np.float32))
    (out_dir / "mic.json").write_text(json.dumps({"sr": sr, "bands": mic_bands}))
    good_seconds = float(sum(u["dur"] for u in utts if u["score"] > 0.5))
    analysis = {
        "duration": round(total, 2),
        "language": tr.get("language") or language,
        "snr_db": round(snr_before, 1),
        "denoised": bool(not studio_mode and (denoise or snr_before < 25)),
        "studio": studio_mode, "room_echo_db": round(echo_before, 1),
        "room_echo_after_db": round(echo_after, 1) if echo_after is not None else None,
        "self_similarity": round(self_sim, 3),
        "usable_speech_seconds": round(good_seconds, 1),
        "transcript": tr["text"],
        "utterances": [
            {k: (round(v, 3) if isinstance(v, float) else v) for k, v in u.items() if k != "emb"} for u in utts
        ],
        "variants": variants,
        "tips": _tips(total, snr_before, self_sim, good_seconds, echo_before, studio_mode),
    }
    (out_dir / "analysis.json").write_text(json.dumps(analysis, indent=1))
    progress(1.0, "Done")
    return analysis


def _select_windows(utts: list[dict], target: float, a: np.ndarray, sr: int, n: int = 4, db10: np.ndarray | None = None, total: float | None = None):
    """Best-quality clips that start and end in real pauses, guaranteed to include the
    speaker's most expressive and calmest clean deliveries."""
    total = total if total is not None else len(a) / sr
    db10 = db10 if db10 is not None else au.frame_db(a, sr, 10)
    cands = _candidate_windows(utts, target, limit=40)
    scored = []
    for i, j, q in cands:
        nxt = utts[j + 1]["start"] if j + 1 < len(utts) else None
        snapped = snap_window(db10, utts[i]["start"], utts[j]["end"], nxt, total)
        if snapped is None:
            continue
        seg = a[int(snapped[0] * sr) : int(snapped[1] * sr)]
        pro = delivery.prosody(seg, sr)
        scored.append([i, j, q, delivery.expressiveness(pro), pro, snapped])
        if len(scored) >= 14:
            break
    if not scored:
        return []
    raw = np.array([c[3] for c in scored])
    ranks = raw.argsort().argsort() / max(1, len(raw) - 1)  # percentile within this speaker
    for c, r in zip(scored, ranks):
        c[3] = float(r) if len(scored) > 1 else 0.5
    best_q = max(c[2] for c in scored)
    good = [c for c in scored if c[2] >= best_q - 0.2] or scored
    picks = [max(scored, key=lambda c: c[2])]
    for extreme in (max(good, key=lambda c: c[3]), min(good, key=lambda c: c[3])):
        if all(extreme is not p for p in picks):
            picks.append(extreme)
    for c in sorted(scored, key=lambda c: -c[2]):
        if len(picks) >= n:
            break
        if all(c is not p for p in picks):
            picks.append(c)
    return [tuple(c) for c in picks]


def _candidate_windows(utts: list[dict], target: float, limit: int = 4) -> list[tuple[int, int, float]]:
    """Contiguous utterance spans ~target seconds, ranked; non-overlapping picks."""
    lo, hi = max(5.0, target * 0.6), target * 1.5
    cands = []
    for i in range(len(utts)):
        for j in range(i, len(utts)):
            dur = utts[j]["end"] - utts[i]["start"]
            if dur > hi:
                break
            if dur < lo:
                continue
            span = utts[i : j + 1]
            gaps = [span[k + 1]["start"] - span[k]["end"] for k in range(len(span) - 1)]
            if gaps and max(gaps) > 1.2:  # a long pause inside the prompt teaches the model to pause
                continue
            q = float(np.mean([u["score"] for u in span]))
            worst = min(u["score"] for u in span)
            ends_clean = span[-1]["text"].rstrip().endswith((".", "!", "?"))
            s = q + 0.3 * worst + 0.05 * ends_clean - 0.02 * abs(dur - target)
            cands.append((i, j, s))
    cands.sort(key=lambda c: -c[2])
    picked: list[tuple[int, int, float]] = []
    for c in cands:
        if all(c[1] < p[0] or c[0] > p[1] for p in picked):
            picked.append(c)
        if len(picked) >= limit:
            break
    if not picked and utts:  # very short sample: use everything
        picked = [(0, len(utts) - 1, float(np.mean([u["score"] for u in utts])))]
    return picked


def _tips(total, snr, self_sim, good_seconds, echo=-50.0, studio_mode=False) -> list[str]:
    tips = []
    if echo > -30 and not studio_mode:
        tips.append("This recording has noticeable room echo or background noise, which clones can pick up as a robotic or echoey sound. Turn on Studio quality to remove it.")
    if total < 20:
        tips.append("Short sample: 1-5 minutes of clean speech gives a better voice fingerprint and more reference options.")
    if snr < 25 and not studio_mode:
        tips.append("Background noise detected and reduced. A quieter recording will clone more accurately.")
    if self_sim < 0.6:
        tips.append("The sample seems to contain more than one voice, or music. Upload only the target speaker for best results.")
    if good_seconds < 8:
        tips.append("Very little clean speech found. Record in a quiet room, 15-30 cm from the mic.")
    return tips


def refresh_clips(pdir: Path, profile: dict) -> dict:
    """Non-destructive upgrade of a saved voice: re-cut its reference clips inside real
    pauses (never mid-word) from the same cleaned recording.

    Keeps everything the user chose or edited: name, description, which clip is in use,
    transcript edits, delivery tags, voice fingerprint. A re-cut clip is only accepted if
    Whisper hears exactly the stored transcript in it, so a clip can't silently gain or
    lose a word. The previous clip is kept as ref_N.v3.wav."""
    import shutil

    a, sr = au.load(pdir / "clean.wav")
    total = len(a) / sr
    utts = json.loads((pdir / "analysis.json").read_text())["utterances"]
    db10 = au.frame_db(a, sr, 10)
    report = {"updated": [], "kept": []}
    for v in profile.get("variants") or []:
        idx = v["index"]
        inside = [u for u in utts if u["start"] >= v["start"] - 0.15 and u["end"] <= v["end"] + 0.06]
        if not inside:
            report["kept"].append(idx)
            continue
        first, last = inside[0], inside[-1]
        nxt = next((u["start"] for u in utts if u["start"] > last["end"] + 0.005), None)
        snapped = snap_window(db10, first["start"], last["end"], nxt, total)
        text = (profile.get("ref_text_override") or {}).get(str(idx)) or v["text"]
        ok = False
        if snapped is not None:
            s0, e0 = snapped
            clip = au.fade(a[int(s0 * sr) : int(e0 * sr)], sr, fade_in_ms=15, fade_out_ms=20)
            heard = ml.transcribe(clip, sr, language=profile.get("language") or "en")["text"]
            ok = ml.wer(text, heard) <= 0.12
        if not ok:
            # Second chance, only for a clip that really ends inside a sound and whose transcript
            # the user hasn't edited: end it at the previous natural pause (drops the unfinished
            # last word) and take the transcript from what is actually spoken.
            thr = pause_threshold(db10)
            chopped = bool(np.max(db10[max(0, int(v["end"] / 0.01) - 3) : int(v["end"] / 0.01)]) > thr)
            edited = str(idx) in (profile.get("ref_text_override") or {})
            back = None
            if chopped and not edited:
                for va, vb in reversed(_end_pauses(db10, thr)):
                    if vb * 0.01 <= v["end"] - 0.15 and va * 0.01 >= v["start"] + 3.0:
                        back = va * 0.01 + min(0.22, 0.65 * (vb - va) * 0.01)
                        break
            if back is None:
                report["kept"].append(idx)
                continue
            s0, e0 = v["start"], back
            clip = au.fade(a[int(s0 * sr) : int(e0 * sr)], sr, fade_in_ms=15, fade_out_ms=20)
            heard = ml.transcribe(clip, sr, language=profile.get("language") or "en")["text"].strip()
            if len(heard.split()) < 0.6 * len(v["text"].split()):
                report["kept"].append(idx)
                continue
            v["text"] = heard
            v["trimmed_last_word"] = True
        path = pdir / v["file"]
        backup = path.with_name(path.stem + ".v3.wav")
        if path.exists() and not backup.exists():
            shutil.copyfile(path, backup)
        au.save(path, clip, sr)
        v.update(start=round(s0, 2), end=round(e0, 2), duration=round(e0 - s0, 2), clip_version=4)
        report["updated"].append(idx)
        if v.get("trimmed_last_word"):
            report.setdefault("trimmed", []).append(idx)
    return report
