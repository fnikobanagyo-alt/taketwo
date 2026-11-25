"""Script cleanup and sentence-aware chunking."""
from __future__ import annotations

import html
import re
from dataclasses import dataclass

ABBREVIATIONS = {
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e",
    "no", "vol", "fig", "approx", "dept", "est", "inc", "ltd", "co", "mt", "ft",
}


_BARE_NUMERIC_ENTITY = re.compile(r"&(\d{2,5});")  # "&39;": an apostrophe pasted from a web page


def fix_entities(text: str) -> str:
    """Undo web/HTML artifacts in a pasted script: &39; &amp; &quot; &nbsp; ...
    Left in place, the voice reads them aloud ("Marika and 39's")."""
    t = _BARE_NUMERIC_ENTITY.sub(lambda m: chr(int(m.group(1))) if 32 <= int(m.group(1)) < 0x2000 else m.group(0), text or "")
    return html.unescape(t).replace("\u00a0", " ")


def clean_text(text: str, pronunciations: list[dict] | None = None, strip_citations: bool = True) -> str:
    t = fix_entities(text).replace("\r\n", "\n").replace("\r", "\n")
    if strip_citations:
        t = re.sub(r"\s*\[\d+(?:[,\-–]\s*\d+)*\]", "", t)  # [1], [2,3], [4-6]
    t = t.replace("’", "'").replace("‘", "'").replace("“", '"').replace("”", '"')
    t = re.sub(r"\s*[—–]\s*", ", ", t)  # em/en dash -> comma pause
    t = t.replace("…", "...")
    t = re.sub(r"[*_#`>|~]", "", t)  # markdown artifacts
    t = re.sub(r"https?://\S+", "", t)
    for p in pronunciations or []:
        src, dst = (p.get("from") or "").strip(), (p.get("to") or "").strip()
        if src and dst:
            t = re.sub(rf"(?<!\w){re.escape(src)}(?!\w)", dst, t, flags=re.IGNORECASE)
    t = re.sub(r"[ \t]+", " ", t)
    t = re.sub(r" ?\n ?", "\n", t)
    t = unwrap_lines(t)
    t = re.sub(r"\n{3,}", "\n\n", t)
    t = re.sub(r",\s*,", ",", t)
    t = re.sub(r"(^|\n),\s*", r"\1", t)
    return t.strip()


_TERMINAL = (".", "!", "?", ":", ";", '"', "'", ")", "]")


def unwrap_lines(t: str) -> str:
    """Rejoin lines that were hard-wrapped mid-sentence (text copied from PDFs, Docs,
    emails). A single line break only counts as a paragraph when the line actually
    ends a sentence; otherwise it's a wrap and becomes a space. Blank lines always stay
    paragraphs, and short title-like lines (headings) keep their own line."""
    out: list[str] = []
    for block in re.split(r"\n\s*\n", t):
        lines = [ln.strip() for ln in block.split("\n") if ln.strip()]
        merged = ""
        for ln in lines:
            if not merged:
                merged = ln
                continue
            prev = merged.split("\n")[-1]
            ends_sentence = prev.rstrip().endswith(_TERMINAL)
            heading = len(prev) < 50 and ln[:1].isupper() and not prev.rstrip().endswith((",", "-"))
            if ends_sentence or heading:
                merged += "\n" + ln
            elif prev.endswith("-") and ln[:1].islower():
                merged = merged[:-1] + ln  # "long-" + "house" -> "longhouse"
            elif prev.endswith("-"):
                merged += ln  # "well-" + "Known" -> "well-Known"
            else:
                merged += " " + ln
        out.append(merged)
    return "\n\n".join(o for o in out if o)


def split_sentences(paragraph: str) -> list[str]:
    parts = re.split(r'(?<=[.!?])["\')\]]?\s+(?=["\'(\[]?[A-Za-z0-9])', paragraph)
    # Re-join false splits after abbreviations / initials.
    out: list[str] = []
    for p in parts:
        if out:
            last_word = re.findall(r"(\b[\w.]+)\.$", out[-1])
            if last_word and (last_word[0].lower() in ABBREVIATIONS or re.fullmatch(r"[A-Z]", last_word[0])):
                out[-1] = out[-1] + " " + p
                continue
        out.append(p.strip())
    return [s for s in out if s]


