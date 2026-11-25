"""TTS engine registry.

Every engine is a free, open-weights model run locally through mlx-audio (Apple
Silicon / Metal). Adding a newly released model = one entry in BUILTIN_ENGINES,
or add any mlx-audio compatible Hugging Face repo from the Models page at runtime.
"""
from __future__ import annotations

import gc
import os
from dataclasses import asdict, dataclass, field
from pathlib import Path

import numpy as np

from . import worker

LANG_NAMES = {
    "en": "english", "zh": "chinese", "ja": "japanese", "ko": "korean", "de": "german",
    "fr": "french", "es": "spanish", "it": "italian", "pt": "portuguese", "ru": "russian",
}


@dataclass
class Param:
    key: str
    label: str
    default: float
    min: float
    max: float
    step: float
    help: str = ""


@dataclass
class EngineSpec:
    id: str
    name: str
    repo: str
    family: str  # qwen3 | voxcpm2 | chatterbox | fish | omnivoice | generic
    description: str
    license: str
    size_gb: float
    languages: list[str] = field(default_factory=lambda: ["en"])
    needs_ref_text: bool = True
    recommended: bool = False
    params: list[Param] = field(default_factory=list)
    custom: bool = False
    kind: str = "clone"  # clone | design

    def public(self) -> dict:
        d = asdict(self)
        d["downloaded"] = is_downloaded(self.repo)
        return d


QWEN_PARAMS = [
    Param("temperature", "Temperature", 0.7, 0.1, 1.2, 0.05, "Lower = steadier, more consistent delivery. Higher = more expressive but riskier."),
    Param("top_p", "Top-p", 0.9, 0.5, 1.0, 0.01),
    Param("top_k", "Top-k", 40, 5, 100, 1),
    Param("repetition_penalty", "Repetition penalty", 1.05, 1.0, 2.0, 0.05,
          "Lower = less suppression of repeated sound codes. Higher = fewer loops. Changes can affect pronunciation."),
]

BUILTIN_ENGINES: list[EngineSpec] = [
    EngineSpec(
        "qwen3-tts-1.7b", "Qwen3-TTS 1.7B", "mlx-community/Qwen3-TTS-12Hz-1.7B-Base-bf16", "qwen3",
        "Best all-round cloning accuracy and natural narration. Uses reference audio + transcript (in-context cloning).",
        "Apache-2.0", 4.5, ["en", "zh", "ja", "ko", "de", "fr", "es", "it", "pt", "ru"], True, True, QWEN_PARAMS,
    ),
    EngineSpec(
        "voxcpm2", "VoxCPM2 (2B, 48 kHz)", "mlx-community/VoxCPM2-8bit", "voxcpm2",
        "Studio-quality 48 kHz output, very high speaker similarity using reference + continuation cloning. 30 languages.",
        "Apache-2.0", 3.2, ["en", "zh", "ja", "ko", "de", "fr", "es", "it", "pt", "ru"], True, True,
        [
            Param("cfg_value", "Guidance (CFG)", 2.0, 1.0, 4.0, 0.1, "Higher = sticks closer to the reference voice."),
            Param("inference_timesteps", "Diffusion steps", 12, 4, 30, 1, "Higher = cleaner audio, slower."),
        ],
    ),
    EngineSpec(
        "qwen3-tts-0.6b", "Qwen3-TTS 0.6B (fast)", "mlx-community/Qwen3-TTS-12Hz-0.6B-Base-bf16", "qwen3",
        "Smaller, ~2x faster Qwen3. Slightly lower similarity.",
        "Apache-2.0", 2.5, ["en", "zh", "ja", "ko", "de", "fr", "es", "it", "pt", "ru"], True, False, QWEN_PARAMS,
    ),
    EngineSpec(
        "chatterbox", "Chatterbox (English)", "mlx-community/chatterbox-fp16", "chatterbox",
        "Very stable English cloning with an emotion/exaggeration control. Needs no transcript.",
        "MIT", 2.6, ["en"], False, False,
        [
            Param("exaggeration", "Exaggeration", 0.5, 0.0, 1.5, 0.05, "Emotional intensity. 0.5 is neutral narration."),
            Param("cfg_weight", "CFG weight", 0.5, 0.0, 1.0, 0.05, "Lower = slower, more deliberate pacing."),
            Param("temperature", "Temperature", 0.7, 0.1, 1.2, 0.05),
        ],
    ),
    EngineSpec(
        "chatterbox-multilingual", "Chatterbox Multilingual", "mlx-community/chatterbox-multilingual-v3", "chatterbox",
        "Chatterbox for 23 languages.",
        "MIT", 2.7, ["en", "zh", "ja", "ko", "de", "fr", "es", "it", "pt", "ru"], False, False,
        [
            Param("exaggeration", "Exaggeration", 0.5, 0.0, 1.5, 0.05),
            Param("cfg_weight", "CFG weight", 0.5, 0.0, 1.0, 0.05),
            Param("temperature", "Temperature", 0.7, 0.1, 1.2, 0.05),
        ],
    ),
    EngineSpec(
        "fish-s2-pro", "Fish Audio S2 Pro", "mlx-community/fish-audio-s2-pro-8bit", "fish",
        "Top-tier expressive cloning with inline [tags] like [whisper] [sigh]. Free for personal / non-commercial use only.",
        "Fish Audio Research License (non-commercial)", 6.7, ["en", "zh", "ja", "ko", "de", "fr", "es", "it", "pt", "ru"], True, False,
        [
            Param("temperature", "Temperature", 0.7, 0.1, 1.2, 0.05),
            Param("top_p", "Top-p", 0.8, 0.5, 1.0, 0.01),
        ],
    ),
    EngineSpec(
        "omnivoice", "OmniVoice", "mlx-community/OmniVoice-bf16", "omnivoice",
        "Non-autoregressive engine. Slightly closer voice match in our tests, but its length is fixed before it speaks, so it needs more retakes. 600+ languages.",
        "Apache-2.0", 1.6, ["en", "zh", "ja", "ko", "de", "fr", "es", "it", "pt", "ru"], True, False,
        [
            Param("num_steps", "Decoding steps", 32, 8, 64, 1),
            Param("guidance_scale", "Guidance", 2.0, 1.0, 4.0, 0.1),
        ],
    ),
]


