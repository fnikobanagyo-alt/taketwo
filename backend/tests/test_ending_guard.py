"""Safety controls for Qwen's sentence continuation; no models or real data."""
import copy
import os
import sys
import tempfile
import types
import unittest
from contextlib import ExitStack
from pathlib import Path
from unittest.mock import patch

import numpy as np
import soundfile as sf

_data = tempfile.TemporaryDirectory(prefix="vf-end-guard-")
os.environ.setdefault("VOICEFORGE_DATA", _data.name)
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from app import config, ending_guard as guard, pipeline

SR = 24000


def tone(seconds, amp=.3):
    return (amp * np.sin(2 * np.pi * 210 * np.arange(round(seconds * SR)) / SR)).astype(np.float32)


def fixture():
    # Quiet 250-ms release lies beyond ASR's final-word end. Keep every sample.
    a = np.r_[tone(1), tone(.25, .0015), np.zeros(round(.35 * SR)), tone(1)].astype(np.float32)
    words = [{"word": " village.", "start": .5, "end": 1.},
             {"word": " Meanwhile,", "start": 1.6, "end": 1.85},
             {"word": " another", "start": 1.85, "end": 2.1},
             {"word": " chapter", "start": 2.1, "end": 2.35},
             {"word": " begins.", "start": 2.35, "end": 2.6}]
    return a, {"text": "village. " + guard.CONTINUATION, "segments": [{"words": words}]}


