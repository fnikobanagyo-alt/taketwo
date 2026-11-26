"""Monotone/awkward-pause findings of 11 Nov 2026: pitch level, sentence-end pauses, timings, MCP options.

Run: backend/.venv/bin/python -B -m unittest discover -s backend/tests -v.
Temporary data only; no model is loaded, downloaded or contacted.
"""
from __future__ import annotations

import os
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

import numpy as np

_data = tempfile.TemporaryDirectory(prefix="voiceforge-quality-tests-")
os.environ.setdefault("TAKETWO_DATA", _data.name)
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from app import audio as au, config, pipeline  # noqa: E402

SR = 24000


def voiced(seconds):
    a = (0.4 * np.sin(2 * np.pi * 200 * np.arange(int(seconds * SR)) / SR)).astype(np.float32)
    a[-4800:] *= np.geomspace(1.0, 1e-5, 4800)
    return a


class PitchLevelTests(unittest.TestCase):
    """Takes of one voice drifted 138 -> 190 Hz (5.5 semitones) and all passed."""

    def setUp(self):
        self.gdir = config.GENERATIONS_DIR / self.id().split(".")[-1]
        self.gdir.mkdir(parents=True)
        self.gen = {"id": self.gdir.name, "engine": "qwen3-tts-1.7b", "language": "en", "params": {}, "profile_id": "p",
                    "chunks": [{"i": 0, "text": "One small nudge.", "pause_after": "sentence", "status": "pending"},
                               {"i": 1, "text": "High tide stops knocking.", "pause_after": "end", "status": "pending"}]}

    def _render(self, ci, take_pitches, profile_hz=150.0):
        a = voiced(2.0)
        ev = lambda: {"audio": a.copy(), "raw_audio": a, "sr": SR, "issues": [], "score": 0.0, "wer": 0, "sim": 1,
                      "max_gap": 0, "dur": 2.0, "heard": "x", "ending": {}}
        pitches = iter(take_pitches)
        with patch.object(pipeline.engines, "get_spec", return_value=types.SimpleNamespace(family="qwen3")), \
             patch.object(pipeline.delivery, "resolve", return_value=({}, {})), \
             patch.object(pipeline.delivery, "pick_variant", return_value=0), \
             patch.object(pipeline, "profile_assets", return_value=(Path("ref.wav"), "ref", np.ones(1), 1, 1)), \
             patch.object(pipeline, "noise_limit", return_value=.1), patch.object(pipeline, "_ref_pitch", return_value=None), \
             patch.object(pipeline.engines, "synthesize", return_value=(a, SR)), \
             patch.object(pipeline.engines, "synthesis_report", return_value={}), \
             patch.object(pipeline.ending_guard, "synthesis_text", return_value=("x", False)), \
             patch.object(pipeline, "evaluate", side_effect=lambda *a_, **k: ev()), \
             patch.object(pipeline.delivery, "prosody", side_effect=lambda *a_: {"pitch_st": 2.0, "pitch_hz": next(pitches)}), \
             patch.object(pipeline.au, "normalize_lufs", side_effect=lambda x, *args, **kw: x):
            return pipeline.render_chunk(self.gen, ci, {"pitch_hz": profile_hz}, {**config.DEFAULT_SETTINGS, "max_attempts": 4})

    def test_take_far_from_the_voice_median_is_retaken(self):
        # 190 Hz against a 150 Hz voice is +4.1 semitones (5f0a4e7b304d against this voice).
        ch = self._render(0, [190.0, 152.0])
        self.assertEqual([a_["issues"] for a_ in ch["attempts"]], [["off_pitch"], []])
        self.assertEqual(ch["status"], "ok")
        self.assertEqual(ch["pitch_hz"], 152.0)

    def test_take_far_from_its_neighbour_part_is_retaken(self):
        # Both 140 and 165 Hz are within 2 semitones of 150 Hz, but 2.9 semitones apart from each other.
        self.gen["chunks"][0].update(status="ok", file="chunk_000.wav", pitch_hz=140.0)
        ch = self._render(1, [165.0, 146.0])
        self.assertEqual([a_["issues"] for a_ in ch["attempts"]], [["off_pitch"], []])
        self.assertEqual(ch["pitch_hz"], 146.0)

    def test_closest_pitch_is_kept_when_every_take_is_off(self):
        ch = self._render(0, [200.0, 185.0, 210.0, 195.0])
        self.assertEqual(ch["status"], "flagged")
        self.assertEqual(ch["issues"], ["off_pitch"])
        self.assertEqual(ch["pitch_hz"], 185.0)

    def test_voice_without_a_stored_median_is_not_gated(self):
        ch = self._render(0, [200.0], profile_hz=None)
        self.assertEqual(ch["status"], "ok")


