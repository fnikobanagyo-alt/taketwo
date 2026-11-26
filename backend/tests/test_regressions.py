"""Run: backend/.venv/bin/python -B -m unittest discover -s backend/tests -v.

Imports and writes use temporary data, never the user's voices or generations.
No test loads MLX models, downloads files, or contacts a service.
"""
from __future__ import annotations

import atexit
import copy
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

import numpy as np
import soundfile as sf

_data = tempfile.TemporaryDirectory(prefix="voiceforge-regressions-")
atexit.register(_data.cleanup)
os.environ["TAKETWO_DATA"] = _data.name
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from app import audio as au, config, pipeline, qwen_compat, textproc


def setUpModule():
    global _normalizer_patch
    import re
    _normalizer_patch = patch.object(pipeline.ml, "normalize_for_wer", side_effect=lambda t: re.sub(r'[^\w\s]', '', t.lower()).strip())
    _normalizer_patch.start()


def tearDownModule():
    _normalizer_patch.stop()


class FakeTokenizer:
    decode_upsample_rate = 1920

    def decode(self, codes):
        n = codes.shape[1] * self.decode_upsample_rate
        return np.arange(n, dtype=np.float32)[None, :], (codes[..., 0] > 0).sum(axis=1) * self.decode_upsample_rate


class FakeQwen:
    def __init__(self):
        self.speech_tokenizer = FakeTokenizer()

    def _generate_icl(self, ref_codes, gen_codes):
        full_codes = np.concatenate([ref_codes, gen_codes], axis=1)
        ref_len = ref_codes.shape[1]
        total_len = full_codes.shape[1]
        audio, audio_lengths = self.speech_tokenizer.decode(full_codes)
        audio = audio[0]
        valid_len = int(audio_lengths[0])
        audio = audio[:valid_len]
        cut = int(ref_len / max(total_len, 1) * audio.shape[0])
        yield audio[cut:]


class DecoderTests(unittest.TestCase):
    def setUp(self):
        # Exercise the project adapter with the same array API without a GPU.
        self.mx = types.ModuleType("mlx.core")
        self.mx.full, self.mx.int32 = np.full, np.int32
        self.modules = patch.dict(sys.modules, {"mlx.core": self.mx, "mlx": types.ModuleType("mlx")})
        self.modules.start()
        self.addCleanup(self.modules.stop)

    def test_zero_codes_preserve_length_and_exact_reference_boundary(self):
        model = FakeQwen()
        ref = np.ones((1, 7, 16), dtype=np.int32)
        target = np.ones((1, 13, 16), dtype=np.int32)
        ref[0, 2, 0] = target[0, 5, 0] = 0
        old = next(model._generate_icl(ref, target))
        self.assertNotEqual(len(old), 13 * 1920)  # positive defect control
        qwen_compat.install(model)
        new = next(model._generate_icl(ref, target))
        self.assertEqual(len(new), 13 * 1920)
        self.assertEqual(new[0], 7 * 1920)
        self.assertEqual(new[-1], 20 * 1920 - 1)
        self.assertEqual(model.speech_tokenizer._voiceforge_decode_lengths["zero_codes"], 2)

    def test_all_zero_codes_are_data_and_install_is_idempotent(self):
        model = FakeQwen()
        qwen_compat.install(model)
        decode = model.speech_tokenizer.decode
        qwen_compat.install(model)
        self.assertIs(model.speech_tokenizer.decode, decode)
        wav, lengths = decode(np.zeros((1, 4, 16), dtype=np.int32))
        self.assertEqual(int(lengths[0]), 4 * 1920)
        self.assertEqual(wav.shape[-1], 4 * 1920)

    def test_short_decoder_output_is_rejected(self):
        model = FakeQwen()
        model.speech_tokenizer.decode = lambda codes: (np.zeros((1, 3)), np.array([3]))
        qwen_compat.install(model)
        with self.assertRaisesRegex(RuntimeError, "Refusing truncated audio"):
            model.speech_tokenizer.decode(np.ones((1, 4, 16)))

    def test_upstream_reference_change_fails_before_install(self):
        def changed(self):
            cut = 42
            yield cut

        model = FakeQwen()
        model._generate_icl = types.MethodType(changed, model)
        decode = model.speech_tokenizer.decode
        with self.assertRaisesRegex(RuntimeError, "Unsupported"):
            qwen_compat.install(model)
        self.assertEqual(model.speech_tokenizer.decode, decode)