DESIGN_ENGINE = EngineSpec(
    "qwen3-voicedesign", "Qwen3-TTS VoiceDesign", "mlx-community/Qwen3-TTS-12Hz-1.7B-VoiceDesign-8bit", "qwen3",
    "Creates a brand-new voice from a text description.", "Apache-2.0", 3.1,
    ["en", "zh", "ja", "ko", "de", "fr", "es", "it", "pt", "ru"], False, False, [], kind="design",
)

_custom: list[EngineSpec] = []


def _family_params(family: str) -> list[Param]:
    """The parameters synthesize() reads for a family, taken from its built-in model."""
    for e in BUILTIN_ENGINES:
        if e.family == family and e.params:
            return [Param(p.key, p.label, p.default, p.min, p.max, p.step, p.help) for p in e.params]
    return [Param("temperature", "Temperature", 0.7, 0.1, 1.2, 0.05)]


def set_custom_engines(items: list[dict]) -> None:
    _custom.clear()
    for it in items:
        family = it.get("family") or guess_family(it["repo"])
        _custom.append(
            EngineSpec(
                id=it["id"], name=it.get("name") or it["repo"].split("/")[-1], repo=it["repo"],
                family=family,
                description=it.get("description") or "User-added model.", license=it.get("license") or "see model card",
                size_gb=float(it.get("size_gb") or 0), needs_ref_text=True, custom=True,
                params=_family_params(family),
            )
        )


def guess_family(repo: str) -> str:
    r = repo.lower()
    for key, fam in (("qwen3-tts", "qwen3"), ("voxcpm2", "voxcpm2"), ("chatterbox", "chatterbox"),
                     ("fish", "fish"), ("omnivoice", "omnivoice")):
        if key in r:
            return fam
    return "generic"


def all_engines() -> list[EngineSpec]:
    return BUILTIN_ENGINES + _custom + [DESIGN_ENGINE]


def get_spec(engine_id: str) -> EngineSpec:
    for e in all_engines():
        if e.id == engine_id:
            return e
    raise KeyError(f"Unknown engine {engine_id}")


# ------------------------------------------------------------------ downloads