class EndingGuardTests(unittest.TestCase):
    def test_english_qwen_only_gets_continuation_when_explicitly_enabled(self):
        speak, enabled = guard.synthesis_text("A village.", "qwen3", "en", enabled=True)
        self.assertTrue(enabled)
        self.assertEqual(speak, "A village.\n\n" + guard.CONTINUATION)

    def test_unsupported_engines_and_languages_are_unchanged(self):
        for family, language in [("voxcpm2", "en"), ("qwen3", "ja"), ("generic", "en")]:
            self.assertEqual(guard.synthesis_text("A village", family, language, enabled=True), ("A village", False))

    def test_default_qwen_text_is_unchanged(self):
        self.assertEqual(guard.synthesis_text("A village.", "qwen3", "en"), ("A village.", False))

    def test_unpunctuated_chunk_has_sentence_separator(self):
        self.assertTrue(guard.synthesis_text("A village", "qwen3", "en", enabled=True)[0].startswith("A village.\n"))

    def test_quiet_release_preserved_and_context_not_exported(self):
        a, tr = fixture()
        kept, trimmed, info = guard.remove_context(a, SR, "village.", tr)
        self.assertGreater(len(kept), round(1.25 * SR))
        self.assertLess(len(kept), round(1.6 * SR))
        np.testing.assert_array_equal(kept, a[:len(kept)])
        self.assertEqual(trimmed["text"], "village.")
        self.assertEqual(len(trimmed["segments"][0]["words"]), 1)
        self.assertGreaterEqual(info["pause_end"] - info["pause_start"], .06)

    def test_whisper_end_time_alone_does_not_cut_the_audio(self):
        a, tr = fixture()
        kept, _, _ = guard.remove_context(a, SR, "village.", tr)
        np.testing.assert_array_equal(kept[SR:round(1.25 * SR)], a[SR:round(1.25 * SR)])

    def test_closure_inside_last_word_is_not_selected(self):
        a, tr = fixture()
        a[round(.99 * SR):round(1.06 * SR)] = 0
        kept, _, info = guard.remove_context(a, SR, "village.", tr)
        self.assertGreater(info["pause_start"], 1.1)
        np.testing.assert_array_equal(kept[:round(1.25 * SR)], a[:round(1.25 * SR)])

    def test_no_pause_is_rejected_without_saving_dummy_words(self):
        _, tr = fixture()
        with self.assertRaises(guard.UnsafeBoundary):
            guard.remove_context(tone(2.6), SR, "village.", tr)

    def test_missing_context_word_is_rejected(self):
        a, tr = fixture()
        tr["segments"][0]["words"].pop()
        with self.assertRaises(guard.UnsafeBoundary):
            guard.remove_context(a, SR, "village.", tr)

    def test_missing_target_word_is_rejected(self):
        a, tr = fixture()
        tr["segments"][0]["words"][0]["word"] = "butchered"
        with self.assertRaises(guard.UnsafeBoundary):
            guard.remove_context(a, SR, "village.", tr)

    def test_identical_context_in_script_uses_final_occurrence(self):
        a, tr = fixture()
        tr["segments"][0]["words"] = [
            {"word": "Meanwhile, another chapter begins.", "start": 0, "end": .4}
        ] + tr["segments"][0]["words"]
        kept, cropped, _ = guard.remove_context(a, SR, guard.CONTINUATION + " village.", tr)
        self.assertEqual(cropped["text"], guard.CONTINUATION + " village.")
        self.assertLess(len(kept), int(1.6 * SR))

    def _render(self, a, tr, directory, limit=1, experimental=True, generated_texts=None):
        gen = {"id": directory.name, "engine": "qwen3-tts-1.7b", "language": "en", "params": {},
               "chunks": [{"i": 0, "text": "village.", "pause_after": "end"}]}
        ev = {"audio": a[:SR], "raw_audio": a[:SR], "sr": SR, "issues": [], "score": 0.,
              "wer": 0., "sim": 1., "max_gap": 0., "dur": 1., "heard": "village.", "ending": {}}
        def synth(*args, **kwargs):
            if generated_texts is not None:
                generated_texts.append(args[1])
            return a, SR
        with ExitStack() as stack:
            for obj, name, kw in [
                (pipeline.engines, "get_spec", {"return_value": types.SimpleNamespace(family="qwen3")}),
                (pipeline.delivery, "resolve", {"return_value": ({}, {})}),
                (pipeline.delivery, "pick_variant", {"return_value": 0}),
                (pipeline, "profile_assets", {"return_value": (Path("ref"), "ref", np.ones(1), 1., 1.)}),
                (pipeline, "noise_limit", {"return_value": .1}), (pipeline, "_ref_pitch", {"return_value": None}),
                (pipeline.engines, "synthesize", {"side_effect": synth}),
                (pipeline.engines, "synthesis_report", {"return_value": {}}),
                (pipeline.ml, "transcribe", {"return_value": tr}),
                (pipeline, "evaluate", {"side_effect": lambda *args, **kwargs: copy.deepcopy(ev)}),
                (pipeline.au, "normalize_lufs", {"side_effect": lambda x, *args, **kwargs: x})]:
                stack.enter_context(patch.object(obj, name, **kw))
            return pipeline.render_chunk(gen, 0, {}, {**config.DEFAULT_SETTINGS, "max_attempts": limit,
                                                     "experimental_qwen_ending_guard": experimental})

    def test_default_render_does_not_synthesize_or_remove_dummy_context(self):
        with tempfile.TemporaryDirectory(dir=config.GENERATIONS_DIR) as tmp:
            directory = Path(tmp)
            texts = []
            a, tr = fixture()
            with patch.object(guard, "remove_context", side_effect=AssertionError("Default must not remove context")):
                ch = self._render(a, tr, directory, experimental=False, generated_texts=texts)
            self.assertEqual(texts, ["village."])
            self.assertIsNone(ch["ending_guard"])
            self.assertIsNone(ch["context_file"])
            self.assertFalse(config.DEFAULT_SETTINGS["experimental_qwen_ending_guard"])

    def test_false_string_cannot_accidentally_enable_experimental_context(self):
        with tempfile.TemporaryDirectory(dir=config.GENERATIONS_DIR) as tmp:
            texts = []
            a, tr = fixture()
            ch = self._render(a, tr, Path(tmp), experimental="false", generated_texts=texts)
            self.assertEqual(texts, ["village."])
            self.assertIsNone(ch["context_file"])

    def test_real_render_path_saves_separate_context_evidence(self):
        with tempfile.TemporaryDirectory(dir=config.GENERATIONS_DIR) as tmp:
            directory = Path(tmp)
            a, tr = fixture()
            ch = self._render(a, tr, directory)
            self.assertEqual(ch["status"], "ok")
            self.assertIsNotNone(ch["ending_guard"])
            saved, sr = sf.read(directory / ch["context_file"], dtype="float32")
            np.testing.assert_array_equal(saved, a)
            self.assertEqual(sr, SR)
            self.assertGreater(ch["attempts"][0]["expected_seconds"], 4.)

    def test_all_unsafe_takes_leave_old_audio_untouched(self):
        with tempfile.TemporaryDirectory(dir=config.GENERATIONS_DIR) as tmp:
            directory = Path(tmp)
            old = b"previous clip"
            (directory / "chunk_000.wav").write_bytes(old)
            _, tr = fixture()
            with self.assertRaisesRegex(ValueError, "safely separate"):
                self._render(tone(2.6), tr, directory, limit=2)
            self.assertEqual((directory / "chunk_000.wav").read_bytes(), old)
            self.assertFalse((directory / "raw_chunk_000.wav").exists())

    def test_each_sentence_inside_one_part_is_guarded(self):
        a, tr = fixture()
        seeds = []
        def synth(*args, **kwargs):
            seeds.append(args[6])
            self.assertEqual(args[1], "village.\n\n" + guard.CONTINUATION)
            return a, SR
        with patch.object(pipeline.engines, "synthesize", side_effect=synth), \
             patch.object(pipeline.engines, "synthesis_report", return_value={}), \
             patch.object(pipeline.ml, "transcribe", return_value=tr):
            raw, sr, heard, context, full, report = pipeline._guarded_take(
                "village. village.", "qwen3-tts-1.7b", Path("ref"), "ref", "en", {}, 12, 2., 1., 300, lambda: False)
        # split_sentences accepts lower-case next sentences too (tested below).
        self.assertEqual(len(seeds), 2)
        self.assertNotEqual(seeds[0], seeds[1])
        self.assertEqual(heard["text"], "village. village.")
        self.assertEqual(len(context["sentences"]), 2)
        self.assertGreater(heard["segments"][0]["words"][1]["start"], 1.5)
        self.assertEqual(len(full), 2 * len(a))
        self.assertEqual(sr, SR)

    def test_new_narration_can_continue_with_flagged_plain_fallback(self):
        with tempfile.TemporaryDirectory(dir=config.GENERATIONS_DIR) as tmp:
            _, tr = fixture()
            ch = self._render(tone(2.6), tr, Path(tmp), limit=1)
            self.assertEqual(ch["status"], "flagged")
            self.assertIn("ending_unverified", ch["issues"])
            self.assertIsNone(ch["context_file"])
            self.assertTrue(ch["attempts"][-1]["ending_guard_fallback"])
            self.assertNotEqual(ch["attempts"][0]["seed"], ch["attempts"][-1]["seed"])

    def test_bad_second_boundary_does_not_regenerate_first_sentence(self):
        a, tr = fixture()
        with patch.object(pipeline.engines, "synthesize", side_effect=[(a, SR), (tone(2.6), SR), (a, SR)]) as synth, \
             patch.object(pipeline.engines, "synthesis_report", return_value={}), \
             patch.object(pipeline.ml, "transcribe", return_value=tr):
            _, _, _, context, _, _ = pipeline._guarded_take(
                "village. village.", "qwen3-tts-1.7b", Path("ref"), "ref", "en", {}, 12, 2., 1., 300, lambda: False)
        self.assertEqual(synth.call_count, 3)
        self.assertEqual(len(context["sentences"][0]["boundary_attempts"]), 1)
        self.assertEqual(len(context["sentences"][1]["boundary_attempts"]), 2)
        seeds=[call.args[6] for call in synth.call_args_list]
        self.assertEqual(len(set(seeds)), 3)


if __name__ == "__main__":
    unittest.main()
