"""Record mlx-audio's Whisper word grouping on canned inputs (step S13, gate G1).

    HF_HUB_OFFLINE=1 backend/.venv/bin/python -B rust/golden/record_whisper_words.py

Writes rust/golden/whisper_words.json. Runs mlx-audio's own add_word_timestamps, find_alignment,
merge_punctuations and HFTokenizerWrapper.split_to_word_tokens with the local
openai/whisper-large-v3-turbo tokenizer files. No model is loaded: find_alignment's model call is
a fake that returns canned token probabilities, and timing.dtw returns a canned path, so the jump
times are the canned ones. Each case stores the token ids and raw bytes (decoded from vocab.json as
the ggml converter does), so the Rust test needs no tokenizer files.
"""
from __future__ import annotations

import copy
import json
import math
import os
from pathlib import Path

import numpy as np

assert os.environ.get("HF_HUB_OFFLINE") == "1", "run with HF_HUB_OFFLINE=1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"

import mlx.core as mx  # noqa: E402
from huggingface_hub import snapshot_download  # noqa: E402
from mlx_audio.stt.models.whisper import timing  # noqa: E402
from mlx_audio.stt.models.whisper.whisper import HFTokenizerWrapper  # noqa: E402
from transformers import WhisperProcessor  # noqa: E402

HERE = Path(__file__).resolve().parent
OUT = HERE / "whisper_words.json"
TOK_REPO = "openai/whisper-large-v3-turbo"
N_VOCAB = 51866


def bytes_to_unicode():  # as convert-pt-to-ggml.py / prototypes/scripts/convert_whisper_ggml.py
    bs = list(range(ord("!"), ord("~") + 1)) + list(range(ord("¡"), ord("¬") + 1)) + list(range(ord("®"), ord("ÿ") + 1))
    cs = bs[:]
    n = 0
    for b in range(2**8):
        if b not in bs:
            bs.append(b)
            cs.append(2**8 + n)
            n += 1
    return dict(zip(bs, [chr(c) for c in cs]))


TOK_DIR = Path(snapshot_download(TOK_REPO, local_files_only=True))
BYTE_DECODER = {v: k for k, v in bytes_to_unicode().items()}
ID_TO_BYTES = {
    int(i): bytes(BYTE_DECODER[c] for c in t)
    for t, i in json.loads((TOK_DIR / "vocab.json").read_text(encoding="utf8")).items()
    if t != "<|endoftext|>"
}
PROCESSOR = WhisperProcessor.from_pretrained(TOK_REPO, local_files_only=True)


class FakeModel:
    """Stands in for the Whisper model inside find_alignment: canned logits, flat cross attention."""

    def __init__(self, probs: list[float]):
        self.probs = probs
        self.alignment_heads = mx.array([[0, 0]])

    def forward_with_cross_qk(self, mel, tokens):
        n_tok = tokens.shape[1]
        logits = np.zeros((1, n_tok, N_VOCAB), np.float32)
        toks = tokens.tolist()[0]
        eot = toks[-1]
        for k, p in enumerate(self.probs):  # row 3 + k predicts text token k (sot sequence is 3 long)
            logits[0, 3 + k, toks[4 + k]] = math.log(p * (eot - 1) / (1 - p))
        cross_qk = [mx.zeros((1, 1, n_tok, 1500))]
        return mx.array(logits), cross_qk


def canned_dtw(jumps: list[int]):
    """A DTW path whose row k starts at audio token jumps[k] (rows = text tokens + eot)."""
    rows, cols = [], []
    for k, j0 in enumerate(jumps):
        j1 = jumps[k + 1] - 1 if k + 1 < len(jumps) else j0 + 2
        for j in range(j0, max(j0, j1) + 1):
            rows.append(k)
            cols.append(j)
    return lambda x: (np.array(rows), np.array(cols))