def local_path(repo: str, patterns: list[str] | None = None) -> Path | None:
    """Cached snapshot path, resolved without touching the network."""
    from huggingface_hub import snapshot_download
    from mlx_audio.utils import DEFAULT_ALLOW_PATTERNS

    try:
        path = Path(snapshot_download(repo, local_files_only=True, allow_patterns=patterns or DEFAULT_ALLOW_PATTERNS))
    except Exception:
        return None
    if (path / "config.json").exists() and any(path.rglob("*.safetensors")):
        return path
    return None


def is_downloaded(repo: str) -> bool:
    return local_path(repo) is not None


def ensure_local(repo: str) -> Path:
    """Local snapshot path; downloads (with retries) only if missing."""
    p = local_path(repo)
    if p is None:
        download(repo)
        p = local_path(repo)
    if p is None:
        raise RuntimeError(f"Download of {repo} did not complete. Check your internet connection and try again.")
    return p


def repo_cache_bytes(repo: str) -> int:
    from huggingface_hub.constants import HF_HUB_CACHE

    d = Path(HF_HUB_CACHE) / ("models--" + repo.replace("/", "--"))
    if not d.exists():
        return 0
    total = 0
    for root, _, files in os.walk(d):
        for f in files:
            p = Path(root) / f
            if not p.is_symlink():
                total += p.stat().st_size
    return total


def download(repo: str, retries: int = 4) -> None:
    import time

    from huggingface_hub import snapshot_download
    from mlx_audio.utils import DEFAULT_ALLOW_PATTERNS

    for attempt in range(retries):
        try:
            snapshot_download(repo, allow_patterns=DEFAULT_ALLOW_PATTERNS)
            return
        except Exception:
            if attempt == retries - 1:
                raise
            time.sleep(2 + 3 * attempt)  # resumes partial files on retry


# ------------------------------------------------------------------ loading

_loaded: dict = {"id": None, "repo": None, "model": None}


def loaded_model_for_repo(repo: str):
    return _loaded["model"] if _loaded["repo"] == repo else None


def load(engine_id: str):
    spec = get_spec(engine_id)
    if _loaded["id"] == engine_id:
        return _loaded["model"]
    unload()
    from mlx_audio.tts.utils import load_model

    model = load_model(str(ensure_local(spec.repo)))
    if spec.family == "qwen3":
        from .qwen_compat import install

        install(model)
    _loaded.update(id=engine_id, repo=spec.repo, model=model)
    return model


def unload() -> None:
    if _loaded["model"] is not None:
        _loaded.update(id=None, repo=None, model=None)
        gc.collect()
        try:
            import mlx.core as mx

            mx.clear_cache()
        except Exception:
            pass


def _omni_duration(text: str, ref_wav: Path, ref_text: str) -> float:
    """Seconds of speech OmniVoice should produce, from the reference's own pace.

    Uses the model's phonetic-weight estimator with the reference text and its token count
    after the same silence removal the model applies to the clip before encoding it.
    """
    import soundfile as sf
    from mlx_audio.tts.models.omnivoice.duration import RuleDurationEstimator
    from mlx_audio.tts.models.omnivoice.utils import _remove_silence

    a, sr = sf.read(str(ref_wav), dtype="float32")
    if a.ndim > 1:
        a = a.mean(axis=1)
    if sr != 24000:
        from . import audio as au

        a, sr = au.resample(a, sr, 24000), 24000
    spoken = len(_remove_silence(a, sr)) / sr
    est = RuleDurationEstimator().estimate_duration(text, ref_text, max(1, int(spoken * 25)))
    return max(1.2, est / 25)


# ------------------------------------------------------------------ inference


_last_synthesis: dict = {}


def synthesis_report() -> dict:
    return dict(_last_synthesis)