_SOFT_BREAKS = frozenset(",;:.!?\u3001\u3002\uff01\uff0c\uff1b\uff1f")  # incl. CJK punctuation
_CJK = re.compile("[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uac00-\ud7af\uf900-\ufaff]")


def _hard_split(token: str, max_chars: int) -> list[str]:
    """Cut a run with no spaces (CJK text, a long URL or code token) into pieces of at most
    max_chars, preferring a break right after punctuation in the second half of the window."""
    out = []
    while len(token) > max_chars:
        window = token[:max_chars]
        cut = next((j for j in range(len(window), max_chars // 2, -1) if window[j - 1] in _SOFT_BREAKS), max_chars)
        out.append(token[:cut])
        token = token[cut:]
    if token:
        out.append(token)
    return out


def _split_long(sentence: str, max_chars: int) -> list[str]:
    """Split an over-long sentence at clause punctuation, then at spaces, then by characters."""
    if len(sentence) <= max_chars:
        return [sentence]
    pieces = re.split(r"(?<=[,;:])\s+", sentence)
    out, cur = [], ""
    for p in pieces:
        if len(p) > max_chars:
            # Flush earlier clauses before emitting pieces of the next clause.
            if cur:
                out.append(cur)
                cur = ""
            words, buf = p.split(), ""
            for w in words:
                if len(w) > max_chars:  # no spaces to break at: split the run itself
                    if buf:
                        out.append(buf)
                    *whole, buf = _hard_split(w, max_chars)
                    out.extend(whole)
                    continue
                if len(buf) + len(w) + 1 > max_chars and buf:
                    out.append(buf)
                    buf = w
                else:
                    buf = f"{buf} {w}".strip()
            p = buf
        if cur and len(cur) + len(p) + 1 > max_chars:
            out.append(cur)
            cur = p
        else:
            cur = f"{cur} {p}".strip()
    if cur:
        out.append(cur)
    return out


@dataclass
class Chunk:
    text: str
    pause_after: str  # "sentence" | "paragraph" | "end"


def chunk_text(text: str, max_chars: int = 220) -> list[Chunk]:
    """Group whole sentences into chunks of <= max_chars, never crossing paragraphs.

    Short chunks are the single biggest reliability win for autoregressive TTS:
    long prompts are where models drift into silence, babble or loops.
    """
    chunks: list[Chunk] = []
    paragraphs = [p.strip() for p in re.split(r"\n\s*\n|\n", text) if p.strip()]
    for pi, para in enumerate(paragraphs):
        sentences: list[str] = []
        for s in split_sentences(para):
            sentences.extend(_split_long(s, max_chars))
        cur = ""
        para_chunks = []
        for s in sentences:
            if cur and len(cur) + len(s) + 1 > max_chars:
                para_chunks.append(cur)
                cur = s
            else:
                cur = f"{cur} {s}".strip()
        if cur:
            para_chunks.append(cur)
        # Avoid tiny orphan chunks (models handle 1-2 word prompts badly).
        if len(para_chunks) > 1 and len(para_chunks[-1]) < 25 and len(para_chunks[-2]) + len(para_chunks[-1]) < max_chars * 1.3:
            orphan = para_chunks.pop()
            para_chunks[-1] = f"{para_chunks[-1]} {orphan}"
        for ci, c in enumerate(para_chunks):
            last_in_para = ci == len(para_chunks) - 1
            kind = "end" if (last_in_para and pi == len(paragraphs) - 1) else ("paragraph" if last_in_para else "sentence")
            chunks.append(Chunk(c, kind))
    return chunks


def word_count(text: str) -> int:
    """Spoken-word estimate. Each CJK/kana/Hangul character is a syllable, so a run of them
    is many words, not one (a 200-character Chinese sentence has no spaces)."""
    return len(re.findall(r"[\w']+", _CJK.sub(" ", text))) + len(_CJK.findall(text))
