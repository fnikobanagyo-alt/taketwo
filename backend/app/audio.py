"""Audio I/O, cleanup, silence analysis and assembly helpers (numpy only)."""
from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import pyloudnorm as pyln
import soundfile as sf
from scipy.signal import butter, resample_poly, sosfiltfilt


def decode_to_wav(src: Path, dst: Path, sr: int, max_seconds: float | None = None) -> None:
    """Decode any format ffmpeg understands (mp3, m4a, webm, mp4...) to mono float wav."""
    cmd = ["ffmpeg", "-y", "-hide_banner", "-loglevel", "error", "-i", str(src), "-vn", "-ac", "1", "-ar", str(sr)]
    if max_seconds:
        cmd += ["-t", str(max_seconds)]
    cmd += ["-c:a", "pcm_f32le", str(dst)]
    subprocess.run(cmd, check=True)


def load(path: Path | str, sr: int | None = None) -> tuple[np.ndarray, int]:
    a, file_sr = sf.read(str(path), dtype="float32", always_2d=False)
    if a.ndim > 1:
        a = a.mean(axis=1)
    if sr and file_sr != sr:
        a = resample(a, file_sr, sr)
        file_sr = sr
    return a.astype(np.float32), file_sr


def save(path: Path | str, audio: np.ndarray, sr: int) -> None:
    sf.write(str(path), np.clip(audio, -1.0, 1.0), sr, subtype="PCM_16")


def export_mp3(wav: Path, mp3: Path) -> None:
    subprocess.run(
        ["ffmpeg", "-y", "-hide_banner", "-loglevel", "error", "-i", str(wav), "-b:a", "192k", str(mp3)],
        check=True,
    )