@worker.phase("synthesis")
def synthesize(
    engine_id: str,
    text: str,
    ref_wav: Path,
    ref_text: str,
    language: str,
    params: dict,
    seed: int,
    expected_seconds: float,
) -> tuple[np.ndarray, int]:
    """One synthesis call for one chunk. Returns (mono float32 audio, sample_rate)."""
    import mlx.core as mx
    from mlx_audio.utils import load_audio

    spec = get_spec(engine_id)
    model = load(engine_id)
    sr = int(model.sample_rate)
    mx.random.seed(int(seed))
    np.random.seed(int(seed) % (2**32))
    p = {x.key: x.default for x in spec.params}
    p.update({k: v for k, v in (params or {}).items() if v is not None})
    # Hard cap on output length: a runaway generation (silence / babble / loop)
    # is cut off instead of producing 20+ seconds of garbage.
    max_sec = expected_seconds * 1.8 + 3.0

    fam = spec.family
    if fam == "qwen3":
        kw = dict(
            text=text, ref_audio=load_audio(str(ref_wav), sample_rate=sr), ref_text=ref_text,
            lang_code=LANG_NAMES.get(language, "auto"), temperature=float(p["temperature"]),
            top_p=float(p["top_p"]), top_k=int(p["top_k"]), max_tokens=int(12.5 * max_sec) + 8,
            repetition_penalty=float(p.get("repetition_penalty", 1.05)),
            split_pattern="", stream=False,  # unpadded full takes; we chunk ourselves
        )
    elif fam == "voxcpm2":
        kw = dict(
            text=text, ref_audio=str(ref_wav), prompt_audio=str(ref_wav), prompt_text=ref_text.strip() + " ",
            cfg_value=float(p["cfg_value"]), inference_timesteps=int(p["inference_timesteps"]),
        )
    elif fam == "chatterbox":
        kw = dict(
            text=text, ref_audio=load_audio(str(ref_wav), sample_rate=sr), exaggeration=float(p["exaggeration"]),
            cfg_weight=float(p["cfg_weight"]), temperature=float(p["temperature"]),
            max_new_tokens=int(25 * max_sec) + 10, lang_code=language, verbose=False,
        )
    elif fam == "fish":
        kw = dict(
            text=text, ref_audio=load_audio(str(ref_wav), sample_rate=sr), ref_text=ref_text,
            temperature=float(p["temperature"]), top_p=float(p["top_p"]), chunk_length=10_000,
            max_tokens=int(22 * max_sec) + 10, verbose=False,
        )
    elif fam == "omnivoice":
        # Non-autoregressive: the output length is fixed BEFORE speaking. mlx-audio's default
        # guesses it from a generic 1-second sample, which runs ~1.5x too long for a brisk
        # speaker (slow words, long pauses). Calibrate on this voice's own reference instead.
        kw = dict(
            text=text, ref_audio=str(ref_wav), ref_text=ref_text, language=language,
            num_steps=int(p["num_steps"]), guidance_scale=float(p["guidance_scale"]),
            duration_s=_omni_duration(text, ref_wav, ref_text) * float(p.get("length_scale", 1.0)),
        )
    else:
        kw = dict(text=text, ref_audio=str(ref_wav), ref_text=ref_text)
        if "temperature" in p:
            kw["temperature"] = float(p["temperature"])

    _last_synthesis.clear()
    if fam == "qwen3":
        from . import qwen_compat

        results = qwen_compat.generate(model, **kw)
        route = "icl_explicit_penalty" if qwen_compat.uses_icl(model, kw["ref_audio"], kw["ref_text"]) else "public_generate"
    else:
        results = model.generate(**kw)
    pieces, tokens = [], 0
    for res in results:
        pieces.append(np.array(res.audio, dtype=np.float32).reshape(-1))
        sr = int(getattr(res, "sample_rate", sr) or sr)
        tokens += int(getattr(res, "token_count", 0) or 0)
    audio = np.concatenate(pieces) if pieces else np.zeros(0, dtype=np.float32)
    cap = kw.get("max_tokens", kw.get("max_new_tokens"))
    _last_synthesis.update(tokens=tokens, token_limit=cap, raw_samples=len(audio), sample_rate=sr,
                           stop_reason="length_limit_reached" if cap and tokens >= cap else "ended_before_limit" if cap else "not_reported")
    if fam == "qwen3":
        _last_synthesis["sampler"] = {k: kw[k] for k in ("temperature", "top_p", "top_k", "repetition_penalty")}
        _last_synthesis["generation_route"] = route
        _last_synthesis["decoder"] = dict(getattr(model.speech_tokenizer, "_voiceforge_decode_lengths", {}))
    return audio, sr
