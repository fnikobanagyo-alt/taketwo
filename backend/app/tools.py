"""Extra studio tools: subtitles, speech-to-text, voice design."""
from __future__ import annotations

import json
import re
import time
import uuid
from pathlib import Path

import numpy as np

from . import audio as au
from . import engines, ml
from .config import DESIGNS_DIR, TRANSCRIPTS_DIR, WORK_SR

# ------------------------------------------------------------------ subtitles

MAX_CAPTION_CHARS = 84  # two lines of ~42


def _caption_pieces(text: str) -> list[str]:
    """Sentence -> caption-sized phrases, split at clause punctuation then words."""
    out = []
    for sent in re.split(r"(?<=[.!?])\s+", text.strip()):
        if len(sent) <= MAX_CAPTION_CHARS:
            out.append(sent)
            continue
        buf = ""
        for part in re.split(r"(?<=[,;:])\s+", sent):
            for w in part.split():
                if buf and len(buf) + len(w) + 1 > MAX_CAPTION_CHARS:
                    out.append(buf)
                    buf = w
                else:
                    buf = f"{buf} {w}".strip()
            if len(buf) > MAX_CAPTION_CHARS * 0.55:
                out.append(buf)
                buf = ""
        if buf:
            out.append(buf)
    return [o for o in out if o]


def generation_captions(gen: dict) -> list[dict]:
    """Captions timed from each chunk's real position in the final audio.

    Inside a chunk (<= ~12 s) time is shared by character count, which lands within a
    few hundred ms of the spoken words: good enough for YouTube captions.
    """
    chunks = {c["i"]: c for c in gen["chunks"]}
    caps = []
    for t in gen.get("timeline", []):
        pieces = _caption_pieces(chunks[t["i"]]["text"])
        total = sum(len(p) for p in pieces) or 1
        cur = t["start"]
        span = t["end"] - t["start"]
        for p in pieces:
            d = span * len(p) / total
            caps.append({"start": cur, "end": cur + d, "text": p})
            cur += d
    return caps


def generation_timings(gen: dict) -> dict:
    """Start/end of every sentence and script word in the final file (after speed).

    Word times are the kept takes' own Whisper times. A word Whisper misheard takes the time between its
    heard neighbours. Parts recorded before word times were kept are shared by character count ("estimated")."""
    import difflib

    from .textproc import split_sentences

    norm = lambda w: "".join(c for c in w.lower() if c.isalnum() or c == "'")
    speed = float(gen.get("speed") or 1.0)
    chunks = {c["i"]: c for c in gen["chunks"]}
    words, sentences, estimated = [], [], False
    for t in gen.get("timeline", []):
        ch = chunks[t["i"]]
        script, heard = ch["text"].split(), ch.get("words")
        if heard:
            times: list = [None] * len(script)
            sm = difflib.SequenceMatcher(None, [norm(w) for w in script], [norm(w["word"]) for w in heard], autojunk=False)
            for i, j, size in sm.get_matching_blocks():
                for k in range(size):
                    times[i + k] = (heard[j + k]["start"], heard[j + k]["end"])
            for i in range(len(script)):
                if times[i] is None:
                    prev = next((times[p][1] for p in range(i - 1, -1, -1) if times[p]), 0.0)
                    nxt = next((times[n][0] for n in range(i + 1, len(script)) if times[n]), prev)
                    times[i] = (prev, max(prev, nxt))
            span = [(t["start"] + s / speed, t["start"] + e / speed) for s, e in times]
        else:
            estimated = True
            total, cur, span = sum(len(w) for w in script) or 1, t["start"], []
            for w in script:
                d = (t["end"] - t["start"]) * len(w) / total
                span.append((cur, cur + d))
                cur += d
        part = [{"word": w, "start": round(s, 3), "end": round(e, 3)} for w, (s, e) in zip(script, span)]
        k = 0
        for sentence in split_sentences(ch["text"]):
            n = len(sentence.split())
            if n:
                sentences.append({"text": sentence, "start": part[k]["start"], "end": part[k + n - 1]["end"]})
            k += n
        words += part
    return {"speed": speed, "duration": gen.get("duration"), "estimated": estimated, "sentences": sentences, "words": words}