def speech(natural=False):
    sr = 24000
    a = (0.4 * np.sin(2 * np.pi * 200 * np.arange(sr) / sr)).astype(np.float32)
    if natural:
        a[-4800:] *= np.geomspace(1.0, 1e-5, 4800)
    return a, sr


class EndingTests(unittest.TestCase):
    def test_padding_never_repairs_a_hard_stop(self):
        a, sr = speech()
        for pad in (0, int(.15 * sr), int(.8 * sr)):
            ending = au.end_analysis(np.pad(a, (0, pad)), sr, .95)
            self.assertFalse(ending["natural"])

    def test_natural_decay_is_a_positive_pass_control(self):
        a, sr = speech(natural=True)
        self.assertTrue(au.end_analysis(np.pad(a, (0, 3600)), sr, .95)["natural"])

    def test_evaluate_keeps_untouched_evidence_and_does_not_fade_speech_tail(self):
        a, sr = speech()
        tr = {"text": "hello", "segments": [{"words": [{"word": "hello.", "start": 0, "end": 1}]}]}
        with patch.object(pipeline.ml, "transcribe", return_value=tr), patch.object(pipeline.ml, "wer", return_value=0), patch.object(pipeline.ml, "speaker_embedding", return_value=np.ones(1)), patch.object(pipeline.ml, "cosine", return_value=1):
            ev = pipeline.evaluate(a, sr, "hello", np.ones(1), 1, 1, config.DEFAULT_SETTINGS, "en")
        self.assertIn("cut_off", ev["issues"])
        np.testing.assert_array_equal(ev["raw_audio"], a)
        self.assertFalse(ev["ending"]["natural"])
        pad = round(ev["ending"]["head_pad_s"] * sr)
        np.testing.assert_array_equal(ev["audio"][pad+sr-600:pad+sr], a[sr-600:sr])
        self.assertGreaterEqual(len(ev["audio"]), len(a) + int(.12 * sr))


class ChunkTests(unittest.TestCase):
    def test_oversized_clause_preserves_prior_clauses_and_order(self):
        text = "First clause, " + " ".join(f"word{i}" for i in range(20)) + "."
        parts = textproc._split_long(text, 40)
        self.assertEqual(" ".join(parts).split(), text.split())
        self.assertEqual(parts[0], "First clause,")
        self.assertTrue(all(len(p) <= 40 for p in parts))

    def test_sentence_and_paragraph_grouping_preserves_all_words(self):
        for cap in (40, 80, 220):
            text = "Hello there. First clause, " + " ".join(f"word{i}" for i in range(80)) + ".\n\nGoodbye now."
            self.assertEqual(" ".join(c.text for c in textproc.chunk_text(text, cap)).split(), text.split())