def case(name, language, segments, jumps, probs, seek=0, last_speech=0.0, changed=None, split_char=False):
    """segments: [(start, end, text)]; jumps: audio-token index (20 ms) where each token starts, plus eot.
    changed=(word, key, raw): the rule under test must move that word's time away from its raw value.
    split_char: some token must not decode alone as UTF-8 (a character split across tokens)."""
    tok = HFTokenizerWrapper(PROCESSOR.tokenizer, multilingual=True, num_languages=100, language=language)
    segs, all_text = [], []
    for start, end, text in segments:
        # a list is a token sequence given piece by piece (each piece must be one token)
        ids = [one for piece in text for one in tok.encode(piece)] if isinstance(text, list) else tok.encode(text)
        assert not isinstance(text, list) or len(ids) == len(text), (name, ids)
        all_text += ids
        ts0 = tok.timestamp_begin + round((start - seek * 0.01) / 0.02)
        ts1 = tok.timestamp_begin + round((end - seek * 0.01) / 0.02)
        segs.append({"seek": seek, "start": start, "end": end, "tokens": [ts0, *ids, ts1]})
    n = len(all_text)
    if jumps == "even":
        jumps = [5 + 15 * k for k in range(n + 1)]
    if probs == "flat":
        probs = [0.9] * n
    assert len(jumps) == n + 1 and len(probs) == n, (name, n, len(jumps), len(probs))
    inputs = copy.deepcopy(segs)
    timing.dtw = canned_dtw(jumps)
    num_frames = 2 * (jumps[-1] + 3)
    timing.add_word_timestamps(
        segments=segs, model=FakeModel(probs), tokenizer=tok, mel=mx.zeros((3000, 128)),
        num_frames=num_frames, last_speech_timestamp=last_speech,
    )
    out_words = {w["word"]: w for sg in segs for w in sg["words"]}
    if changed:
        word, key, raw = changed
        assert abs(out_words[word][key] - raw) > 1e-9, (name, word, key, out_words[word][key], raw)
    if split_char:
        def bad(b):
            try:
                b.decode("utf-8")
                return False
            except UnicodeDecodeError:
                return True
        assert any(bad(ID_TO_BYTES[i]) for i in all_text), name
    return {
        "name": name,
        "language": language,
        "eot": tok.eot,
        "tokens": [{"id": i, "bytes": list(ID_TO_BYTES[i])} for i in all_text],
        "jump_times": [j / timing.TOKENS_PER_SECOND for j in jumps],
        "token_probs": probs,
        "time_offset": seek * timing.HOP_LENGTH / timing.SAMPLE_RATE,
        "last_speech_timestamp": last_speech,
        "segments_in": [{"start": s["start"], "end": s["end"], "n_text_tokens": len([t for t in s["tokens"] if t < tok.eot])} for s in inputs],
        "segments_out": [{"start": s["start"], "end": s["end"], "words": s["words"]} for s in segs],
    }


def n_tokens(text, language="en"):
    tok = HFTokenizerWrapper(PROCESSOR.tokenizer, multilingual=True, num_languages=100, language=language)
    return len(tok.encode(text))