def _ts(t: float, sep: str) -> str:
    ms = int(round(t * 1000))
    h, ms = divmod(ms, 3_600_000)
    m, ms = divmod(ms, 60_000)
    s, ms = divmod(ms, 1000)
    return f"{h:02d}:{m:02d}:{s:02d}{sep}{ms:03d}"


def _wrap(text: str) -> str:
    if len(text) <= 42:
        return text
    words, mid = text.split(), len(text) / 2
    if len(words) < 2:  # no spaces to break at (CJK, one long token): split the characters
        cut = len(text) // 2
        return text[:cut] + "\n" + text[cut:]
    best, pos = 0, 0
    for i, w in enumerate(words[:-1]):
        pos += len(w) + 1
        if abs(pos - mid) < abs(best - mid) or best == 0:
            best, cut = pos, i + 1
    return " ".join(words[:cut]) + "\n" + " ".join(words[cut:])


def attachment_header(name: str, ext: str) -> str:
    """Content-Disposition for a download. Non-Latin titles are sent as UTF-8 (filename*), with
    an ASCII fallback; a raw Unicode name cannot be encoded in HTTP headers."""
    from urllib.parse import quote

    full = f"{name}.{ext}"
    base = re.sub(r'["\\\r\n]', "_", name.encode("ascii", "ignore").decode()).strip() or "download"
    ascii_name = f"{base}.{ext}"
    return f"attachment; filename=\"{ascii_name}\"; filename*=UTF-8''{quote(full, safe='')}"


def to_srt(caps: list[dict]) -> str:
    return "\n".join(f"{i}\n{_ts(c['start'], ',')} --> {_ts(c['end'], ',')}\n{_wrap(c['text'])}\n" for i, c in enumerate(caps, 1))


def to_vtt(caps: list[dict]) -> str:
    return "WEBVTT\n\n" + "\n".join(f"{_ts(c['start'], '.')} --> {_ts(c['end'], '.')}\n{_wrap(c['text'])}\n" for c in caps)


def to_txt(caps: list[dict]) -> str:
    return " ".join(c["text"] for c in caps)


# ------------------------------------------------------------------ speech to text


def transcript_doc(tid: str) -> dict | None:
    f = TRANSCRIPTS_DIR / tid / "doc.json"
    return json.loads(f.read_text()) if f.exists() else None


def save_transcript_doc(doc: dict) -> None:
    (TRANSCRIPTS_DIR / doc["id"] / "doc.json").write_text(json.dumps(doc))


def list_transcripts() -> list[dict]:
    out = []
    for d in TRANSCRIPTS_DIR.iterdir():
        doc = transcript_doc(d.name) if d.is_dir() else None
        if doc:
            out.append({k: v for k, v in doc.items() if k not in ("segments", "captions")})
    return sorted(out, key=lambda x: -x["created"])


def run_transcription(tid: str, language: str | None, progress) -> None:
    doc = transcript_doc(tid)
    doc["status"] = "running"
    save_transcript_doc(doc)
    try:
        d = TRANSCRIPTS_DIR / tid
        progress(0.1, "Decoding")
        au.decode_to_wav(d / doc["file"], d / "audio.wav", 16000)
        a, sr = au.load(d / "audio.wav")
        progress(0.25, "Transcribing (Whisper large-v3-turbo)")
        tr = ml.transcribe(a, sr, language=language or None, words=True)
        caps = []
        for seg in tr["segments"]:
            words = seg["words"]
            buf = []
            for w in words:
                buf.append(w)
                txt = "".join(x["word"] for x in buf).strip()
                if len(txt) >= MAX_CAPTION_CHARS * 0.8 or txt.endswith((".", "?", "!")):
                    caps.append({"start": buf[0]["start"], "end": buf[-1]["end"], "text": txt})
                    buf = []
            if buf:
                caps.append({"start": buf[0]["start"], "end": buf[-1]["end"], "text": "".join(x["word"] for x in buf).strip()})
        doc.update(status="done", text=tr["text"], language=tr.get("language"), duration=round(len(a) / sr, 2),
                   segments=[{k: v for k, v in s.items() if k != "words"} for s in tr["segments"]], captions=caps,
                   words=sum(len(s["words"]) for s in tr["segments"]))
    except Exception as e:
        doc.update(status="error", error=str(e))
        save_transcript_doc(doc)
        raise
    save_transcript_doc(doc)