def resample(a: np.ndarray, sr_from: int, sr_to: int) -> np.ndarray:
    if sr_from == sr_to:
        return a
    g = np.gcd(sr_from, sr_to)
    return resample_poly(a, sr_to // g, sr_from // g).astype(np.float32)


def highpass(a: np.ndarray, sr: int, hz: float = 60.0) -> np.ndarray:
    sos = butter(4, hz, btype="highpass", fs=sr, output="sos")
    return sosfiltfilt(sos, a).astype(np.float32)


def loudness(a: np.ndarray, sr: int) -> float:
    if len(a) < int(0.5 * sr):
        return float("-inf")
    return float(pyln.Meter(sr).integrated_loudness(a))


def normalize_lufs(a: np.ndarray, sr: int, target: float, peak_db: float = -1.0) -> np.ndarray:
    cur = loudness(a, sr)
    if not np.isfinite(cur):
        return a
    out = a * (10 ** ((target - cur) / 20))
    peak = np.max(np.abs(out)) if len(out) else 0
    lim = 10 ** (peak_db / 20)
    if peak > lim:
        out = soft_limit(out, lim)
    return out.astype(np.float32)


def soft_limit(a: np.ndarray, ceiling: float) -> np.ndarray:
    """Transparent-ish limiter: tanh knee above 80% of the ceiling."""
    knee = 0.8 * ceiling
    out = a.copy()
    mask = np.abs(out) > knee
    over = np.abs(out[mask]) - knee
    out[mask] = np.sign(out[mask]) * (knee + (ceiling - knee) * np.tanh(over / (ceiling - knee)))
    return out


# ---------------------------------------------------------------- analysis

FRAME_MS = 20


def frame_db(a: np.ndarray, sr: int, frame_ms: int = FRAME_MS) -> np.ndarray:
    n = int(sr * frame_ms / 1000)
    if len(a) < n:
        return np.array([-120.0])
    frames = a[: len(a) // n * n].reshape(-1, n)
    rms = np.sqrt(np.mean(frames**2, axis=1) + 1e-12)
    return 20 * np.log10(rms + 1e-12)


def speech_threshold_db(db: np.ndarray) -> float:
    """Adaptive threshold: halfway between noise floor and speech level, clamped."""
    floor = np.percentile(db, 10)
    speech = np.percentile(db, 90)
    return float(max(min(floor + 0.35 * (speech - floor), speech - 18), -60))


@dataclass
class SilenceReport:
    lead: float
    tail: float
    max_internal: float
    internal_gaps: list[tuple[float, float]]  # (start, end) seconds
    speech_ratio: float


def silence_report(a: np.ndarray, sr: int, min_gap: float = 0.25) -> SilenceReport:
    db = frame_db(a, sr)
    thr = speech_threshold_db(db)
    voiced = db > thr
    fs = FRAME_MS / 1000
    if not voiced.any():
        d = len(a) / sr
        return SilenceReport(d, d, d, [], 0.0)
    idx = np.flatnonzero(voiced)
    first, last = idx[0], idx[-1]
    gaps = []
    run_start = None
    for i in range(first, last + 1):
        if not voiced[i]:
            if run_start is None:
                run_start = i
        elif run_start is not None:
            dur = (i - run_start) * fs
            if dur >= min_gap:
                gaps.append((run_start * fs, i * fs))
            run_start = None
    return SilenceReport(
        lead=first * fs,
        tail=(len(voiced) - 1 - last) * fs,
        max_internal=max((e - s for s, e in gaps), default=0.0),
        internal_gaps=gaps,
        speech_ratio=float(voiced.mean()),
    )


def _floor_threshold(db: np.ndarray) -> float:
    """Level that separates true silence from quiet speech (breathy onsets, decaying
    consonants). Much lower than the speech/pause threshold, so trimming never eats
    the soft end of a word."""
    # Relative to speech only. Basing this on the noise floor made voices with an airy
    # background (floor ~-25 dB) lose the soft end of every word.
    speech = np.percentile(db, 90)
    return float(speech - 48.0)


def trim_edges(a: np.ndarray, sr: int, keep_ms: int = 60, tail_ms: int = 220) -> np.ndarray:
    """Remove leading/trailing silence but keep natural onsets and word decays."""
    db = frame_db(a, sr)
    if len(db) < 3:
        return a
    idx = np.flatnonzero(db > _floor_threshold(db))
    if not len(idx):
        return a
    fs = FRAME_MS / 1000
    start = max(0.0, idx[0] * fs - keep_ms / 1000)
    end = min(len(a) / sr, (idx[-1] + 1) * fs + tail_ms / 1000)
    return a[int(start * sr) : int(end * sr)]


def speech_start(a: np.ndarray, sr: int, keep_ms: int = 100) -> float:
    """Conservative onset: retain soft initial speech and room before it."""
    db = frame_db(a, sr, 10)
    live = np.flatnonzero(db > _floor_threshold(db))
    return max(0.0, live[0] * 0.01 - keep_ms / 1000) if len(live) else 0.0


def ending_level(a: np.ndarray, sr: int) -> float:
    """Level (dB relative to speech) of the last non-silent 20 ms frame.

    A word that ends naturally decays to < -35 dB before silence. When a model stops
    mid-phoneme the audio falls off a cliff from speech level straight to digital
    silence ("at a ti-"). Values above about -37 dB mean the take was cut off."""
    db = frame_db(a, sr)
    if len(db) < 5:
        return -99.0
    speech = np.percentile(db, 90)
    live = np.flatnonzero(db > speech - 60)
    if not len(live):
        return -99.0
    return float(db[live[-1]] - speech)


def shorten_gaps(a: np.ndarray, sr: int, gaps: list[tuple[float, float]], max_gap: float) -> np.ndarray:
    """Collapse internal silences longer than max_gap down to max_gap (with fades)."""
    if not gaps:
        return a
    pieces, cursor = [], 0
    for s, e in gaps:
        if e - s <= max_gap:
            continue
        keep_half = max_gap / 2
        cut_a = int((s + keep_half) * sr)
        cut_b = int((e - keep_half) * sr)
        pieces.append(fade(a[cursor:cut_a], sr, fade_out_ms=8))
        cursor = cut_b
    pieces.append(fade(a[cursor:], sr, fade_in_ms=8))
    return np.concatenate(pieces)


def fade(a: np.ndarray, sr: int, fade_in_ms: int = 0, fade_out_ms: int = 0) -> np.ndarray:
    a = a.copy()
    if fade_in_ms:
        n = min(len(a), int(sr * fade_in_ms / 1000))
        a[:n] *= np.linspace(0, 1, n, dtype=np.float32) ** 2
    if fade_out_ms:
        n = min(len(a), int(sr * fade_out_ms / 1000))
        if n:
            a[-n:] *= np.linspace(1, 0, n, dtype=np.float32) ** 2
    return a


def clipping_ratio(a: np.ndarray) -> float:
    return float(np.mean(np.abs(a) > 0.985)) if len(a) else 0.0


def noise_bursts(a: np.ndarray, sr: int) -> float:
    """Longest run (seconds) of loud, noise-like frames.

    Static / hiss / crackle bursts that TTS codecs sometimes emit have a nearly flat
    power spectrum (flatness ~0.5); speech, even fricatives, stays far below 0.05.
    """
    n = int(0.03 * sr)
    if len(a) < n * 4:
        return 0.0
    frames = a[: len(a) // n * n].reshape(-1, n) * np.hanning(n)
    spec = np.abs(np.fft.rfft(frames, axis=1)) ** 2 + 1e-12
    flat = np.exp(np.mean(np.log(spec), axis=1)) / np.mean(spec, axis=1)
    db = 10 * np.log10(np.mean(spec, axis=1))
    bad = (db > np.percentile(db, 90) - 25) & (flat > 0.1)
    best = run = 0
    for b in bad:
        run = run + 1 if b else 0
        best = max(best, run)
    return best * n / sr


def estimate_snr(a: np.ndarray, sr: int) -> float:
    db = frame_db(a, sr)
    return float(np.percentile(db, 95) - np.percentile(db, 10))


def concat_with_pauses(chunks: list[np.ndarray], pauses_ms: list[int], sr: int) -> np.ndarray:
    """Join already edge-trimmed chunks with short fades and natural pauses.

    Deliberately no overlapping crossfade: overlapping two different takes of speech
    is one of the causes of garbled/"weird" joins.
    """
    out = []
    for i, c in enumerate(chunks):
        # chunks already carry ~220 ms of natural decay; this only removes clicks
        # Current chunks have silence padding. Legacy chunks may start/end with
        # speech, so never apply a joining fade to their spoken samples.
        head = int(sr * 6 / 1000)
        tail = int(sr * 25 / 1000)
        out.append(fade(c, sr,
                        fade_in_ms=6 if len(c) >= head and np.max(np.abs(c[:head])) < 1e-6 else 0,
                        fade_out_ms=25 if len(c) >= tail and np.max(np.abs(c[-tail:])) < 1e-6 else 0))
        if i < len(chunks) - 1:
            out.append(np.zeros(int(sr * pauses_ms[i] / 1000), dtype=np.float32))
    return np.concatenate(out) if out else np.zeros(0, dtype=np.float32)


# ---------------------------------------------------------------- sentence-end analysis


def sound_regions(a: np.ndarray, sr: int, bridge_ms: int = 130) -> tuple[list[tuple[int, int]], np.ndarray, float]:
    """Regions of sound (10 ms frames), where silences shorter than `bridge_ms` are bridged.
    Stop-consonant closures and bursts ("adapte-d", "doorste-p") are shorter than that, so
    they stay inside the word instead of looking like an end. Returns (regions, db, speech_level)."""
    db = frame_db(a, sr, 10)
    n = len(db)
    sp = float(np.percentile(db, 90))
    thr = float(min(np.percentile(db, 10) + 10.0, sp - 20.0))
    voiced = db >= thr
    gap = max(1, int(bridge_ms / 10))
    regions: list[tuple[int, int]] = []
    i = 0
    while i < n:
        if not voiced[i]:
            i += 1
            continue
        j = i
        while j < n:
            if voiced[j]:
                j += 1
                continue
            k = j
            while k < n and not voiced[k]:
                k += 1
            if k < n and (k - j) < gap:
                j = k
            else:
                break
        regions.append((i, j))
        i = j
    return regions, db, sp


def end_analysis(a: np.ndarray, sr: int, last_word_end: float, tail_s: float = 0.12) -> dict:
    """Where the last word's sound really ends, whether it ends naturally, and any junk after.

    * cut: time to trim to. Everything after the last word is kept until 250 ms of silence
      (a split-off final consonant or a soft release stays; the next phrase does not)
    * natural: False when the audio stops while still loud (a real cut-off)
    * junk: a separate loud sound of real length after the last word (stray next word, babble)
    """
    regions, db, sp = sound_regions(a, sr)
    n = len(db)
    if not regions:
        return {"start": 0.0, "cut": n * 0.01, "natural": True, "junk": False, "last_level": -99.0}
    lw = int(max(0.0, last_word_end - 0.05) / 0.01)
    idx = next((k for k, (s0, e0) in enumerate(regions) if e0 >= lw), len(regions) - 1)
    s0, e0 = regions[idx]
    end = _terminal_end(db, max(s0, lw - 15), sp)
    if end is None:  # no audible sound at or after the word: fall back to the word's own region
        end = e0
    junk = any(
        (e1 - s1) >= 8 and s1 >= end and float(np.percentile(db[s1:e1], 90) - sp) > -27.0
        for s1, e1 in regions[idx + 1 :]
    )
    judge = end
    if not junk:
        # A short release after a pause is not a stray phrase. Keep it rather
        # than silently deleting sound that our own junk check accepted.
        audible = np.flatnonzero(db > sp - 48.0)
        if len(audible) and int(audible[-1]) + 1 > end:
            tail = db[end : int(audible[-1]) + 1]
            # Judge the ending at a real release ("stopped": closure, then "t"), not at a faint
            # sound 30+ dB under speech: that would let a hard stop pass as natural (goal item 2).
            if float(tail.max()) >= sp - 30.0:
                judge = int(audible[-1]) + 1
            end = int(audible[-1]) + 1
    tail_frames = min(int(tail_s / 0.01), max(0, n - end))
    cut = len(a) / sr if end + tail_frames >= n else (end + tail_frames) * 0.01
    # Average energy, not decibels: one quiet frame must not hide the loud
    # frame immediately before it. Judge the untouched take, before padding.
    levels = db[max(s0, judge - 2) : judge] - sp
    last_level = float(10 * np.log10(np.mean(10 ** (levels / 10))))
    natural = last_level <= -24.0
    return {"start": regions[0][0] * 0.01, "cut": min(cut, len(a) / sr), "natural": natural, "junk": junk, "last_level": last_level}


TERMINAL_SILENCE_FRAMES = 25  # 250 ms of silence closes a word; the next phrase starts after it


def _terminal_end(db: np.ndarray, scan0: int, sp: float) -> int | None:
    """Frame just after the last audible frame that follows the last word, or None.

    Scanning stops at the first silence of TERMINAL_SILENCE_FRAMES, so a quiet or split-off
    final sound that sits within a normal pause is kept, while a separate phrase is not."""
    thr = sp - 48.0
    end, quiet = None, 0
    # A scan budget is not a speech endpoint. Continue to sustained silence or
    # the physical file end; otherwise an early ASR timestamp truncates the word.
    for i in range(max(0, scan0), len(db)):
        if db[i] > thr:
            end, quiet = i + 1, 0
        else:
            quiet += 1
            if quiet >= TERMINAL_SILENCE_FRAMES:
                break
    return end


def lead_burst(a: np.ndarray, sr: int) -> tuple[float, float] | None:
    """(start, length) of a short hiss opening the take, set apart from what follows.

    A reference clip cut before its transcript ends makes takes open by finishing that word
    (one voice's clip lost the "s" of "links", so takes began "xs"). Whisper ignores the burst. A real
    S runs into its word within tens of ms, so only hiss followed by a clear pause counts."""
    n = int(0.01 * sr)
    m = min(len(a), int(1.2 * sr)) // n
    if m < 20:
        return None
    level = lambda x: 20 * np.log10(np.sqrt(np.mean(x[: m * n].reshape(m, n) ** 2, axis=1)) + 1e-9)
    full = level(a)
    high = level(sosfiltfilt(butter(4, [4000, min(10000, sr / 2 * 0.95)], btype="band", fs=sr, output="sos"), a))
    loud = full > np.percentile(frame_db(a, sr, 10), 90) - 30
    if not loud.any():
        return None
    start = int(np.argmax(loud))
    end = start + int(np.argmin(loud[start:])) if not loud[start:].all() else m
    rest = np.flatnonzero(loud[end:])
    if not len(rest) or not 4 <= end - start <= 30 or rest[0] < 15 or np.mean(high[start:end] - full[start:end]) <= -5:
        return None
    return start * 0.01, (end - start) * 0.01


def edge_silence(a: np.ndarray, sr: int) -> tuple[float, float]:
    """(lead, tail) seconds of real silence at the edges of a part."""
    regions, db, sp = sound_regions(a, sr, bridge_ms=60)
    if not regions:
        return 0.0, 0.0
    return regions[0][0] * 0.01, (len(db) - regions[-1][1]) * 0.01