class PipelineTests(unittest.TestCase):
    def setUp(self):
        self.gdir = config.GENERATIONS_DIR / self.id().split(".")[-1]
        self.gdir.mkdir()
        self.gen = {"id": self.gdir.name, "engine": "qwen3-tts-1.7b", "language": "en", "params": {},
                    "profile_id": "test", "settings": dict(config.DEFAULT_SETTINGS), "chunks": [
                        {"i": 0, "text": "hello", "pause_after": "end", "status": "pending"}]}

    def test_final_scan_checks_raw_cutoff_despite_fade_and_padding(self):
        a, sr = speech()
        # Stored/final audio deliberately masks the hard stop; untouched evidence does not.
        final = np.pad(au.fade(a, sr, fade_out_ms=100), (3600, 12000))
        sf.write(self.gdir / "raw.wav", a, sr, subtype="FLOAT")
        sf.write(self.gdir / "final.wav", final, sr)
        self.gen["chunks"][0].update(raw_file="raw.wav", issues=[], ending=au.end_analysis(a, sr, .95))
        self.gen["timeline"] = [{"i": 0, "start": .15, "end": 1.65}]
        self.assertEqual(pipeline.final_scan(self.gen), [0])
        self.assertIn("cut_off", self.gen["chunks"][0]["final_issues"])

    def test_final_scan_preserves_legacy_known_failures(self):
        a, sr = speech(natural=True)
        sf.write(self.gdir / "final.wav", np.pad(a, (0, sr)), sr)
        self.gen["chunks"][0]["issues"] = ["cut_off"]
        self.gen["timeline"] = [{"i": 0, "start": 0, "end": 2}]
        self.assertEqual(pipeline.final_scan(self.gen), [0])

    def test_render_saves_raw_float_and_keeps_cutoff_flagged(self):
        a, sr = speech()
        ev = {"audio": np.pad(a, (0, 3600)), "raw_audio": a, "sr": sr, "issues": ["cut_off"], "score": 2.5,
              "wer": 0, "sim": 1, "max_gap": 0, "dur": 1.15, "heard": "hello", "ending": au.end_analysis(a, sr, .95)}
        s = {**config.DEFAULT_SETTINGS, "max_attempts": 1}
        with patch.object(pipeline.engines, "get_spec", return_value=types.SimpleNamespace(family="qwen3")), \
             patch.object(pipeline.delivery, "resolve", return_value=({}, {})), \
             patch.object(pipeline.delivery, "pick_variant", return_value=0), \
             patch.object(pipeline, "profile_assets", return_value=(Path("ref.wav"), "ref", np.ones(1), 1, 1)), \
             patch.object(pipeline, "noise_limit", return_value=.1), patch.object(pipeline, "_ref_pitch", return_value=None), \
             patch.object(pipeline.engines, "synthesize", return_value=(a, sr)), \
             patch.object(pipeline.ending_guard, "synthesis_text", return_value=("hello", False)), \
             patch.object(pipeline, "evaluate", return_value=ev), patch.object(pipeline.au, "normalize_lufs", side_effect=lambda a, *args, **kwargs: a):
            ch = pipeline.render_chunk(self.gen, 0, {}, s)
            self.assertEqual(ch["audio_revision"], 1)
            # Revision changes only after this clip's replacement has been saved.
            ch = pipeline.render_chunk(self.gen, 0, {}, s)
            self.assertEqual(ch["audio_revision"], 2)
        self.assertEqual(ch["status"], "flagged")
        self.assertIn("cut_off", ch["issues"])
        raw, _ = sf.read(self.gdir / ch["raw_file"], dtype="float32")
        np.testing.assert_array_equal(raw, a)
        self.assertEqual(sf.info(self.gdir / ch["raw_file"]).subtype, "FLOAT")
        stored, _ = sf.read(self.gdir / ch["file"], dtype="float32")
        np.testing.assert_allclose(stored[sr-600:sr], a[sr-600:sr], atol=1/32768)

    def test_clean_take_outranks_lower_scoring_failed_take(self):
        a, sr = speech(natural=True)
        def ev(issues, score):
            return {"audio": np.pad(a, (0, 3600)), "raw_audio": a, "sr": sr, "issues": issues, "score": score,
                    "wer": 0, "sim": 1, "max_gap": 0, "dur": 1.15, "heard": "hello", "ending": au.end_analysis(a, sr, .95)}
        with patch.object(pipeline.engines, "get_spec", return_value=types.SimpleNamespace(family="qwen3")), \
             patch.object(pipeline.delivery, "resolve", return_value=({}, {})), \
             patch.object(pipeline.delivery, "pick_variant", return_value=0), \
             patch.object(pipeline, "profile_assets", return_value=(Path("ref.wav"), "ref", np.ones(1), 1, 1)), \
             patch.object(pipeline, "noise_limit", return_value=.1), patch.object(pipeline, "_ref_pitch", return_value=None), \
             patch.object(pipeline.engines, "synthesize", return_value=(a, sr)), \
             patch.object(pipeline.ending_guard, "synthesis_text", return_value=("hello", False)), \
             patch.object(pipeline.engines, "synthesis_report", return_value={}), \
             patch.object(pipeline, "evaluate", side_effect=[ev(["mispronounced"], 0.1), ev([], 9.0)]), \
             patch.object(pipeline.au, "normalize_lufs", side_effect=lambda a, *args, **kwargs: a):
            ch = pipeline.render_chunk(self.gen, 0, {}, {**config.DEFAULT_SETTINGS, "max_attempts": 2})
        self.assertEqual(ch["status"], "ok")
        self.assertEqual(ch["issues"], [])
        self.assertEqual(len(ch["attempts"]), 2)

    def test_token_cap_is_never_a_clean_take(self):
        a, sr = speech(natural=True)
        ev = {"audio": np.pad(a, (0, 3600)), "raw_audio": a, "sr": sr, "issues": [], "score": 0,
              "wer": 0, "sim": 1, "max_gap": 0, "dur": 1.15, "heard": "hello", "ending": au.end_analysis(a, sr, .95)}
        with patch.object(pipeline.engines, "get_spec", return_value=types.SimpleNamespace(family="qwen3")), \
             patch.object(pipeline.delivery, "resolve", return_value=({}, {})), \
             patch.object(pipeline.delivery, "pick_variant", return_value=0), \
             patch.object(pipeline, "profile_assets", return_value=(Path("ref.wav"), "ref", np.ones(1), 1, 1)), \
             patch.object(pipeline, "noise_limit", return_value=.1), patch.object(pipeline, "_ref_pitch", return_value=None), \
             patch.object(pipeline.engines, "synthesize", return_value=(a, sr)), \
             patch.object(pipeline.ending_guard, "synthesis_text", return_value=("hello", False)), \
             patch.object(pipeline.engines, "synthesis_report", return_value={"stop_reason": "length_limit_reached"}), \
             patch.object(pipeline, "evaluate", return_value=ev), \
             patch.object(pipeline.au, "normalize_lufs", side_effect=lambda a, *args, **kwargs: a):
            ch = pipeline.render_chunk(self.gen, 0, {}, {**config.DEFAULT_SETTINGS, "max_attempts": 1})
        self.assertEqual(ch["status"], "flagged")
        self.assertIn("cut_off", ch["issues"])

    def test_assembly_preserves_part_duration_and_wav_export(self):
        a, sr = speech(natural=True)
        a = np.pad(a, (0, 3600))
        sf.write(self.gdir / "chunk_000.wav", a, sr)
        self.gen["chunks"][0].update(file="chunk_000.wav", sr=sr, status="ok", issues=[])
        self.gen["voice_settings"] = {"studio_finish": False, "mic_match": False}
        pipeline.assemble(self.gen, config.DEFAULT_SETTINGS)
        master, sm = sf.read(self.gdir / "master.wav", dtype="float32")
        final, sfinal = sf.read(self.gdir / "final.wav", dtype="float32")
        np.testing.assert_array_equal(final, master)
        self.assertEqual(sm, sr)
        self.assertEqual(sfinal, sr)
        self.assertEqual(len(master), len(a) + int(.55 * sr))
        t = self.gen["timeline"][0]
        self.assertAlmostEqual(t["end"] - t["start"], len(a) / sr, places=3)
        self.assertGreater((self.gdir / "final.mp3").stat().st_size, 0)

    def _run_mocked_generation(self, scans):
        self.gen["timeline"] = [{"i": 0, "start": 0, "end": 1}]
        calls = []

        def render(gen, ci, *args, **kwargs):
            calls.append(ci)
            gen["chunks"][ci].update(status="ok", issues=[])

        def scan(gen):
            bad = scans.pop(0)
            gen["chunks"][0]["final_issues"] = ["cut_off"] if bad else []
            return bad

        with patch.object(pipeline.db, "get_generation", return_value=self.gen), \
             patch.object(pipeline.db, "get_profile", return_value={"id": "test"}), \
             patch.object(pipeline.db, "get_settings", return_value=config.DEFAULT_SETTINGS), \
             patch.object(pipeline.db, "put_generation_live", return_value=True), patch.object(pipeline.delivery, "backfill", return_value=False), \
             patch.object(pipeline.engines, "get_spec", return_value=types.SimpleNamespace(repo="test", name="test")), \
             patch.object(pipeline.engines, "is_downloaded", return_value=True), patch.object(pipeline.engines, "load"), \
             patch.object(pipeline, "render_chunk", side_effect=render), patch.object(pipeline, "assemble"), \
             patch.object(pipeline, "final_scan", side_effect=scan) as scan_mock:
            pipeline.run_generation(self.gen["id"], lambda: False, only_chunks=[0])
        self.assertEqual(scan_mock.call_count, 2)
        return calls

    def test_single_part_replacement_and_final_mix_are_rechecked(self):
        self.assertEqual(self._run_mocked_generation([[0], []]), [0, 0])
        self.assertEqual(self.gen["final_scan"]["fixed"], [0])
        self.assertEqual(self.gen["final_scan"]["remaining"], [])

    def test_failed_replacement_remains_flagged_and_is_not_reported_fixed(self):
        self._run_mocked_generation([[0], [0]])
        self.assertEqual(self.gen["final_scan"]["fixed"], [])
        self.assertEqual(self.gen["final_scan"]["remaining"], [0])
        self.assertEqual(self.gen["flagged"], [0])
        self.assertFalse(self.gen["chunks"][0]["final_fix"])
        self.assertIn("cut_off", self.gen["chunks"][0]["final_issues"])  # kept apart from the take's own issues


if __name__ == "__main__":
    unittest.main()