def new_transcript(filename: str) -> dict:
    tid = uuid.uuid4().hex[:12]
    (TRANSCRIPTS_DIR / tid).mkdir(parents=True)
    ext = Path(filename).suffix.lower() or ".wav"
    doc = {"id": tid, "name": Path(filename).stem, "file": f"original{ext}", "status": "queued", "created": time.time()}
    save_transcript_doc(doc)
    return doc


# ------------------------------------------------------------------ voice design

DEFAULT_DESIGN_TEXT = (
    "The old lighthouse keeper climbed the stairs one last time. Below him, the sea was calm and silver "
    "in the early light. He had watched these waters for forty years, and tonight, for the first time, "
    "someone else would light the lamp."
)


def design_doc(did: str) -> dict | None:
    f = DESIGNS_DIR / did / "doc.json"
    return json.loads(f.read_text()) if f.exists() else None


def save_design_doc(doc: dict) -> None:
    (DESIGNS_DIR / doc["id"] / "doc.json").write_text(json.dumps(doc))


def new_design(description: str, text: str | None, language: str, count: int) -> dict:
    did = uuid.uuid4().hex[:12]
    (DESIGNS_DIR / did).mkdir(parents=True)
    doc = {"id": did, "description": description.strip(), "text": (text or DEFAULT_DESIGN_TEXT).strip(),
           "language": language, "count": max(1, min(4, count)), "status": "queued", "previews": [], "created": time.time()}
    save_design_doc(doc)
    return doc


def run_design(did: str, progress, cancel) -> None:
    import mlx.core as mx

    doc = design_doc(did)
    doc["status"] = "running"
    save_design_doc(doc)
    try:
        spec = engines.DESIGN_ENGINE
        if not engines.is_downloaded(spec.repo):
            progress(0.02, f"Downloading voice designer ({spec.size_gb} GB, first time only)")
            engines.download(spec.repo)
        progress(0.1, "Loading voice designer")
        model = engines.load(spec.id)
        for n in range(doc["count"]):
            if cancel():
                break
            progress(0.15 + 0.8 * n / doc["count"], f"Designing voice {n + 1} of {doc['count']}")
            mx.random.seed(int(time.time()) % 100000 + n * 7919)
            pieces = [np.array(r.audio, dtype=np.float32).reshape(-1) for r in model.generate(
                text=doc["text"], instruct=doc["description"], lang_code=engines.LANG_NAMES.get(doc["language"], "auto"),
                temperature=0.8, max_tokens=int(12.5 * 40))]
            a = np.concatenate(pieces) if pieces else np.zeros(0, np.float32)
            a = au.trim_edges(a, int(model.sample_rate), keep_ms=80)
            a = au.normalize_lufs(a, int(model.sample_rate), -18.0)
            fname = f"preview_{n}.wav"
            au.save(DESIGNS_DIR / did / fname, a, int(model.sample_rate))
            doc["previews"].append({"index": n, "file": fname, "duration": round(len(a) / model.sample_rate, 2)})
            save_design_doc(doc)
        doc["status"] = "done"
    except Exception as e:
        doc.update(status="error", error=str(e))
        save_design_doc(doc)
        raise
    save_design_doc(doc)
    # free the designer so cloning engines have memory again
    engines.unload()
