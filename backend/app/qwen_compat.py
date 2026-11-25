"""Project-owned corrections for mlx-audio Qwen's unpadded cloning path.

Codec ID zero is speech data, not a length/padding marker. The installed 0.5.7
decoder counts nonzero codes, and ICL removes the reference proportionally. Keep
the library's waveform decoder and generation loop, but use explicit frame counts
and the exact reference sample boundary. No installed package files are changed.

The small AST substitution is deliberately guarded: a different upstream ICL
implementation must be reviewed rather than silently applying a stale patch.
"""
from __future__ import annotations

import ast
import inspect
import textwrap
from types import MethodType


def uses_icl(model, ref_audio, ref_text) -> bool:
    """Match the installed Qwen router, including its model-type check."""
    return (getattr(getattr(model, "config", None), "tts_model_type", "base") == "base"
            and ref_audio is not None and ref_text is not None
            and bool(getattr(getattr(model, "speech_tokenizer", None), "has_encoder", False)))


def generate(model, **kwargs):
    """Honor the requested cloning penalty instead of the public router's >=1.5 clamp.

    This is the same ICL loop installed above, with no decoder or sampling-loop
    changes. Keep non-ICL model routing in mlx-audio. VoiceForge's default is 1.05,
    selected after listening to the original sentence without a continuation.
    Higher penalties remain available for references/text that produce loops.
    """
    if not uses_icl(model, kwargs.get("ref_audio"), kwargs.get("ref_text")):
        yield from model.generate(**kwargs)
        return
    if not getattr(model, "_voiceforge_exact_lengths", False):
        raise RuntimeError("Qwen cloning adapter was not installed; refusing an unverified private generation path.")
    yield from model._generate_icl(
        text=kwargs["text"], ref_audio=kwargs["ref_audio"], ref_text=kwargs["ref_text"],
        language=kwargs.get("lang_code", "auto"), temperature=kwargs.get("temperature", .9),
        max_tokens=kwargs.get("max_tokens", 4096), top_k=kwargs.get("top_k", 50),
        top_p=kwargs.get("top_p", 1.), repetition_penalty=kwargs["repetition_penalty"],
        verbose=kwargs.get("verbose", False), stream=kwargs.get("stream", False),
        streaming_interval=kwargs.get("streaming_interval", 2.),
    )


def _exact_reference_generator(original):
    tree = ast.parse(textwrap.dedent(inspect.getsource(original)))
    fn = tree.body[0]
    fn.decorator_list = []
    candidates = [n for n in ast.walk(fn) if isinstance(n, ast.Assign)
                  and any(isinstance(t, ast.Name) and t.id == "cut" for t in n.targets)]
    old = ast.parse("int(ref_len / max(total_len, 1) * audio.shape[0])", mode="eval").body
    exact = ast.parse("ref_len * self.speech_tokenizer.decode_upsample_rate", mode="eval").body
    if len(candidates) != 1:
        raise RuntimeError("Qwen ICL reference boundary changed; review app/qwen_compat.py before generating.")
    node = candidates[0]
    if ast.dump(node.value) not in (ast.dump(old), ast.dump(exact)):
        raise RuntimeError("Unsupported Qwen ICL reference trimming; review app/qwen_compat.py.")
    node.value = exact
    ast.fix_missing_locations(tree)
    namespace = dict(original.__globals__)
    exec(compile(tree, __file__, "exec"), namespace)
    return namespace[fn.name]


def install(model) -> None:
    """Install once on a loaded Qwen model; batch padding uses decode_batch separately."""
    if getattr(model, "_voiceforge_exact_lengths", False):
        return
    tokenizer = model.speech_tokenizer
    if tokenizer is None:
        raise RuntimeError("Qwen speech tokenizer is unavailable.")
    original_decode = tokenizer.decode
    original_generate = model._generate_icl.__func__
    patched_generate = _exact_reference_generator(original_generate)

    def decode_unpadded(self, audio_codes):
        wav, _ = original_decode(audio_codes)
        expected = int(audio_codes.shape[1]) * int(self.decode_upsample_rate)
        if wav.shape[-1] < expected:
            raise RuntimeError(f"Qwen decoded only {wav.shape[-1]} samples; expected {expected}. Refusing truncated audio.")
        # Preserve the native array type. Lengths describe frames, never token values.
        import mlx.core as mx

        lengths = mx.full((audio_codes.shape[0],), expected, dtype=mx.int32)
        self._voiceforge_decode_lengths = {
            "frames": int(audio_codes.shape[1]), "samples": expected,
            "zero_codes": int((audio_codes[..., 0] == 0).sum().item()),
        }
        return wav[..., :expected], lengths

    tokenizer.decode = MethodType(decode_unpadded, tokenizer)
    model._generate_icl = MethodType(patched_generate, model)
    model._voiceforge_exact_lengths = True
