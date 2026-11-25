"""Experimental spoken-continuation strategy, disabled by default.

ASR word coverage cannot prove a complete final consonant. Keeping the requested
ending away from the model's end-of-sequence is preventive, not a phoneme score.
Only English is enabled until other languages have a tested continuation.

Listening follow-up: the user confirmed that a raw take still clipped "village"
before the continuation. This guard cannot repair articulation missing in synthesis.
"""
from __future__ import annotations

import difflib
import re
import numpy as np

from . import audio as au

CONTINUATION = "Meanwhile, another chapter begins."


class UnsafeBoundary(ValueError):
    pass


def synthesis_text(text: str, family: str, language: str, *, enabled: bool = False) -> tuple[str, bool]:
    guarded = enabled and family == "qwen3" and language.lower() in ("en", "english")
    if not guarded:
        return text, False
    # A period also separates chunks ending at a comma/semicolon or without punctuation.
    terminal = "" if text.rstrip().endswith((".", "!", "?")) else "."
    return text.rstrip() + terminal + "\n\n" + CONTINUATION, True


def _tokens(text: str) -> list[str]:
    return re.findall(r"[^\W_]+", text.casefold(), flags=re.UNICODE)


def remove_context(a: np.ndarray, sr: int, text: str, transcript: dict) -> tuple[np.ndarray, dict, dict]:
    """Return a sample-exact prefix and its ASR, or reject an ambiguous boundary.

    Match the entire continuation at the transcript tail. Word times only bound
    the search; the cut must be inside >=60 ms of acoustic silence. Search at a
    low threshold to keep quiet releases/fricatives, and choose the last pause
    before the context, rather than a closure inside the requested final word.
    """
    words = [w for seg in transcript.get("segments", []) for w in seg.get("words", [])]
    tokens, owners = [], []
    for wi, word in enumerate(words):
        for token in _tokens(word["word"]):
            tokens.append(token)
            owners.append(wi)
    suffix = _tokens(CONTINUATION)
    if len(tokens) <= len(suffix) or tokens[-len(suffix):] != suffix:
        raise UnsafeBoundary("Continuation was not fully recognized")
    first_context = owners[len(tokens) - len(suffix)]
    if first_context <= 0 or owners[len(tokens) - len(suffix) - 1] == first_context:
        raise UnsafeBoundary("Requested ending and continuation share a word timestamp")
    expected = _tokens(text)
    last_target = words[first_context - 1]
    actual = _tokens(last_target["word"])
    if not expected or not actual or difflib.SequenceMatcher(None, expected[-1], ''.join(actual)).ratio() < .6:
        raise UnsafeBoundary("Requested final word was not recognized before the continuation")
    context_start = float(words[first_context]["start"])
    last_end = float(last_target["end"])
    lo = max(float(last_target["start"]) + .06, last_end - .15)
    hi = context_start - .02
    db = au.frame_db(a, sr, 10)
    if not len(db) or not 0 <= lo < hi <= len(a) / sr:
        raise UnsafeBoundary("No room to separate the requested ending")
    # Compare to the target voice, not an unusually loud continuation.
    target_db = db[:max(1, int(lo / .01))]
    left, right = int(np.ceil(lo / .01)), int(np.floor(hi / .01))
    if right - left < 6:
        raise UnsafeBoundary("Word timestamps leave no measurable pause")
    # Codec silence has a noise floor. Require a valley close to that floor and
    # at least 42 dB below speech; a fixed -52 dB test rejects noisy quiet gaps.
    floor = float(np.percentile(db[left:right], 10))
    threshold = max(-105., min(float(np.percentile(target_db, 90)) - 42., floor + 6.))
    quiet = db[left:right] < threshold
    edges = np.diff(np.r_[False, quiet, False].astype(int))
    runs = [(left + s, left + e) for s, e in zip(np.flatnonzero(edges == 1), np.flatnonzero(edges == -1)) if e - s >= 6]
    if not runs:
        raise UnsafeBoundary("No verified quiet pause before the continuation")
    start, end = runs[-1]
    cut = int(round((start + end) * .005 * sr))
    retained = words[:first_context]
    tr = {"text": " ".join(w["word"].strip() for w in retained), "segments": [{"words": retained}]}
    info = {"version": 2, "continuation": CONTINUATION, "full_duration": len(a) / sr,
            "cut_seconds": cut / sr, "pause_start": start * .01, "pause_end": end * .01,
            "context_start": context_start, "last_word_end": last_end, "threshold_db": threshold}
    return a[:cut].copy(), tr, info