def main() -> None:
    cases = [
        case("plain sentence", "en", [(0.0, 2.0, " Hello there, my friend.")], "even", "flat"),
        case("leading and trailing punctuation", "en", [(0.0, 3.0, ' "Quoted" words (in brackets) end here!')], "even", "flat"),
        case("hyphenated word", "en", [(0.0, 2.5, " A well-known, long-standing fact.")], "even", "flat"),
        case("numbers", "en", [(0.0, 3.0, " It cost 1,250 dollars in 2024, about 3.5%.")], "even", "flat"),
        case("contractions", "en", [(0.0, 2.5, " Don't stop, it's fine, we'll see.")], "even", "flat"),
        case("space before punctuation", "en", [(0.0, 3.0, ' He said " yes " and - then - left .')], "even", "flat"),
        case("multi-byte characters split across tokens", "en", [(0.0, 3.0, " Café naïve 🎉 party ünd Ωmega.")], "even", "flat", split_char=True),
        case("no-space language (ja)", "ja", [(0.0, 3.0, "こんにちは、世界。今日はいい天気です。")], "even", "flat"),
        case("no-space language (zh)", "zh", [(0.0, 3.0, "你好，世界。我们走吧！")], "even", "flat"),
    ]

    # over-long word right after a sentence end: alignment start is pulled to end - max_duration
    t = " Okay. Wonderful things happen here."
    n = n_tokens(t)
    j = [0, 10, 20, 100] + [110 + 10 * k for k in range(n - 3)]
    cases.append(case("over-long word at a sentence start", "en", [(0.0, 3.0, t)], j[: n + 1], "flat",
                      changed=(" Wonderful", "start", 0.4)))

    # over-long sentence-end mark: its end is cut to start + max_duration
    # (a lone "." merges into the word before it and its time is dropped; after a " " token it stays
    # a word of its own, " .", so the cut is visible: tokens " We", " stop", " here", " ", ".", " Then", " go", ".")
    t = [" We", " stop", " here", " ", ".", " Then", " go", "."]
    j = [0, 10, 20, 30, 40, 100, 110, 120, 130]
    cases.append(case("over-long sentence end mark", "en", [(0.0, 3.0, t)], j, "flat",
                      changed=(" .", "end", 2.0)))

    # pause before the last word, which runs long past the segment end
    t = " I waited and then finally"
    n = n_tokens(t)
    j = [10 * k for k in range(n)] + [10 * (n - 1) + 120]
    cases.append(case("pause before the last word", "en", [(0.0, 1.2, t)], j, "flat",
                      changed=(" finally", "end", 0.2 * (n - 1) + 2.4)))

    # first word after a long pause, longer than twice the median: start pulled in
    t = " Afterwards everything was quiet again."
    n = n_tokens(t)
    j = [0, 60] + [60 + 10 * k for k in range(1, n)]
    cases.append(case("long first word after a pause", "en", [(4.0, 8.0, t)], j[: n + 1], "flat", seek=300, last_speech=0.5,
                      changed=(" Afterwards", "start", 3.0)))

    # first and second word both long after a pause: they share a boundary
    t = " Hello everybody out there."
    n = n_tokens(t)
    j = [0, 80, 160] + [160 + 10 * k for k in range(1, n - 1)]
    cases.append(case("two long words after a pause", "en", [(0.0, 4.0, t)], j[: n + 1], "flat",
                      changed=(" everybody", "start", 1.6)))

    # the segment start is much later than the first word's DTW start
    t = " Late start of a segment."
    n = n_tokens(t)
    j = [0] + [100 + 10 * k for k in range(n)]
    cases.append(case("segment start preferred", "en", [(1.6, 3.0, t)], j, "flat", last_speech=1.9,
                      changed=(" Late", "start", 0.0)))

    # two segments in one window, varied probabilities
    a, b = " First part is here.", " Second part follows, too."
    na, nb = n_tokens(a), n_tokens(b)
    probs = [round(0.5 + 0.4 * ((k * 37) % 11) / 10, 3) for k in range(na + nb)]
    cases.append(case("two segments in one window", "en", [(0.0, 1.5, a), (1.6, 3.4, b)], "even", probs))

    # tokens at the same audio frame (zero-length words are left out of the median)
    t = " Quick, quick words now."
    n = n_tokens(t)
    j = [0, 10, 10, 10, 20, 30, 40, 50, 60][: n + 1]
    j += [j[-1] + 10 * (k + 1) for k in range(n + 1 - len(j))]
    cases.append(case("zero-length tokens", "en", [(0.0, 2.0, t)], j, "flat"))

    for c in cases:
        words = [w["word"] for s in c["segments_out"] for w in s["words"]]
        print(f"{c['name']}: {len(c['tokens'])} tokens -> {words}")
    OUT.write_text(json.dumps({"cases": cases}, indent=1, ensure_ascii=False) + "\n")
    print(f"wrote {OUT.relative_to(HERE.parent.parent)}: {len(cases)} cases")


if __name__ == "__main__":
    main()