class SentencePauseTests(unittest.TestCase):
    """6073f671476d: full stops inside a part got 0.06-0.13 s while the part join got the 0.30 s setting."""

    def _take(self, gap_s):
        rng = np.random.default_rng(0)
        quiet = (0.0005 * rng.standard_normal(int(gap_s * SR))).astype(np.float32)  # the take's own room tone
        word = lambda sec: np.concatenate([voiced(sec)[:-4800], voiced(sec)[-4800:-4320] * np.geomspace(1, 1e-4, 480)])
        return word(1.0), quiet, word(1.2)  # words end within 20 ms, so the pause is just the quiet

    def _words(self, first_end="half."):
        return [{"word": " Pull", "start": 0.0, "end": 0.5}, {"word": " " + first_end, "start": 0.5, "end": 0.97},
                {"word": " One", "start": 1.08, "end": 1.5}, {"word": " nudge.", "start": 1.5, "end": 2.2}]

    def _gap_after_first(self, a):
        rep = au.silence_report(a, SR, min_gap=0.02)
        return max(e - s for s, e in rep.internal_gaps)

    def test_short_full_stop_is_widened_to_the_target_without_touching_speech(self):
        s1, quiet, s2 = self._take(0.08)
        a = np.concatenate([s1, quiet, s2])
        out = pipeline.widen_sentence_pauses(a, SR, "Pull half. One nudge.", self._words(), 0.30)[0]
        self.assertAlmostEqual(self._gap_after_first(out), 0.30, delta=0.03)
        np.testing.assert_array_equal(out[:len(s1)], s1)
        np.testing.assert_array_equal(out[-len(s2):], s2)
        self.assertGreater(np.abs(out[len(s1):-len(s2)]).max(), 0)  # room tone, not digital silence

    def test_full_stop_misheard_as_a_comma_is_still_widened(self):
        # Whisper heard "half," for the rushed "half." in 6073f671476d.
        a = np.concatenate(self._take(0.08))
        out = pipeline.widen_sentence_pauses(a, SR, "Pull half. One nudge.", self._words("half,"), 0.30)[0]
        self.assertAlmostEqual(self._gap_after_first(out), 0.30, delta=0.03)

    def test_pause_goes_after_the_word_not_into_a_dip_inside_it(self):
        # "knocking." in 6073f671476d: a 20 ms dip in "-ing" lay nearer the word end than the real 60 ms pause.
        s1, quiet, s2 = self._take(0.06)
        dip = len(s1) - int(0.12 * SR)
        s1 = s1.copy()
        s1[dip:dip + int(0.02 * SR)] *= 1e-4
        a = np.concatenate([s1, quiet, s2])
        words = self._words()
        words[1] = {**words[1], "end": len(s1) / SR - 0.09}  # Whisper's end: 0.03 s after the dip, 0.09 s before the pause
        words[2] = {**words[2], "start": len(s1) / SR + 0.08}
        out = pipeline.widen_sentence_pauses(a, SR, "Pull half. One nudge.", words, 0.30)[0]
        np.testing.assert_array_equal(out[:len(s1)], s1)
        self.assertAlmostEqual(self._gap_after_first(out), 0.30, delta=0.03)

    def test_long_enough_pause_and_comma_are_left_alone(self):
        a = np.concatenate(self._take(0.40))
        np.testing.assert_array_equal(pipeline.widen_sentence_pauses(a, SR, "Pull half. One nudge.", self._words(), 0.30)[0], a)
        b = np.concatenate(self._take(0.08))
        np.testing.assert_array_equal(pipeline.widen_sentence_pauses(b, SR, "Pull half, one nudge.", self._words("half,"), 0.30)[0], b)


