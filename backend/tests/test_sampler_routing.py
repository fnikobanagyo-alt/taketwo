"""A requested sampler value must reach cloning unchanged; no model/GPU load."""
import os
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

import numpy as np

_data = tempfile.TemporaryDirectory(prefix="vf-sampler-tests-")
os.environ.setdefault("VOICEFORGE_DATA", _data.name)
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from app import engines, qwen_compat


class FakeModel:
    sample_rate = 24000
    _voiceforge_exact_lengths = True

    def __init__(self, model_type="base", encoder=True):
        self.config = types.SimpleNamespace(tts_model_type=model_type)
        self.speech_tokenizer = types.SimpleNamespace(has_encoder=encoder)
        self.calls = []

    def _generate_icl(self, **kwargs):
        self.calls.append(("icl", kwargs))
        yield types.SimpleNamespace(audio=np.ones(100), sample_rate=24000, token_count=5)

    def generate(self, **kwargs):
        self.calls.append(("public", kwargs))
        # Positive defect control: the installed router silently clamps this.
        if qwen_compat.uses_icl(self, kwargs.get("ref_audio"), kwargs.get("ref_text")):
            kwargs["repetition_penalty"] = max(kwargs["repetition_penalty"], 1.5)
            yield from self._generate_icl(**kwargs)
        else:
            yield types.SimpleNamespace(audio=np.ones(100), sample_rate=24000, token_count=5)


class SamplerRoutingTests(unittest.TestCase):
    def kwargs(self, penalty=1.05):
        return dict(text="A village.", ref_audio=np.ones(10), ref_text="Reference.",
                    lang_code="english", temperature=.775, top_p=.9, top_k=40,
                    max_tokens=211, stream=False, split_pattern="", repetition_penalty=penalty)

    def test_public_control_clamps_but_adapter_honors_lower_value(self):
        model = FakeModel()
        list(model.generate(**self.kwargs()))
        self.assertEqual(model.calls[-1][1]["repetition_penalty"], 1.5)
        model.calls.clear()
        list(qwen_compat.generate(model, **self.kwargs()))
        self.assertEqual(len(model.calls), 1)
        route, kw = model.calls[0]
        self.assertEqual(route, "icl")
        self.assertEqual(kw["repetition_penalty"], 1.05)
        self.assertEqual(kw["language"], "english")
        self.assertEqual(kw["max_tokens"], 211)

    def test_explicit_stronger_penalty_is_not_replaced(self):
        model = FakeModel()
        list(qwen_compat.generate(model, **self.kwargs(1.75)))
        self.assertEqual(model.calls[0][1]["repetition_penalty"], 1.75)

    def test_encoderless_and_nonbase_models_use_public_router(self):
        for model in [FakeModel(encoder=False), FakeModel("custom_voice"), FakeModel("voice_design")]:
            list(qwen_compat.generate(model, **self.kwargs()))
            self.assertEqual([c[0] for c in model.calls], ["public"])

    def test_absent_reference_uses_public_router(self):
        for missing in ("ref_audio", "ref_text"):
            model = FakeModel()
            kw = self.kwargs(); kw[missing] = None
            list(qwen_compat.generate(model, **kw))
            self.assertEqual(model.calls[0][0], "public")

    def test_uninstalled_clone_adapter_fails_before_any_audio(self):
        model = FakeModel(); model._voiceforge_exact_lengths = False
        with self.assertRaisesRegex(RuntimeError, "not installed"):
            list(qwen_compat.generate(model, **self.kwargs()))
        self.assertEqual(model.calls, [])

    def test_engine_routes_requested_value_and_records_actual_sampler(self):
        model = FakeModel()
        mx = types.ModuleType("mlx.core")
        mx.random = types.SimpleNamespace(seed=lambda seed: None)
        utils = types.ModuleType("mlx_audio.utils")
        utils.load_audio = lambda *a, **k: np.ones(10)
        modules = {"mlx": types.ModuleType("mlx"), "mlx.core": mx,
                   "mlx_audio": types.ModuleType("mlx_audio"), "mlx_audio.utils": utils}
        with patch.dict(sys.modules, modules), patch.object(engines, "load", return_value=model):
            a, sr = engines.synthesize("qwen3-tts-1.7b", "A village.", Path("ref.wav"),
                                      "Reference.", "en", {"repetition_penalty": 1.05}, 12, 3.)
        self.assertEqual(model.calls[0][1]["repetition_penalty"], 1.05)
        report = engines.synthesis_report()
        self.assertEqual(report["sampler"]["repetition_penalty"], 1.05)
        self.assertEqual(report["generation_route"], "icl_explicit_penalty")
        self.assertEqual(report["tokens"], 5)
        self.assertEqual((len(a), sr), (100, 24000))

    def test_default_uses_user_accepted_plain_take_penalty(self):
        defaults = {p.key: p.default for p in engines.QWEN_PARAMS}
        self.assertEqual(defaults["repetition_penalty"], 1.05)


if __name__ == "__main__":
    unittest.main()
