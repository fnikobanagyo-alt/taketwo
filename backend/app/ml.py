"""Speech recognition (Whisper) and speaker fingerprints (Qwen3 ECAPA encoder).

All MLX work runs on the single worker thread (see worker.py), so no locking here.
"""
from __future__ import annotations

import warnings

import numpy as np

from . import worker

from . import audio as au
from .config import STT_PROCESSOR_REPO, STT_REPO, VOICE_ENCODER_REPO

_stt = None
_normalizer = None
_spk = None


def stt_model():
    global _stt
    if _stt is None:
        from mlx_audio.stt.utils import load_model

        from . import engines

        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            _stt = load_model(str(engines.ensure_local(STT_REPO)))
        if getattr(_stt, "_processor", None) is None:
            from transformers import WhisperProcessor

            try:
                _stt._processor = WhisperProcessor.from_pretrained(STT_PROCESSOR_REPO, local_files_only=True)
            except Exception:
                _stt._processor = WhisperProcessor.from_pretrained(STT_PROCESSOR_REPO)
    return _stt


def unload() -> None:
    """Free Whisper and the voice encoder; they reload on next use."""
    global _stt, _spk
    _stt = _spk = None


def normalize_for_wer(text: str) -> str:
    global _normalizer
    if _normalizer is None:
        _normalizer = stt_model()._processor.tokenizer.normalize
    return _normalizer(text)


@worker.phase("transcription")
def transcribe(a: np.ndarray, sr: int, language: str | None = "en", words: bool = False) -> dict:
    """Returns {"text", "segments": [{start,end,text,avg_logprob,no_speech_prob,words:[{word,start,end,probability}]}]}"""
    x = au.resample(a, sr, 16000)
    r = stt_model().generate(
        x,
        language=language or None,
        word_timestamps=words,
        condition_on_previous_text=False,  # prevents hallucinated carry-over on long refs
        verbose=False,
    )
    segs = []
    for s in r.segments or []:
        segs.append(
            {
                "start": float(s["start"]),
                "end": float(s["end"]),
                "text": s["text"].strip(),
                "avg_logprob": float(s.get("avg_logprob", 0)),
                "no_speech_prob": float(s.get("no_speech_prob", 0)),
                "words": [
                    {"word": w["word"], "start": float(w["start"]), "end": float(w["end"]), "p": float(w.get("probability", 1))}
                    for w in (s.get("words") or [])
                ],
            }
        )
    return {"text": (r.text or "").strip(), "segments": segs, "language": getattr(r, "language", language)}


def wer(reference: str, hypothesis: str) -> float:
    """Word error rate, forgiving of spelling/format variants that sound identical.

    Whisper's normalizer drops (parenthesised) text, so brackets become plain words;
    and a spacing-insensitive character error rate catches "hide-working" vs
    "hideworking" and similar, which are not real mistakes.
    """
    import re

    import jiwer

    unbracket = lambda t: re.sub(r"[()\[\]{}]", " ", t)
    ref = normalize_for_wer(unbracket(reference))
    hyp = normalize_for_wer(unbracket(hypothesis))
    if not ref.strip():
        return 0.0 if not hyp.strip() else 1.0
    if not hyp.strip():
        return 1.0
    w = float(jiwer.wer(ref, hyp))
    c = float(jiwer.cer(ref.replace(" ", ""), hyp.replace(" ", "")))
    return min(w, c * 2.0)


# ------------------------------------------------------------ speaker encoder
# Resemble's GE2E voice encoder (5.7 MB, trained for speaker verification). It separates
# different speakers far better than TTS-internal x-vectors, which is what we need to
# detect a take that drifted away from the cloned voice.


def _voice_encoder():
    global _spk
    if _spk is None:
        import mlx.core as mx
        from huggingface_hub import hf_hub_download
        from mlx_audio.tts.models.chatterbox.voice_encoder.voice_encoder import VoiceEncoder

        ve = VoiceEncoder()
        try:
            f = hf_hub_download(VOICE_ENCODER_REPO, "ve.safetensors", local_files_only=True)
        except Exception:
            f = hf_hub_download(VOICE_ENCODER_REPO, "ve.safetensors")
        w = ve.sanitize(mx.load(f))
        ve.load_weights(list(w.items()))
        mx.eval(ve.parameters())
        _spk = ve
    return _spk


def speaker_embedding(a: np.ndarray, sr: int) -> np.ndarray:
    import mlx.core as mx

    e = np.array(_voice_encoder().embeds_from_wavs([mx.array(a)], sr, as_spk=True)).reshape(-1)
    return (e / (np.linalg.norm(e) + 1e-9)).astype(np.float32)


def cosine(a: np.ndarray, b: np.ndarray) -> float:
    return float(np.dot(a, b) / (np.linalg.norm(a) * np.linalg.norm(b) + 1e-9))