class SentencePauseRenderTests(unittest.TestCase):
    def _render(self, pause_ms):
        import soundfile as sf

        gdir = config.GENERATIONS_DIR / f"pause{pause_ms}"
        gdir.mkdir(parents=True, exist_ok=True)
        gen = {"id": gdir.name, "engine": "qwen3-tts-1.7b", "language": "en", "params": {}, "profile_id": "p",
               "chunks": [{"i": 0, "text": "Pull half. One nudge.", "pause_after": "end", "status": "pending"}]}
        s1, quiet, s2 = SentencePauseTests._take(None, 0.08)
        a = np.concatenate([s1, quiet, s2])
        # evaluate trimmed 0.3 s of lead-in and padded 0.02 s: word times are in the untrimmed take
        words = [{**w, "start": w["start"] + 0.28, "end": w["end"] + 0.28} for w in SentencePauseTests._words(None)]
        ev = {"audio": a, "raw_audio": a, "sr": SR, "issues": [], "score": 0.0, "wer": 0, "sim": 1, "max_gap": 0.08,
              "dur": len(a) / SR, "heard": "x", "words": words, "ending": {"trim_start": 0.3, "head_pad_s": 0.02}}
        with patch.object(pipeline.engines, "get_spec", return_value=types.SimpleNamespace(family="qwen3")), \
             patch.object(pipeline.delivery, "resolve", return_value=({}, {})), \
             patch.object(pipeline.delivery, "pick_variant", return_value=0), \
             patch.object(pipeline, "profile_assets", return_value=(Path("ref.wav"), "ref", np.ones(1), 1, 1)), \
             patch.object(pipeline, "noise_limit", return_value=.1), patch.object(pipeline, "_ref_pitch", return_value=None), \
             patch.object(pipeline.engines, "synthesize", return_value=(a, SR)), \
             patch.object(pipeline.engines, "synthesis_report", return_value={}), \
             patch.object(pipeline.ending_guard, "synthesis_text", return_value=("x", False)), \
             patch.object(pipeline, "evaluate", return_value=ev), \
             patch.object(pipeline.au, "normalize_lufs", side_effect=lambda x, *args, **kw: x):
            ch = pipeline.render_chunk(gen, 0, {}, {**config.DEFAULT_SETTINGS, "inner_sentence_pause_ms": pause_ms})
        out, _ = sf.read(gdir / ch["file"], dtype="float32")
        self.ch = ch
        return max(e - s for s, e in au.silence_report(out, SR, min_gap=0.02).internal_gaps)

    def test_full_stop_inside_a_part_gets_the_setting_when_turned_on(self):
        self.assertAlmostEqual(self._render(300), 0.30, delta=0.03)

    def test_kept_word_times_follow_the_widened_audio(self):
        self._render(300)
        starts = [w["start"] for w in self.ch["words"]]
        self.assertAlmostEqual(starts[0], 0.0, delta=0.001)  # untrimmed 0.28 s, minus 0.3 s trim, plus 0.02 s pad
        self.assertAlmostEqual(starts[2], 1.08 + 0.30 - 0.08, delta=0.03)  # after the full stop: moved by the added quiet

    def test_off_by_default_until_a_length_is_chosen_by_ear(self):
        self.assertEqual(config.DEFAULT_SETTINGS["inner_sentence_pause_ms"], 0)
        self.assertAlmostEqual(self._render(0), 0.08, delta=0.03)


