"""Regression tests for the bug audit of 10 Oct 2026 (VoiceForge_Full_Bug_Audit.txt).

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
import soundfile as sf

_data = tempfile.TemporaryDirectory(prefix="voiceforge-audit-tests-")
os.environ.setdefault("VOICEFORGE_DATA", _data.name)
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from app import audio as au, config, db, delivery, engines, main, pipeline, reference, studio, textproc, tools  # noqa: E402

SR = 24000


def tone(seconds, decay=0.15, amp=0.3):
    """The audit's fixture: a steady tone that fades over `decay` seconds."""
    a = (amp * np.sin(2 * np.pi * 200 * np.arange(int(seconds * SR)) / SR)).astype(np.float32)
    if decay:
        a[-int(decay * SR):] *= np.geomspace(1, 1e-5, int(decay * SR))
    return a


def simple_norm(t):
    import re

    return re.sub(r"[^\w\s]", "", t.lower()).strip()


def setUpModule():
    global _normalizer_patch
    _normalizer_patch = patch.object(pipeline.ml, "normalize_for_wer", side_effect=simple_norm)
    _normalizer_patch.start()


def tearDownModule():
    _normalizer_patch.stop()


class EndingTests(unittest.TestCase):
    def test_short_terminal_sound_after_a_pause_is_kept(self):
        # Audit finding 02: a 60 ms sound 150 ms after the ASR end used to be cut away (cut 1.04 s).
        a = np.concatenate([tone(1), np.zeros(int(0.15 * SR), np.float32), tone(0.06, 0.02), np.zeros(int(0.2 * SR), np.float32)])
        e = au.end_analysis(a, SR, 1.0)
        self.assertGreaterEqual(e["cut"], 1.21, "the sound that ends at 1.21 s must not be trimmed")
        self.assertFalse(e["junk"])

    def test_next_phrase_after_a_long_silence_is_not_kept(self):
        a = np.concatenate([tone(1), np.zeros(int(0.5 * SR), np.float32), tone(0.5, 0.05), np.zeros(int(0.2 * SR), np.float32)])
        e = au.end_analysis(a, SR, 1.0)
        self.assertLess(e["cut"], 1.3)

    def test_a_faint_tail_does_not_hide_a_hard_stop(self):
        # Goal item 2: the word stops at full level; 300 ms later a faint sound (42 dB under speech) used to
        # move the measuring point onto itself, so the ending read as natural.
        faint = tone(0.08, 0, amp=0.3 * 10 ** (-42 / 20))
        a = np.concatenate([tone(1, 0), np.zeros(int(0.3 * SR), np.float32), faint, np.zeros(int(0.2 * SR), np.float32)])
        e = au.end_analysis(a, SR, 1.0)
        self.assertFalse(e["natural"])
        self.assertGreaterEqual(e["cut"], 1.38, "the faint sound is still kept, only not used to judge the ending")

    def test_a_short_last_word_alone_is_not_a_cut(self):
        # Listening test, 2026-11-25: endings flagged only for a short, still-audible last word sounded mostly fine.
        take = np.concatenate([tone(1.75, 0), tone(0.35, 0), tone(0.03, 0, amp=0.3 * 10 ** (-27 / 20)), np.zeros(int(0.3 * SR), np.float32)])
        words = [{"word": "words", "start": k * 0.35, "end": k * 0.35 + 0.3} for k in range(5)] + [{"word": "Between.", "start": 1.75, "end": 2.13}]
        text = "words words words words words Between."
        with patch.object(pipeline.ml, "speaker_embedding", return_value=np.ones(1)), \
             patch.object(pipeline.ml, "cosine", return_value=1.0), \
             patch.object(pipeline.ml, "normalize_for_wer", side_effect=simple_norm):
            ev = pipeline.evaluate(take, SR, text, np.ones(1), 1.0, 2.5, config.DEFAULT_SETTINGS, "en", transcript={"text": text, "segments": [{"words": words}]})
        self.assertNotIn("cut_off", ev["issues"])

    def test_a_title_like_mr_is_not_a_sentence_end_inside_a_take(self):
        # Step 6: every take of "...Later, Mister Bell rode up..." failed because Whisper writes "Mr." and the
        # check judged the still-loud voice after it as a sentence cut off. A listener marked such parts good.
        a = np.concatenate([tone(2.0, 0), tone(1.5, 0.15), np.zeros(int(0.3 * SR), np.float32)])
        words = [{"word": "Later,", "start": 0.0, "end": 0.6}, {"word": "Mr.", "start": 0.7, "end": 1.0},
                 {"word": "Bell", "start": 1.2, "end": 1.6}, {"word": "rode", "start": 1.7, "end": 2.2}, {"word": "up.", "start": 2.3, "end": 3.4}]
        self.assertFalse(pipeline._internal_sentence_cut(a, SR, words))
        words[1]["word"] = "hand."  # the same sound after a real sentence end is still a cut
        self.assertTrue(pipeline._internal_sentence_cut(a, SR, words))

    def test_the_next_sentence_starting_early_is_not_a_cut(self):
        # "'Nobody,' she said. But..." (marked good by ear): Whisper puts "But" late, so cutting the take at
        # 80% of the gap caught the start of "But" and read it as the sentence stopping loud.
        a = np.concatenate([tone(1.0, 0.15), np.zeros(int(0.15 * SR), np.float32), tone(1.0, 0), np.zeros(int(0.3 * SR), np.float32)])
        words = [{"word": w, "start": i * 0.2, "end": i * 0.2 + 0.15} for i, w in enumerate(("one", "two", "three", "four"))]
        words += [{"word": "said.", "start": 0.8, "end": 1.0}, {"word": "But", "start": 1.3, "end": 1.6}, {"word": "more.", "start": 1.7, "end": 2.1}]
        self.assertFalse(pipeline._internal_sentence_cut(a, SR, words))

    def test_valid_short_final_word_is_not_junk(self):
        # Audit finding 04: a complete 200 ms final word after a 200 ms pause was flagged junk_tail.
        x = np.concatenate([tone(2), np.zeros(int(0.2 * SR), np.float32), tone(0.2, 0.1), np.zeros(int(0.2 * SR), np.float32)])
        self.assertTrue(au.end_analysis(x, SR, 2.4)["natural"])


class MasteringTests(unittest.TestCase):
    def test_expander_keeps_quiet_word_tails(self):
        # Audit R1: a quiet tail lost 11.45 dB through the expander.
        x = np.concatenate([tone(2, 0), tone(0.4, 0, amp=0.3 * 10 ** (-45 / 20)), np.zeros(SR // 2, np.float32)])
        y = studio.expander(x, SR)
        lo, hi = int(2.25 * SR), int(2.4 * SR)
        gain = 20 * np.log10(np.linalg.norm(y[lo:hi]) / np.linalg.norm(x[lo:hi]))
        self.assertGreater(gain, -1.0)

    def test_de_esser_keeps_soft_fricative_endings(self):
        # The de-esser cut soft "s"/"t" word endings (67 of 321 real takes lost >3 dB).
        t = np.arange(int(1.0 * SR)) / SR
        vowel = 0.3 * np.sin(2 * np.pi * 180 * t[: int(0.8 * SR)])
        fric = 0.03 * np.sin(2 * np.pi * 7000 * t[int(0.8 * SR):]) * np.hanning(int(0.2 * SR))
        x = np.concatenate([vowel, fric]).astype(np.float32)
        y = studio.de_ess(x, SR)
        lo = int(0.8 * SR)
        self.assertGreater(20 * np.log10(np.linalg.norm(y[lo:]) / np.linalg.norm(x[lo:])), -1.5)


class FinalScanTests(unittest.TestCase):
    def test_valid_short_final_word_passes_the_final_check(self):
        # Audit finding 04: a complete 200 ms final word after a pause was flagged junk_tail.
        gid = "shortwordscan"
        gdir = config.GENERATIONS_DIR / gid
        gdir.mkdir(parents=True, exist_ok=True)
        x = np.concatenate([tone(2), np.zeros(int(0.2 * SR), np.float32), tone(0.2, 0.1), np.zeros(int(0.2 * SR), np.float32)])
        sf.write(gdir / "raw.wav", x, SR, subtype="FLOAT")
        sf.write(gdir / "final.wav", x, SR, subtype="PCM_16")
        gen = {"id": gid, "chunks": [{"i": 0, "raw_file": "raw.wav", "issues": [], "ending": {"last_word_end": 2.4}}],
               "timeline": [{"i": 0, "start": 0.0, "end": len(x) / SR}]}
        self.assertEqual(pipeline.final_scan(gen), [])
        self.assertNotIn("junk_tail", gen["chunks"][0]["final_issues"])


class TakeCheckTests(unittest.TestCase):
    def _evaluate(self, text, heard, audio):
        tr = {"text": heard, "segments": [{"words": [{"word": heard.split()[-1], "start": 0.0, "end": len(audio) / SR}]}]}
        with patch.object(pipeline.ml, "transcribe", return_value=tr), \
             patch.object(pipeline.ml, "speaker_embedding", return_value=np.ones(1)), \
             patch.object(pipeline.ml, "cosine", return_value=1.0), \
             patch.object(pipeline.ml, "normalize_for_wer", side_effect=simple_norm):
            return pipeline.evaluate(audio, SR, text, np.ones(1), 1.0, 2.5, config.DEFAULT_SETTINGS, "en")

    def test_dropped_last_word_is_rejected(self):
        # Audit finding 01: WER 0.111 was accepted although "home" was never heard.
        a = np.pad(tone(4), (0, int(0.2 * SR)))
        ev = self._evaluate("We walked through the old village before returning home.",
                            "We walked through the old village before returning.", a)
        self.assertIn("end_word_missing", ev["issues"])

    def test_complete_last_word_is_accepted(self):
        a = np.pad(tone(4), (0, int(0.2 * SR)))
        ev = self._evaluate("We walked through the old village before returning home.",
                            "We walked through the old village before returning home.", a)
        self.assertNotIn("end_word_missing", ev["issues"])

    def test_hard_stop_after_first_sentence_is_rejected(self):
        # Audit finding 03: only the last sentence of a chunk was checked.
        raw = np.concatenate([tone(0.85, 0.0), np.zeros(int(0.3 * SR), np.float32), tone(1.0, 0.2)])  # stops hard after "First."
        words = [{"word": "First.", "start": 0.0, "end": 0.85}, {"word": "Second", "start": 1.15, "end": 1.6}]
        self.assertTrue(pipeline._internal_sentence_cut(raw, SR, words))

    def test_natural_sentence_ends_pass(self):
        a = np.concatenate([tone(0.9, 0.3), np.zeros(int(0.3 * SR), np.float32), tone(1.0, 0.2)])
        words = [{"word": "First.", "start": 0.0, "end": 0.9}, {"word": "Second", "start": 1.2, "end": 1.9}]
        self.assertFalse(pipeline._internal_sentence_cut(a, SR, words))


class TextTests(unittest.TestCase):
    def test_overlong_unspaced_run_is_split(self):
        # Audit finding 30: an 800-character unspaced run stayed one chunk.
        chunks = textproc.chunk_text("你好" * 400 + ".", 220)
        self.assertTrue(all(len(c.text) <= 220 for c in chunks), [len(c.text) for c in chunks])
        self.assertEqual("".join(c.text for c in chunks).replace(" ", ""), "你好" * 400 + ".")

    def test_cjk_counts_as_words(self):
        # Audit finding 23: 200 Chinese characters counted as one word.
        self.assertGreaterEqual(textproc.word_count("你好世界" * 50), 200)
        self.assertEqual(textproc.word_count("Hello there, friend."), 3)


class ExportTests(unittest.TestCase):
    def test_unspaced_caption_does_not_crash(self):
        # Audit finding 14: _wrap raised UnboundLocalError on one long token.
        out = tools.to_srt([{"start": 0, "end": 2, "text": "这是一段没有空格的中文句子" * 5}])
        self.assertIn("-->", out)

    def test_non_latin_download_header_encodes(self):
        # Audit finding 13: a non-Latin title crashed the subtitle download.
        header = tools.attachment_header("你好世界", "srt")
        header.encode("latin-1")  # must not raise
        self.assertIn("filename*=UTF-8''", header)
        self.assertIn('filename="download.srt"', header)


class SettingsAndEngineTests(unittest.TestCase):
    def test_invalid_setting_is_rejected(self):
        # Audit finding 22: max_attempts=0 was persisted and broke generation.
        with self.assertRaises(ValueError):
            db.save_settings({"max_attempts": 0})
        with self.assertRaises(ValueError):
            db.save_settings({"max_wer": float("nan")})
        self.assertEqual(db.save_settings({"max_attempts": 3})["max_attempts"], 3)

    def test_custom_model_gets_its_family_parameters(self):
        # Audit finding 08: a custom Qwen model failed with KeyError('top_p').
        engines.set_custom_engines([{"id": "custom-qwen", "repo": "local/Qwen3-TTS-test"}])
        keys = {p.key for p in engines.get_spec("custom-qwen").params}
        self.assertTrue({"temperature", "top_p", "top_k"} <= keys)

    def test_same_basename_models_get_distinct_ids(self):
        # Audit finding 09: owner-a/model and owner-b/model both became custom-model.
        items = [{"id": "custom-model", "repo": "owner-a/model"}]
        self.assertNotEqual(main._custom_engine_id("owner-b/model", items), "custom-model")
        self.assertEqual(main._custom_engine_id("owner-a/model", items), "custom-model")

    @unittest.skipUnless(hasattr(main, "spa"), "frontend build not present")
    def test_frontend_route_cannot_leave_the_build_folder(self):
        # Audit finding 15: GET /..%2fpackage.json served a file outside frontend/dist.
        response = main.spa("../package.json")
        self.assertEqual(Path(response.path).name, "index.html")
        self.assertEqual(main.spa("settings").headers["cache-control"], "no-cache")  # never serve a stale UI


class GenerationStateTests(unittest.TestCase):
    def _gen(self, gid, title="Original"):
        doc = {"id": gid, "profile_id": "p", "title": title, "favorite": False, "status": "done",
               "chunks": [{"i": 0, "text": "Old text.", "status": "ok"}], "timeline": [], "speed": 1.0}
        db.put_generation(doc)
        return doc

    def test_deleted_narration_is_not_recreated_by_a_running_worker(self):
        # Audit finding 11.
        g = self._gen("delgen1")
        db.delete_generation("delgen1")
        self.assertFalse(db.put_generation_live(dict(g)))
        self.assertIsNone(db.get_generation("delgen1"))

    def test_title_edit_during_render_is_kept(self):
        # Audit finding 26: the worker wrote back the title it loaded before the edit.
        g = self._gen("titlegen1")
        worker_copy = dict(g, chunks=[dict(c) for c in g["chunks"]])
        db.patch_generation_meta("titlegen1", {"title": "My edited title", "favorite": True})
        db.put_generation_live(worker_copy)
        stored = db.get_generation("titlegen1")
        self.assertEqual(stored["title"], "My edited title")
        self.assertTrue(stored["favorite"])

    def test_failed_speed_export_keeps_wav_and_mp3_together(self):
        # Audit finding 31: the new WAV replaced the old one before the MP3 export failed.
        gdir = config.GENERATIONS_DIR / "speedgen1"
        gdir.mkdir(parents=True, exist_ok=True)
        sf.write(gdir / "master.wav", tone(1.0, 0.0), SR, subtype="PCM_16")
        sf.write(gdir / "final.wav", np.zeros(SR, np.float32), SR, subtype="PCM_16")
        before = (gdir / "final.wav").read_bytes()
        with patch.object(pipeline.au, "export_mp3", side_effect=RuntimeError("encoder failed")):
            with self.assertRaises(RuntimeError):
                pipeline.apply_speed(gdir, 1.0)
        self.assertEqual((gdir / "final.wav").read_bytes(), before)
        self.assertFalse((gdir / "final.tmp.wav").exists())

    def test_failed_respeak_restores_text_and_audio(self):
        # Audit finding 27: new subtitles were paired with the old audio after a failed re-speak.
        gid = "respeak1"
        gdir = config.GENERATIONS_DIR / gid
        gdir.mkdir(parents=True, exist_ok=True)
        sf.write(gdir / "chunk_000.wav", tone(1.0, 0.1), SR, subtype="PCM_16")
        original = (gdir / "chunk_000.wav").read_bytes()
        doc = self._gen(gid)
        doc["settings"] = {"max_chunk_chars": 220}
        db.put_generation(doc)

        def failing_run(gen_id, cancel, only_chunks=None):
            sf.write(gdir / "chunk_000.wav", np.zeros(SR, np.float32), SR, subtype="PCM_16")  # new audio written...
            raise RuntimeError("model failed")  # ...then the run fails

        with patch.object(pipeline, "run_generation", side_effect=failing_run):
            with self.assertRaises(RuntimeError):
                pipeline.regenerate_chunk(gid, 0, "Brand new sentence.", lambda: False)
        stored = db.get_generation(gid)
        self.assertEqual(stored["chunks"][0]["text"], "Old text.")
        self.assertEqual((gdir / "chunk_000.wav").read_bytes(), original)

    def test_stopped_respeak_restores_text_and_audio(self):
        # A stop during a re-speak is recorded as a status (not raised); the new text must not stay with old audio.
        gid = "respeakstop1"
        gdir = config.GENERATIONS_DIR / gid
        gdir.mkdir(parents=True, exist_ok=True)
        sf.write(gdir / "chunk_000.wav", tone(1.0, 0.1), SR, subtype="PCM_16")
        original = (gdir / "chunk_000.wav").read_bytes()
        doc = self._gen(gid)
        doc["settings"] = {"max_chunk_chars": 220}
        db.put_generation(doc)

        def stopped_run(gen_id, cancel, only_chunks=None):
            g = db.get_generation(gen_id)
            g["status"] = "cancelled"
            db.put_generation(g)

        with patch.object(pipeline, "run_generation", side_effect=stopped_run):
            pipeline.regenerate_chunk(gid, 0, "Brand new sentence.", lambda: False)
        stored = db.get_generation(gid)
        self.assertEqual(stored["chunks"][0]["text"], "Old text.")
        self.assertEqual((gdir / "chunk_000.wav").read_bytes(), original)

    def test_respeak_rejects_text_that_is_too_long(self):
        # Audit finding 20: a re-speak could exceed the part size and skip the chunk rules.
        gen = {"settings": {"max_chunk_chars": 80}}
        with self.assertRaises(ValueError):
            pipeline.clean_replacement("word " * 60, gen, config.DEFAULT_SETTINGS)
        with self.assertRaises(ValueError):
            pipeline.clean_replacement("   ", gen, config.DEFAULT_SETTINGS)

    def test_fix_remaining_targets_every_part_needing_attention(self):
        gid = "fixgen1"
        doc = self._gen(gid)
        doc["chunks"] = [{"i": 0, "text": "a.", "status": "ok"}, {"i": 1, "text": "b.", "status": "flagged"}, {"i": 2, "text": "c.", "status": "ok"}]
        doc["final_scan"] = {"checked": 3, "fixed": [], "remaining": [2]}
        db.put_generation(doc)
        seen = {}
        with patch.object(pipeline, "run_generation", side_effect=lambda gid_, cancel, only_chunks=None: seen.update(chunks=only_chunks)):
            pipeline.fix_remaining(gid, lambda: False)
        self.assertEqual(seen["chunks"], [1, 2])

    def test_stale_worker_does_not_publish_into_a_deleted_narration(self):
        g = self._gen("stalegen1")
        db.delete_generation("stalegen1")
        self.assertFalse(db.put_generation_live(dict(g)))


class PreviewTests(unittest.TestCase):
    def test_length_capped_preview_is_retried_and_never_published(self):
        for all_capped in (False, True):
            with self.subTest(all_capped=all_capped):
                pid = f"preview-cap-{all_capped}"
                pdir = config.PROFILES_DIR / pid
                pdir.mkdir(parents=True, exist_ok=True)
                profile = {"id": pid, "status": "ready", "variants": [{"index": 0, "file": "ref.wav", "text": "ref"}],
                           "active_variant": 0, "language": "en", "default_engine": "qwen3-tts-1.7b"}
                db.put_profile(profile)
                # Keep a previous preview when every replacement is incomplete.
                old = b"previous preview"
                (pdir / "preview.wav").write_bytes(old)
                reports = [{"stop_reason": "length_limit_reached"}] * 3 if all_capped else [
                    {"stop_reason": "length_limit_reached"}, {"stop_reason": "ended_before_limit"}]
                with patch.object(pipeline.engines, "is_downloaded", return_value=True), \
                     patch.object(pipeline.delivery, "backfill", return_value=False), \
                     patch.object(pipeline, "profile_assets", return_value=(Path("ref.wav"), "ref", np.ones(1), 1.0, 2.0)), \
                     patch.object(pipeline, "noise_limit", return_value=0.1), \
                     patch.object(pipeline.engines, "synthesize", return_value=(tone(1), SR)) as synth, \
                     patch.object(pipeline.engines, "synthesis_report", side_effect=reports), \
                     patch.object(pipeline, "evaluate", side_effect=lambda *a, **k: {"audio": tone(1), "sr": SR, "issues": [], "score": 0.0}), \
                     patch.object(pipeline.studio, "master_chain", side_effect=lambda a, sr: a), \
                     patch.object(pipeline.au, "normalize_lufs", side_effect=lambda a, *args, **kwargs: a):
                    self.assertIs(pipeline.make_preview(pid), not all_capped)
                self.assertEqual(synth.call_count, 3 if all_capped else 2)
                self.assertGreater(synth.call_args_list[1].kwargs['expected_seconds'], synth.call_args_list[0].kwargs['expected_seconds'])
                if all_capped:
                    self.assertEqual((pdir / "preview.wav").read_bytes(), old)

    def test_clean_take_beats_a_failed_take_with_a_better_score(self):
        # Audit finding 06: a cut-off take with score 2.5 was kept over a clean take with score 3.0.
        pid = "previewtest1"
        (config.PROFILES_DIR / pid).mkdir(parents=True, exist_ok=True)
        profile = {"id": pid, "status": "ready", "variants": [{"index": 0, "file": "ref.wav", "text": "ref"}], "active_variant": 0,
                   "language": "en", "default_engine": "qwen3-tts-1.7b"}
        db.put_profile(profile)
        failed = np.full(SR, 0.1, np.float32)
        clean = np.full(SR, 0.2, np.float32)
        evals = [{"audio": failed, "sr": SR, "issues": ["cut_off"], "score": 2.5},
                 {"audio": clean, "sr": SR, "issues": [], "score": 3.0}]
        with patch.object(pipeline.engines, "is_downloaded", return_value=True), \
             patch.object(pipeline.delivery, "backfill", return_value=False), \
             patch.object(pipeline, "profile_assets", return_value=(Path("ref.wav"), "ref", np.ones(1), 1.0, 2.0)), \
             patch.object(pipeline, "noise_limit", return_value=0.1), \
             patch.object(pipeline.engines, "synthesize", return_value=(failed, SR)), \
             patch.object(pipeline, "evaluate", side_effect=evals), \
             patch.object(pipeline.studio, "master_chain", side_effect=lambda a, sr: a), \
             patch.object(pipeline.au, "normalize_lufs", side_effect=lambda a, *args, **kwargs: a):
            self.assertIs(pipeline.make_preview(pid), True)
        saved, _ = au.load(config.PROFILES_DIR / pid / "preview.wav")
        self.assertGreater(float(np.max(np.abs(saved))), 0.15)  # the clean (0.2) take was kept

    def test_preview_made_from_an_old_voice_setting_is_not_published(self):
        # Audit finding 07: changing the voice while a preview was being made published the old one.
        pid = "previewtest2"
        (config.PROFILES_DIR / pid).mkdir(parents=True, exist_ok=True)
        profile = {"id": pid, "status": "ready", "variants": [{"index": 0, "file": "ref.wav", "text": "ref"}, {"index": 1, "file": "ref1.wav", "text": "other"}],
                   "active_variant": 0, "language": "en", "default_engine": "qwen3-tts-1.7b"}
        db.put_profile(profile)

        def change_voice_mid_render(*args, **kwargs):
            p = db.get_profile(pid)
            p["active_variant"] = 1
            db.put_profile(p)
            return {"audio": np.zeros(SR, np.float32), "sr": SR, "issues": [], "score": 1.0}

        with patch.object(pipeline.engines, "is_downloaded", return_value=True), \
             patch.object(pipeline.delivery, "backfill", return_value=False), \
             patch.object(pipeline, "profile_assets", side_effect=lambda p, v=None: (Path("ref.wav"), "ref", np.ones(1), 1.0, 2.0)), \
             patch.object(pipeline, "noise_limit", return_value=0.1), \
             patch.object(pipeline.engines, "synthesize", return_value=(np.zeros(SR, np.float32), SR)), \
             patch.object(pipeline, "evaluate", side_effect=change_voice_mid_render), \
             patch.object(pipeline.studio, "master_chain", side_effect=lambda a, sr: a), \
             patch.object(pipeline.au, "normalize_lufs", side_effect=lambda a, *args, **kwargs: a):
            self.assertEqual(pipeline.make_preview(pid), "stale")
        self.assertIsNone(db.get_profile(pid).get("preview"))


class StopTests(unittest.TestCase):
    def test_stop_during_the_final_take_is_not_marked_done(self):
        # Audit finding 28: a stop pressed during the final take was ignored and the narration was marked done.
        gid = "stopgen1"
        gen = {"id": gid, "profile_id": "p", "engine": "qwen3-tts-1.7b", "language": "en", "status": "queued", "chunks": [{"i": 0, "text": "A.", "status": "pending"}],
               "timeline": [{"i": 0, "start": 0, "end": 1}], "settings": dict(config.DEFAULT_SETTINGS)}
        db.put_generation(gen)
        flag = {"stop": False}

        def render(g, ci, *args, **kwargs):
            g["chunks"][ci].update(status="ok", issues=[])
            flag["stop"] = True  # the stop arrives during the final take

        with patch.object(pipeline.delivery, "backfill", return_value=False), \
             patch.object(pipeline.db, "get_profile", return_value={"id": "p"}), \
             patch.object(pipeline.engines, "is_downloaded", return_value=True), patch.object(pipeline.engines, "load"), \
             patch.object(pipeline, "render_chunk", side_effect=render), patch.object(pipeline, "assemble"), \
             patch.object(pipeline, "final_scan", return_value=[]):
            pipeline.run_generation(gid, lambda: flag["stop"])
        self.assertEqual(db.get_generation(gid)["status"], "cancelled")


class QueueTests(unittest.TestCase):
    """The queue shows what is waiting, in order, with the narration's real progress."""

    def setUp(self):
        from app import worker

        self.worker = worker
        self.created = []
        for gid, title, chunks in (("qgenA", "First narration", [{"i": 0, "status": "ok"}, {"i": 1, "status": "pending"}]),
                                   ("qgenB", "Second narration", [{"i": 0, "status": "pending"}])):
            db.put_generation({"id": gid, "profile_id": "p", "engine": "qwen3-tts-1.7b", "language": "en", "title": title,
                               "status": "queued", "message": "Queued", "chunks": chunks, "timeline": [], "settings": {}})
        for jid, gid, status, created in (("qjobA", "qgenA", "running", 1.0), ("qjobB", "qgenB", "queued", 2.0)):
            job = {"id": jid, "kind": "generate", "ref": gid, "label": "Generating", "status": status, "progress": 0.0,
                   "message": "Queued", "error": None, "created": created, "cancel": False}
            self.worker.jobs[jid] = job
            self.created.append(jid)

    def tearDown(self):
        for jid in self.created:
            self.worker.jobs.pop(jid, None)

    def test_status_lists_the_queue_with_real_progress(self):
        s = main.status()
        self.assertEqual([q["gid"] for q in s["queue"]], ["qgenA", "qgenB"])
        self.assertEqual(s["running"]["gid"], "qgenA")
        self.assertAlmostEqual(s["running"]["progress"], 0.5)  # 1 of 2 parts, not the job's 0%
        self.assertEqual(s["running"]["title"], "First narration")
        self.assertEqual(s["queued"], 1)

    def test_queued_narration_says_what_it_waits_for(self):
        g = main.get_generation("qgenB")
        self.assertEqual(g["queue"], {"ahead": 1, "waiting_for": ["First narration"]})

    def test_stop_on_a_queued_narration_takes_effect_immediately(self):
        main.cancel_generation("qgenB")
        self.assertEqual(db.get_generation("qgenB")["status"], "cancelled")
        self.assertEqual(self.worker.jobs["qjobB"]["status"], "cancelled")
        self.assertNotIn("qgenB", [q.get("gid") for q in main.status()["queue"]])


class DuplicateNarrationTests(unittest.TestCase):
    """The same script for the same voice is refused while it is queued or rendering."""

    SCRIPT = "Hello there. This is the first narration."

    def setUp(self):
        from fastapi import HTTPException  # noqa: F401  (used by the assertions)

        self.worker = main.worker
        profile = {"id": "dupvoice", "name": "Dup voice", "status": "ready", "variants": [{"index": 0, "file": "r.wav", "text": "r"}],
                   "active_variant": 0, "language": "en", "default_engine": "qwen3-tts-1.7b"}
        db.put_profile(profile)
        db.put_profile(dict(profile, id="dupvoice2", name="Other voice"))
        db.put_generation({"id": "dupgen1", "profile_id": "dupvoice", "engine": "qwen3-tts-1.7b", "language": "en",
                           "title": "First copy", "status": "queued", "message": "Queued", "text": self.SCRIPT,
                           "cleaned_text": self.SCRIPT, "chunks": [{"i": 0, "text": self.SCRIPT, "status": "pending"}],
                           "timeline": [], "settings": {}})
        self.job = {"id": "dupjob1", "kind": "generate", "ref": "dupgen1", "label": "Generating", "status": "queued", "progress": 0.0,
                    "message": "Queued", "error": None, "created": 1.0, "cancel": False}
        self.worker.jobs["dupjob1"] = self.job

    def tearDown(self):
        self.worker.jobs.pop("dupjob1", None)
        main._dup_notices.pop("dupgen1", None)

    def _post(self, text, voice="dupvoice"):
        return main.create_generation(main.GenRequest(profile_id=voice, text=text, engine="qwen3-tts-1.7b"))

    def test_identical_script_for_the_same_voice_is_refused_while_queued(self):
        from fastapi import HTTPException

        with self.assertRaises(HTTPException) as cm:
            self._post(self.SCRIPT)
        self.assertEqual(cm.exception.status_code, 409)
        self.assertEqual(cm.exception.detail["duplicate_of"], "dupgen1")
        self.assertIn("already in the queue", cm.exception.detail["message"].lower())
        self.assertIn("waiting its turn", cm.exception.detail["message"])

    def test_same_script_with_different_spacing_is_still_a_duplicate(self):
        from fastapi import HTTPException

        with self.assertRaises(HTTPException) as cm:
            self._post("Hello   there.  This is the first   narration.")
        self.assertEqual(cm.exception.detail["duplicate_of"], "dupgen1")

    def test_a_paragraph_break_is_a_different_narration(self):
        # A blank line changes the pause length, so the script really is different.
        with patch.object(main.pipeline, "create_generation", return_value={"id": "dupnew3"}), \
             patch.object(main, "_submit_generation", return_value={"id": "dupjob4"}):
            self.assertEqual(self._post("Hello there.\n\nThis is the first narration.")["id"], "dupnew3")

    def test_a_running_narration_is_refused_too(self):
        from fastapi import HTTPException

        self.job["status"] = "running"
        with self.assertRaises(HTTPException) as cm:
            self._post(self.SCRIPT)
        self.assertIn("rendering now", cm.exception.detail["message"])

    def test_same_script_for_another_voice_is_allowed(self):
        with patch.object(main.pipeline, "create_generation", return_value={"id": "dupnew1"}), \
             patch.object(main, "_submit_generation", return_value={"id": "dupjob2"}) as submit:
            out = self._post(self.SCRIPT, voice="dupvoice2")
        submit.assert_called_once_with("dupnew1")
        self.assertEqual(out["id"], "dupnew1")

    def test_a_finished_narration_does_not_block_a_new_one(self):
        self.worker.jobs.pop("dupjob1")  # no longer queued or rendering
        with patch.object(main.pipeline, "create_generation", return_value={"id": "dupnew2"}), \
             patch.object(main, "_submit_generation", return_value={"id": "dupjob3"}):
            self.assertEqual(self._post(self.SCRIPT)["id"], "dupnew2")

    def test_the_queue_and_the_narration_show_the_refused_copy(self):
        from fastapi import HTTPException

        for _ in range(2):
            with self.assertRaises(HTTPException):
                self._post(self.SCRIPT)
        s = main.status()
        self.assertEqual(s["duplicates"][0]["gid"], "dupgen1")
        self.assertEqual(s["duplicates"][0]["count"], 2)
        self.assertEqual(main.get_generation("dupgen1")["duplicate_notice"]["count"], 2)


class TranscriptRetryTests(unittest.TestCase):
    def test_retry_requeues_an_interrupted_transcript_with_its_language(self):
        doc = tools.new_transcript("talk.wav")
        tid = doc["id"]
        (config.TRANSCRIPTS_DIR / tid / doc["file"]).write_bytes(b"RIFF")
        doc.update(status="error", error="Transcription was interrupted. Press Retry.", language_hint="en")
        tools.save_transcript_doc(doc)
        with patch.object(main.worker, "submit") as submit:
            out = main.retry_transcript(tid)
        self.assertEqual(out["status"], "queued")
        self.assertEqual(tools.transcript_doc(tid)["status"], "queued")
        submit.assert_called_once()

    def test_retry_refuses_when_the_file_is_gone(self):
        from fastapi import HTTPException

        doc = tools.new_transcript("lost.wav")
        doc.update(status="error", error="x")
        tools.save_transcript_doc(doc)
        with self.assertRaises(HTTPException):
            main.retry_transcript(doc["id"])


class ScriptArtifactTests(unittest.TestCase):
    def test_web_entities_are_not_read_aloud(self):
        # Scripts pasted from a web page carried "&39;" for an apostrophe; the voice said "39".
        self.assertEqual(textproc.clean_text("Marika&39;s reign &amp; Co&nbsp;here, &quot;quoted&quot;."),
                         "Marika's reign & Co here, \"quoted\".")
        self.assertEqual(textproc.fix_entities("AT&T and 2&3"), "AT&T and 2&3")

    def test_names_split_across_words_are_not_a_missing_word(self):
        # "Scadutree" heard as "Skadu tree" and "Malenia" heard as "Melania" were flagged as missing words.
        self.assertFalse(pipeline._final_word_missing("nestled in the shadow of the Scadutree.", "nestled in the shadow of the Skadu tree."))
        self.assertFalse(pipeline._final_word_missing("two twin demigods: Miquella and Malenia.", "two twin demigods, Mykela and Melania."))
        self.assertTrue(pipeline._final_word_missing("We walked before returning home.", "We walked before returning."))


class FinalCheckTests(unittest.TestCase):
    def test_a_natural_take_passes_the_final_check(self):
        # The finished file is mastered (compressed), which lifted quiet endings; the take decides.
        gid = "finalnatural1"
        gdir = config.GENERATIONS_DIR / gid
        gdir.mkdir(parents=True, exist_ok=True)
        x = np.concatenate([tone(1.5, 0.3), np.zeros(int(0.3 * SR), np.float32)])
        sf.write(gdir / "raw.wav", x, SR, subtype="FLOAT")
        gen = {"id": gid, "chunks": [{"i": 0, "raw_file": "raw.wav", "issues": [], "ending": {"last_word_end": 1.5}}],
               "timeline": [{"i": 0, "start": 0.0, "end": len(x) / SR}]}
        self.assertEqual(pipeline.final_scan(gen), [])

    def test_an_abrupt_take_still_fails_the_final_check(self):
        gid = "finalabrupt1"
        gdir = config.GENERATIONS_DIR / gid
        gdir.mkdir(parents=True, exist_ok=True)
        x = np.concatenate([tone(1.5, 0.0), np.zeros(int(0.3 * SR), np.float32)])
        sf.write(gdir / "raw.wav", x, SR, subtype="FLOAT")
        gen = {"id": gid, "chunks": [{"i": 0, "raw_file": "raw.wav", "issues": [], "ending": {"last_word_end": 1.5}}],
               "timeline": [{"i": 0, "start": 0.0, "end": len(x) / SR}]}
        self.assertEqual(pipeline.final_scan(gen), [0])

    def test_fix_targets_include_web_artifacts_and_flagged_parts_only(self):
        gen = {"chunks": [{"i": 0, "text": "Clean.", "status": "ok"},
                          {"i": 1, "text": "Marika&39;s.", "status": "ok"},
                          {"i": 2, "text": "Flagged.", "status": "flagged"}],
               "final_scan": {"remaining": [3]}}
        self.assertEqual(pipeline.fix_targets(gen), [1, 2, 3])

    def test_fix_remaining_corrects_the_stored_script(self):
        gid = "fixentity1"
        doc = {"id": gid, "profile_id": "p", "engine": "qwen3-tts-1.7b", "language": "en", "title": "t", "status": "done",
               "text": "Marika&39;s.", "cleaned_text": "Marika&39;s.", "chunks": [{"i": 0, "text": "Marika&39;s.", "status": "ok"}],
               "timeline": [], "settings": {}}
        db.put_generation(doc)
        seen = {}
        with patch.object(pipeline, "run_generation", side_effect=lambda g, c, only_chunks=None: seen.update(chunks=only_chunks)):
            pipeline.fix_remaining(gid, lambda: False)
        stored = db.get_generation(gid)
        self.assertEqual(stored["chunks"][0]["text"], "Marika's.")
        self.assertEqual(seen["chunks"], [0])


class IdleReleaseTests(unittest.TestCase):
    def test_models_are_released_after_idle_and_only_once(self):
        import time as _t

        from app import worker

        calls = []
        old_after = worker.idle_after
        worker.on_idle(lambda: calls.append(_t.time()), 0.3)
        try:
            done = []
            worker.submit("test", lambda progress, cancel: done.append(1), label="idle test")
            for _ in range(40):
                if calls:
                    break
                _t.sleep(0.1)
            self.assertEqual(done, [1])
            self.assertEqual(len(calls), 1, "released once after the job")
            _t.sleep(0.8)
            self.assertEqual(len(calls), 1, "not released again while still idle")
        finally:
            worker._idle_hooks.pop()
            worker.idle_after = old_after


class IntegrationsTests(unittest.TestCase):
    def test_reports_each_app_from_its_own_config(self):
        import json as _json

        home = Path(tempfile.mkdtemp())
        with patch.dict(os.environ, {"HOME": str(home)}):
            self.assertEqual([a["connected"] for a in main.integrations()["apps"]], [False, False])
            desktop = home / "Library" / "Application Support" / "Claude"
            desktop.mkdir(parents=True)
            (desktop / "claude_desktop_config.json").write_text(_json.dumps({"mcpServers": {"voiceforge": {}}}))
            (home / ".claude.json").write_text("not json")  # a broken file reads as not set up
            r = main.integrations()
        self.assertEqual([a["connected"] for a in r["apps"]], [True, False])
        self.assertIn("claude mcp add --scope user voiceforge", r["apps"][1]["setup"])


class RecordingScopeTests(unittest.TestCase):
    """Re-speak and Fix remaining errors record only their own parts; a first render may repair others."""

    def _narration(self, gid, statuses=("ok", "ok", "ok")):
        gdir = config.GENERATIONS_DIR / gid
        gdir.mkdir(parents=True, exist_ok=True)
        db.put_profile({"id": "scopevoice", "name": "Scope", "status": "ready"})
        chunks = []
        for i, st in enumerate(statuses):
            (gdir / f"chunk_{i:03d}.wav").write_bytes(b"old")
            chunks.append({"i": i, "text": f"Part {i}.", "status": st, "file": f"chunk_{i:03d}.wav", "issues": []})
        db.put_generation({"id": gid, "profile_id": "scopevoice", "engine": "qwen3-tts-1.7b", "language": "en", "title": "t",
                           "status": "done", "message": "Done", "text": "x", "cleaned_text": "x", "chunks": chunks,
                           "timeline": [{"i": i, "start": i, "end": i + 1} for i in range(len(statuses))], "settings": {}})
        return gdir

    def _run(self, gdir, action, bad_parts, on_render=lambda ci: None):
        recorded = []

        def render(gen, ci, *args, **kwargs):
            recorded.append(ci)
            (gdir / f"chunk_{ci:03d}.wav").write_bytes(b"new")
            gen["chunks"][ci].update(status="ok", issues=[])
            on_render(ci)

        def scan(gen):
            for c in gen["chunks"]:
                c["final_issues"] = ["cut_off"] if c["i"] in bad_parts else []
            return list(bad_parts)

        with patch.object(pipeline.delivery, "backfill", return_value=False), \
             patch.object(pipeline.engines, "is_downloaded", return_value=True), patch.object(pipeline.engines, "load"), \
             patch.object(pipeline, "render_chunk", side_effect=render), patch.object(pipeline, "assemble"), \
             patch.object(pipeline, "final_scan", side_effect=scan):
            action()
        return recorded

    def test_respeak_records_only_the_chosen_part_and_flags_others(self):
        gdir = self._narration("scope1")
        recorded = self._run(gdir, lambda: pipeline.regenerate_chunk("scope1", 0, None, lambda: False), [1])
        self.assertEqual(recorded, [0])
        self.assertEqual((gdir / "chunk_001.wav").read_bytes(), b"old")
        stored = db.get_generation("scope1")
        self.assertEqual(stored["chunks"][1]["status"], "flagged")
        self.assertEqual(stored["final_scan"]["remaining"], [1])

    def test_stopped_respeak_leaves_other_parts_untouched(self):
        # The bug: a stop after the final check re-recorded part 1 restored the old mix but kept part 1's new audio.
        gdir = self._narration("scope2")
        stop = {"now": False}

        def on_render(ci):
            stop["now"] = ci == 1 or stop["now"]

        self._run(gdir, lambda: pipeline.regenerate_chunk("scope2", 0, "New words.", lambda: stop["now"]), [1], on_render)
        self.assertEqual((gdir / "chunk_001.wav").read_bytes(), b"old")

    def test_fix_remaining_records_only_the_parts_it_counted(self):
        gdir = self._narration("scope3", ("ok", "flagged", "ok"))
        recorded = self._run(gdir, lambda: pipeline.fix_remaining("scope3", lambda: False), [2])
        self.assertEqual(recorded, [1])
        self.assertEqual((gdir / "chunk_002.wav").read_bytes(), b"old")
        self.assertEqual(db.get_generation("scope3")["chunks"][2]["status"], "flagged")

    def test_a_chosen_part_failing_the_final_check_gets_one_more_recording(self):
        gdir = self._narration("scope4")
        recorded = self._run(gdir, lambda: pipeline.regenerate_chunk("scope4", 0, None, lambda: False), [0])
        self.assertEqual(recorded, [0, 0])

    def test_first_render_still_repairs_parts_the_final_check_finds(self):
        gdir = self._narration("scope5", ("pending", "pending", "pending"))
        recorded = self._run(gdir, lambda: pipeline.run_generation("scope5", lambda: False), [1])
        self.assertEqual(recorded, [0, 1, 2, 1])


class LeadBurstTests(unittest.TestCase):
    """A reference clip cut before its transcript ends makes takes open by finishing that word ("links": "xs")."""

    def _hiss(self, seconds, amp=0.2):
        rng = np.random.default_rng(3)
        from scipy.signal import butter, sosfiltfilt
        return sosfiltfilt(butter(4, [4000, 10000], btype="band", fs=SR, output="sos"), rng.standard_normal(int(seconds * SR))).astype(np.float32) * amp * 2

    def _speech(self, seconds=2.0):
        t = np.arange(int(seconds * SR)) / SR
        return (0.3 * np.sin(2 * np.pi * 150 * t) * (0.6 + 0.4 * np.sin(2 * np.pi * 4 * t))).astype(np.float32)

    def _take(self, *parts):
        return np.concatenate([np.asarray(p, np.float32) for p in parts])

    def _gap(self, s):
        return np.zeros(int(s * SR), np.float32)

    def test_hiss_then_a_pause_before_the_first_word_is_a_burst(self):
        self.assertIsNotNone(au.lead_burst(self._take(self._hiss(.18), self._gap(.5), self._speech()), SR))

    def test_a_take_starting_with_speech_is_clean(self):
        self.assertIsNone(au.lead_burst(self._take(self._gap(.05), self._speech()), SR))

    def test_a_real_s_sound_running_into_its_word_is_clean(self):
        # "standard": the hiss is only a few tens of ms from the rest of the word.
        self.assertIsNone(au.lead_burst(self._take(self._hiss(.1), self._gap(.03), self._speech()), SR))

    def test_a_short_voiced_first_word_before_a_pause_is_clean(self):
        # "The ... story" is a pause, not hiss: judged by odd_pause, not this check.
        self.assertIsNone(au.lead_burst(self._take(self._speech(.15), self._gap(.5), self._speech()), SR))

    def test_a_burst_fails_the_take(self):
        take = self._take(self._hiss(.18), self._gap(.5), self._speech(3.0), self._gap(.3))
        tr = {"text": "Then a janitor walked over.", "segments": [{"words": [{"word": "over.", "start": 3.0, "end": 3.6}]}]}
        with patch.object(pipeline.ml, "transcribe", return_value=tr), \
             patch.object(pipeline.ml, "speaker_embedding", return_value=np.ones(1)), \
             patch.object(pipeline.ml, "cosine", return_value=1.0), \
             patch.object(pipeline.ml, "normalize_for_wer", side_effect=simple_norm):
            ev = pipeline.evaluate(take, SR, "Then a janitor walked over.", np.ones(1), 1.0, 2.5, config.DEFAULT_SETTINGS, "en")
        self.assertIn("lead_burst", ev["issues"])
        self.assertIn("lead_burst", pipeline.HARD_ISSUES)

    def test_only_a_burst_of_40_to_300_ms_counts(self):
        def burst(ms):
            return au.lead_burst(self._take(self._hiss(ms / 1000), self._gap(.5), self._speech()), SR)
        self.assertIsNone(burst(20), "a click shorter than a consonant is not a burst")
        self.assertIsNotNone(burst(60))
        self.assertIsNotNone(burst(280))
        self.assertIsNone(burst(450), "a long hiss is not a leftover word ending")

    def test_final_scan_reports_a_burst_in_the_saved_take(self):
        take = self._take(self._hiss(.18), self._gap(.5), tone(3.0), self._gap(.3))
        self.assertIsNone(au.lead_burst(tone(3.0), SR))
        gdir = config.GENERATIONS_DIR / "lead-burst-scan"
        gdir.mkdir()
        sf.write(gdir / "raw.wav", take, SR, subtype="FLOAT")
        sf.write(gdir / "final.wav", take, SR)
        end = .68 + 3.0
        gen = {"id": gdir.name, "chunks": [{"i": 0, "text": "Hello.", "status": "ok", "raw_file": "raw.wav", "issues": [],
                                            "ending": au.end_analysis(take, SR, end)}],
               "timeline": [{"i": 0, "start": 0, "end": len(take) / SR}]}
        self.assertEqual(pipeline.final_scan(gen), [0])
        self.assertEqual(gen["chunks"][0]["final_issues"], ["lead_burst"], "the burst must be the reason, not the ending")


class NeedsAListenTests(unittest.TestCase):
    """A finished narration with doubtful parts must say so, with part, text and reason, wherever it is read."""

    def _gen(self, gid, chunks, remaining=()):
        g = {"id": gid, "created": 1.0, "status": "done", "profile_id": "p", "profile_name": "P", "text": "x",
             "chunks": chunks, "final_scan": {"checked": len(chunks), "fixed": [], "remaining": list(remaining)}}
        db.put_generation(g)
        return g

    def test_clean_narration(self):
        self._gen("listen-clean", [{"i": 0, "text": "Fine.", "status": "ok", "issues": []}])
        g = main.get_generation("listen-clean")
        self.assertEqual((g["outcome"], g["review"]), ("clean", []))

    def test_doubtful_parts_carry_part_text_and_reason(self):
        self._gen("listen-bad", [
            {"i": 0, "text": "Fine.", "status": "ok", "issues": []},
            {"i": 1, "text": "Rosa held out her hand.", "status": "flagged", "issues": ["cut_off"], "final_issues": ["lead_burst"]},
            {"i": 2, "text": "Late check.", "status": "ok", "issues": []},
            {"i": 3, "text": "It&#39;s here.", "status": "ok", "issues": []}], remaining=[2])
        g = main.get_generation("listen-bad")
        self.assertEqual(g["outcome"], "needs_listen")
        self.assertEqual([r["part"] for r in g["review"]], [2, 3, 4], "numbered from 1, matching what the Fix button re-records")
        self.assertEqual(g["review"][0]["text"], "Rosa held out her hand.")
        self.assertEqual(g["review"][0]["reasons"], ["Cut-off ending", "Stray sound at the start"])
        self.assertEqual(g["review"][1]["reasons"], [pipeline.ISSUE_LABEL["final_check"]])
        self.assertEqual(g["review"][2]["reasons"], [pipeline.ISSUE_LABEL["web_artifact"]])
        self.assertEqual([r["part"] - 1 for r in g["review"]], pipeline.fix_targets(g))
        listed = next(x for x in main.list_generations() if x["id"] == "listen-bad")
        self.assertEqual((listed["outcome"], listed["review_count"]), ("needs_listen", 3))

    def test_unfinished_narration_has_no_outcome(self):
        g = self._gen("listen-running", [{"i": 0, "text": "Fine.", "status": "flagged", "issues": ["cut_off"]}])
        db.put_generation({**g, "status": "running"})
        self.assertIsNone(main.get_generation("listen-running")["outcome"])

    def test_reason_labels_match_the_app(self):
        import re
        ts = (Path(__file__).resolve().parents[2] / "frontend/src/api.ts").read_text()
        block = ts[ts.index("export const ISSUE_LABEL"):]
        block = block[:block.index("};")]
        self.assertEqual(dict(re.findall(r'(\w+): "([^"]+)"', block)), pipeline.ISSUE_LABEL)


class ClipEndTests(unittest.TestCase):
    """A reference clip must not end in a stop consonant's closure, before the word's last sound."""
    SPEECH, QUIET = -20.0, -52.0

    def _db10(self, *runs):
        # Long quiet lead so pause_threshold sits just above the room's floor, as in real recordings.
        return np.concatenate([np.full(300, self.QUIET), np.full(500, self.SPEECH), *[np.full(n, v) for n, v in runs], np.full(300, self.QUIET)])

    def test_links_the_clip_ends_after_the_final_sound(self):
        # 60 ms closure, 170 ms "ks", then a real 400 ms pause; Whisper says the next word starts at 8.7 s.
        db = self._db10((6, self.QUIET), (17, -30.0), (40, self.QUIET), (200, self.SPEECH))
        word_end = 8.0 + .06  # Whisper's word end falls inside the "ks", as in the real recording
        snapped = reference.snap_window(db, 3.0, word_end, 8.7, len(db) * .01)
        self.assertGreaterEqual(snapped[1], 8.23, "the clip must include the whole final sound")

    def test_a_short_gap_before_the_next_word_is_still_a_clean_end(self):
        db = self._db10((8, self.QUIET), (200, self.SPEECH))
        snapped = reference.snap_window(db, 3.0, 8.0, 8.08, len(db) * .01)
        self.assertLess(snapped[1], 8.08, "the clip must not run into the next word")

    def test_a_short_next_word_after_a_short_gap_is_not_a_final_sound(self):
        db = self._db10((8, self.QUIET), (15, self.SPEECH), (60, self.QUIET), (200, self.SPEECH))
        snapped = reference.snap_window(db, 3.0, 8.0, 8.08, len(db) * .01)
        self.assertLess(snapped[1], 8.08)

    def test_no_clean_pause_after_the_final_sound_means_the_window_is_unused(self):
        # The only pause after the "ks" starts past the point Whisper puts the next word, so no safe cut exists.
        db = self._db10((6, self.QUIET), (17, -30.0), (6, self.QUIET), (200, self.SPEECH))
        self.assertIsNone(reference.snap_window(db, 3.0, 8.0, 8.20, len(db) * .01))

    def test_a_short_pause_at_the_end_of_the_recording_is_a_clean_end(self):
        db = np.concatenate([np.full(300, self.QUIET), np.full(500, self.SPEECH), np.full(10, self.QUIET)])
        self.assertIsNotNone(reference.snap_window(db, 3.0, 8.0, None, len(db) * .01))

    def test_a_faint_breath_after_the_dip_is_not_a_final_sound(self):
        quiet = -80.0  # a clean recording, so a sound 40 dB under speech still counts as sound
        db = np.concatenate([np.full(300, quiet), np.full(500, self.SPEECH), np.full(6, quiet), np.full(17, -60.0), np.full(40, quiet), np.full(200, self.SPEECH), np.full(300, quiet)])
        snapped = reference.snap_window(db, 3.0, 8.0, 8.7, len(db) * .01)
        self.assertLess(snapped[1], 8.07, "a breath must not push the clip's end later")

    def test_a_long_gap_then_a_short_sound_is_a_real_pause(self):
        db = self._db10((30, self.QUIET), (17, -30.0), (40, self.QUIET), (200, self.SPEECH))
        snapped = reference.snap_window(db, 3.0, 8.0, 9.0, len(db) * .01)
        self.assertLess(snapped[1], 8.3)


class ReferencePitchTests(unittest.TestCase):
    def test_pitch_is_remeasured_after_the_reference_is_replaced(self):
        # Audit finding 21: a re-cut reference kept the old pitch measurement.
        ref = config.DATA_DIR / "pitch_ref.wav"
        sf.write(ref, tone(1.0, 0.0), SR, subtype="PCM_16")
        values = iter([1.0, 4.0])
        pipeline._pitch_cache.clear()
        with patch.object(pipeline.delivery, "prosody", side_effect=lambda a, sr: {"pitch_st": next(values)}):
            self.assertEqual(pipeline._ref_pitch(ref), 1.0)
            sf.write(ref, tone(1.5, 0.0), SR, subtype="PCM_16")  # replaced in place
            self.assertEqual(pipeline._ref_pitch(ref), 4.0)


if __name__ == "__main__":
    unittest.main()