class TimingsTests(unittest.TestCase):
    """A caller sends one whole script and still cuts video to each sentence and word of the final file."""

    def _narration(self, gid, words0):
        from app import db

        db.put_generation({"id": gid, "profile_id": "p", "engine": "qwen3-tts-1.7b", "language": "en", "title": "t",
                           "status": "done", "speed": 2.0, "text": "x", "cleaned_text": "x", "settings": {},
                           # timeline is already in final-file time (after speed)
                           "timeline": [{"i": 0, "start": 0.075, "end": 1.2}, {"i": 1, "start": 1.7, "end": 2.1}],
                           "chunks": [{"i": 0, "text": "Pull half. One nudge.", "status": "ok", "words": words0},
                                      {"i": 1, "text": "Go now.", "status": "ok",
                                       "words": [{"word": " Go", "start": 0.1, "end": 0.3}, {"word": " now.", "start": 0.3, "end": 0.7}]}]})

    def test_sentence_and_word_times_are_in_the_final_file_after_speed(self):
        from app import main

        self._narration("timed1", [{"word": " Pull", "start": 0.0, "end": 0.5}, {"word": " half,", "start": 0.5, "end": 0.97},
                                   {"word": " One", "start": 1.08, "end": 1.5}, {"word": " nudge.", "start": 1.5, "end": 2.2}])
        t = main.generation_timings("timed1")
        self.assertEqual(t["words"], [
            {"word": "Pull", "start": 0.075, "end": 0.325}, {"word": "half.", "start": 0.325, "end": 0.56},
            {"word": "One", "start": 0.615, "end": 0.825}, {"word": "nudge.", "start": 0.825, "end": 1.175},
            {"word": "Go", "start": 1.75, "end": 1.85}, {"word": "now.", "start": 1.85, "end": 2.05}])
        self.assertEqual(t["sentences"], [
            {"text": "Pull half.", "start": 0.075, "end": 0.56}, {"text": "One nudge.", "start": 0.615, "end": 1.175},
            {"text": "Go now.", "start": 1.75, "end": 2.05}])
        self.assertFalse(t["estimated"])

    def test_misheard_word_takes_the_time_between_its_neighbours(self):
        from app import main

        self._narration("timed2", [{"word": " Pull", "start": 0.0, "end": 0.5}, {"word": " calf.", "start": 0.5, "end": 0.97},
                                   {"word": " One", "start": 1.08, "end": 1.5}, {"word": " nudge.", "start": 1.5, "end": 2.2}])
        words = main.generation_timings("timed2")["words"]
        self.assertEqual(words[1], {"word": "half.", "start": 0.325, "end": 0.615})

    def test_older_narration_without_word_times_gets_estimated_times(self):
        from app import main

        self._narration("timed3", None)
        t = main.generation_timings("timed3")
        self.assertTrue(t["estimated"])
        self.assertEqual(t["sentences"][0]["start"], 0.075)
        self.assertEqual(t["sentences"][1]["end"], 1.2)
        self.assertEqual(len(t["words"]), 6)


class McpTests(unittest.TestCase):
    """The MCP server passes Style and Stability through and returns timings."""

    def setUp(self):
        import importlib.util

        fastmcp = types.ModuleType("mcp.server.fastmcp")
        fastmcp.FastMCP = lambda name: types.SimpleNamespace(tool=lambda: (lambda f: f), run=lambda: None)
        mods = {"mcp": types.ModuleType("mcp"), "mcp.server": types.ModuleType("mcp.server"), "mcp.server.fastmcp": fastmcp}
        with patch.dict(sys.modules, mods):
            spec = importlib.util.spec_from_file_location("taketwo_mcp", Path(__file__).resolve().parents[2] / "mcp" / "taketwo_mcp.py")
            self.m = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(self.m)
        self.calls = []
        self.m._voice_id = lambda v: "vid"
        self.m._call = lambda method, path, body=None: self.calls.append((method, path, body)) or {"id": "g1", "chunks": [1]}

    def test_style_and_stability_reach_the_narration_request(self):
        self.m.generate_narration("Hi.", "the narrator voice", style=0.4, stability=0.7)
        self.assertEqual(self.calls[0][2]["voice_settings"], {"style": 0.4, "stability": 0.7})

    def test_default_request_keeps_the_voice_settings_untouched(self):
        self.m.generate_narration("Hi.", "the narrator voice")
        self.assertNotIn("voice_settings", self.calls[0][2])

    def test_timings_come_from_the_narration(self):
        self.m.get_timings("g1")
        self.assertEqual(self.calls[0][:2], ("GET", "/api/generations/g1/timings"))


class VoicePitchTests(unittest.TestCase):
    def test_voice_gains_its_median_pitch_without_touching_its_clips(self):
        import json

        import soundfile as sf
        from app import delivery

        pdir = config.PROFILES_DIR / "pitchvoice"
        pdir.mkdir(parents=True)
        t = np.arange(SR * 3) / SR
        sf.write(pdir / "clean.wav", (0.3 * np.sin(2 * np.pi * 150 * t)).astype(np.float32), SR)
        sf.write(pdir / "ref_0.wav", np.zeros(SR, np.float32), SR)
        (pdir / "mic.json").write_text(json.dumps({"bands": []}))
        before = (pdir / "ref_0.wav").read_bytes()
        profile = {"id": "pitchvoice", "variants": [{"index": 0, "file": "ref_0.wav", "expressiveness": 0.5}]}
        self.assertTrue(delivery.backfill(profile, pdir))
        self.assertAlmostEqual(profile["pitch_hz"], 150, delta=3)
        self.assertEqual((pdir / "ref_0.wav").read_bytes(), before)
        self.assertFalse(delivery.backfill(profile, pdir))  # measured once


if __name__ == "__main__":
    unittest.main()
