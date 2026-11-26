"""Review loop: "Sounds fine", Take 1 vs Take 2, soft delete with Undo, persisted source, search. Temp library only."""
from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
_data = tempfile.TemporaryDirectory(prefix="taketwo-review-tests-")
os.environ.setdefault("TAKETWO_DATA", _data.name)

from fastapi.testclient import TestClient  # noqa: E402
from app import config, db, main, pipeline  # noqa: E402

if not config.DATA_DIR.resolve().is_relative_to(Path(tempfile.gettempdir()).resolve()):
    raise unittest.SkipTest("TAKETWO_DATA points at a real library; these tests write and delete files")

LOCAL = "http://127.0.0.1:8766"
H = {"Origin": LOCAL}


def make_gen(gid: str, flagged=(1,), status="done") -> dict:
    gdir = config.GENERATIONS_DIR / gid
    gdir.mkdir(parents=True, exist_ok=True)
    chunks = []
    for i in range(3):
        (gdir / f"chunk_{i:03d}.wav").write_bytes(b"take1-%d" % i)
        chunks.append({"i": i, "text": f"Part {i}.", "pause_after": "sentence", "file": f"chunk_{i:03d}.wav",
                       "status": "flagged" if i in flagged else "ok", "issues": ["cut_off"] if i in flagged else [],
                       "heard": f"Part {i}", "pitch_hz": 180.0, "wer": 0.0, "sim": 0.9, "dur": 1.0})
    g = {"id": gid, "profile_id": "p1", "profile_name": "Ava", "engine": "x", "language": "en", "status": status,
         "text": "Part 0. Part 1. Part 2.", "cleaned_text": "Part 0. Part 1. Part 2.", "title": "Parts", "chunks": chunks,
         "settings": {}, "timeline": [{"i": i, "start": i * 1.5, "end": i * 1.5 + 1.0} for i in range(3)]}
    pipeline.summarize(g)
    return db.put_generation(g)


class SoundsFineTests(unittest.TestCase):
    def setUp(self):
        self.c = TestClient(main.app, base_url=LOCAL)
        make_gen("acc1")

    def test_review_items_carry_where_and_what_was_measured(self):
        r = self.c.get("/api/generations/acc1").json()
        self.assertEqual(r["outcome"], "needs_listen")
        item = r["review"][0]
        self.assertEqual((item["part"], item["start"], item["end"], item["pitch_hz"], item["checked_by"]), (2, 1.5, 2.5, 180.0, ["take check"]))

    def test_sounds_fine_clears_the_part_and_undo_brings_it_back(self):
        r = self.c.post("/api/generations/acc1/chunks/1/accept", headers=H).json()
        self.assertEqual((r["outcome"], r["review"], r["status"]), ("clean", [], "done"))
        self.assertEqual(r["chunks"][1]["accepted"]["by"], "person")
        self.assertIn("sounding fine", r["message"])
        r = self.c.delete("/api/generations/acc1/chunks/1/accept", headers=H).json()
        self.assertEqual(r["outcome"], "needs_listen")
        self.assertEqual([x["part"] for x in r["review"]], [2])

    def test_accepted_parts_are_not_re_recorded_by_fix_remaining(self):
        self.c.post("/api/generations/acc1/chunks/1/accept", headers=H)
        self.assertEqual(pipeline.fix_targets(db.get_generation("acc1")), [])

    def test_a_busy_narration_cannot_be_changed(self):
        make_gen("acc2", status="running")
        self.assertEqual(self.c.post("/api/generations/acc2/chunks/1/accept", headers=H).status_code, 409)


class TakeTests(unittest.TestCase):
    def setUp(self):
        self.c = TestClient(main.app, base_url=LOCAL)
        make_gen("tk1")

    def respeak(self):
        def fake_run(gid, cancel, only_chunks=None):  # the new take: different audio, passes the checks
            g = db.get_generation(gid)
            ci = only_chunks[0]
            (config.GENERATIONS_DIR / gid / f"chunk_{ci:03d}.wav").write_bytes(b"take2")
            g["chunks"][ci].update(status="ok", issues=[], heard="Part 1 again", accepted=None)
            g["status"] = "done"
            pipeline.summarize(g)
            db.put_generation_live(g)
        with patch.object(pipeline, "run_generation", fake_run):
            pipeline.regenerate_chunk("tk1", 1, None, lambda: False)

    def test_respeak_keeps_take_1_playable_next_to_take_2(self):
        self.respeak()
        ch = db.get_generation("tk1")["chunks"][1]
        self.assertEqual(ch["take"], 2)
        self.assertEqual([t["take"] for t in ch["takes"]], [1])
        self.assertEqual(self.c.get("/api/generations/tk1/chunks/1/takes/1/audio").content, b"take1-1")
        self.assertEqual(self.c.get("/api/generations/tk1/chunks/1/takes/2/audio").content, b"take2")

    def test_keeping_take_1_swaps_audio_and_measurements_and_remixes(self):
        self.respeak()
        with patch.object(pipeline, "assemble") as remix:
            pipeline.select_take("tk1", 1, 1)
        remix.assert_called_once()
        g = db.get_generation("tk1")
        ch = g["chunks"][1]
        self.assertEqual((ch["take"], ch["status"], ch["heard"]), (1, "flagged", "Part 1"))
        self.assertEqual((config.GENERATIONS_DIR / "tk1/chunk_001.wav").read_bytes(), b"take1-1")
        self.assertEqual([t["take"] for t in ch["takes"]], [2])  # Take 2 can be chosen again
        self.assertEqual(self.c.get("/api/generations/tk1/chunks/1/takes/2/audio").content, b"take2")
        self.assertEqual(pipeline.outcome(g), "needs_listen")

    def test_a_failed_respeak_leaves_no_orphan_take(self):
        with patch.object(pipeline, "run_generation", side_effect=RuntimeError("boom")):
            with self.assertRaises(RuntimeError):
                pipeline.regenerate_chunk("tk1", 1, None, lambda: False)
        ch = db.get_generation("tk1")["chunks"][1]
        self.assertNotIn("takes", ch)
        self.assertFalse((config.GENERATIONS_DIR / "tk1/takes/001-1").exists())

    def test_unknown_take_is_refused(self):
        self.assertEqual(self.c.post("/api/generations/tk1/chunks/1/select-take", json={"take": 9}, headers=H).status_code, 404)


class SoftDeleteAndSourceTests(unittest.TestCase):
    def setUp(self):
        self.c = TestClient(main.app, base_url=LOCAL)

    def test_delete_hides_then_undo_restores_with_files(self):
        make_gen("del1")
        self.assertTrue(self.c.delete("/api/generations/del1", headers=H).json()["ok"])
        self.assertEqual(self.c.get("/api/generations/del1").status_code, 404)
        self.assertNotIn("del1", [g["id"] for g in self.c.get("/api/generations").json()])
        self.assertTrue((config.GENERATIONS_DIR / "del1/chunk_000.wav").exists())
        self.c.post("/api/generations/del1/restore", headers=H)
        self.assertEqual(self.c.get("/api/generations/del1").status_code, 200)

    def test_a_render_saving_after_delete_keeps_it_deleted(self):
        g = make_gen("del2")
        db.set_generation_deleted("del2", True)
        db.put_generation_live(g)
        self.assertIn("deleted_at", db.get_generation("del2"))

    def test_old_deletes_are_purged(self):
        make_gen("del3")
        db.set_generation_deleted("del3", True)
        self.assertIn("del3", db.purge_deleted_generations(older_than_s=-1))
        self.assertIsNone(db.get_generation("del3"))

    def test_source_and_title_are_saved_on_the_narration(self):
        prof = {"id": "p1", "name": "Ava", "status": "ready", "language": "en"}
        db.put_profile(prof)
        with patch.object(main, "_spec_or_404"), patch.object(main, "_submit_generation", return_value=None):
            for client, want in (("mcp", "mcp"), ("app", "app"), (None, "api")):
                h = {**H, **({"X-TakeTwo-Client": client} if client else {})}
                r = self.c.post("/api/generations", json={"profile_id": "p1", "text": f"Hello from {want}.", "title": f"T {want}"}, headers=h).json()
                g = db.get_generation(r["id"])
                self.assertEqual((g["source"], g["title"]), (want, f"T {want}"))

    def test_search_finds_by_words_voice_status_and_same_script(self):
        make_gen("srch1")
        r = self.c.get("/api/generations/search", params={"q": "part 1", "voice": "ava", "status": "needs_review"}).json()
        self.assertIn("srch1", [x["id"] for x in r])
        r = self.c.get("/api/generations/search", params={"same_script": "Part 0. Part 1. Part 2."}).json()
        self.assertIn("srch1", [x["id"] for x in r])
        self.assertEqual(self.c.get("/api/generations/search", params={"q": "nothing like this"}).json(), [])


class ReviewFixesTests(unittest.TestCase):
    """Regressions from the 2026-11-27 code review."""

    def setUp(self):
        self.c = TestClient(main.app, base_url=LOCAL)

    def respeak(self, gid):
        def fake_run(g_id, cancel, only_chunks=None):
            g = db.get_generation(g_id)
            (config.GENERATIONS_DIR / g_id / "chunk_001.wav").write_bytes(b"take2")
            g["chunks"][1].update(status="ok", issues=[], accepted=None)
            pipeline.summarize(g)
            db.put_generation_live(g)
        with patch.object(pipeline, "run_generation", fake_run):
            pipeline.regenerate_chunk(gid, 1, None, lambda: False)

    def test_a_delete_during_a_take_switch_stays_deleted(self):
        make_gen("fx1"); self.respeak("fx1")
        with patch.object(pipeline, "assemble", side_effect=lambda g, s: db.set_generation_deleted("fx1", True)):
            pipeline.select_take("fx1", 1, 1)
        self.assertIn("deleted_at", db.get_generation("fx1"))

    def test_a_take_switch_on_a_missing_or_deleted_narration_is_refused(self):
        with self.assertRaisesRegex(ValueError, "deleted"):
            pipeline.select_take("no-such-narration", 1, 1)

    def test_switching_takes_clears_sounds_fine(self):
        make_gen("fx2"); self.respeak("fx2")
        g = db.get_generation("fx2"); g["chunks"][1]["accepted"] = {"by": "person", "at": 1}; db.put_generation(g)
        with patch.object(pipeline, "assemble"):
            pipeline.select_take("fx2", 1, 1)
        self.assertFalse(db.get_generation("fx2")["chunks"][1].get("accepted"))

    def test_accepted_parts_leave_the_flagged_list(self):
        make_gen("fx3")
        r = self.c.post("/api/generations/fx3/chunks/1/accept", headers=H).json()
        self.assertEqual(r["flagged"], [])

    def test_a_part_that_passed_cannot_be_marked(self):
        make_gen("fx4")
        self.assertEqual(self.c.post("/api/generations/fx4/chunks/0/accept", headers=H).status_code, 400)

    def test_checked_by_names_the_check_that_found_it(self):
        g = make_gen("fx5")
        g["chunks"][1].update(issues=[], final_issues=["cut_off"]); db.put_generation(g)
        self.assertEqual(self.c.get("/api/generations/fx5").json()["review"][0]["checked_by"], ["final check"])


class RevealTests(unittest.TestCase):
    """History's folder buttons open the file manager; nothing in the library is changed."""

    def setUp(self):
        self.c = TestClient(main.app, base_url=LOCAL)
        make_gen("rv1")
        (config.GENERATIONS_DIR / "rv1/final.mp3").write_bytes(b"ID3")

    def test_a_narration_shows_its_audio_file_selected(self):
        with patch("platform.system", return_value="Darwin"), patch("subprocess.run") as run:
            r = self.c.post("/api/generations/rv1/reveal", headers=H)
        self.assertEqual(r.status_code, 200)
        self.assertEqual(run.call_args[0][0], ["open", "-R", str(config.GENERATIONS_DIR / "rv1/final.mp3")])

    def test_windows_selects_the_file_in_explorer(self):
        with patch("platform.system", return_value="Windows"), patch("subprocess.run") as run:
            self.c.post("/api/generations/rv1/reveal", headers=H)
        self.assertEqual(run.call_args[0][0][0], "explorer")

    def test_the_folder_button_opens_the_narrations_folder(self):
        with patch("platform.system", return_value="Darwin"), patch("subprocess.run") as run:
            r = self.c.post("/api/generations/reveal", headers=H)
        self.assertEqual(run.call_args[0][0], ["open", str(config.GENERATIONS_DIR)])
        self.assertEqual(r.json()["path"], str(config.GENERATIONS_DIR))

    def test_bad_or_deleted_ids_open_nothing(self):
        db.set_generation_deleted("rv1", True)
        with patch("subprocess.run") as run:
            self.assertEqual(self.c.post("/api/generations/rv1/reveal", headers=H).status_code, 404)
            self.assertEqual(self.c.post("/api/generations/..%2F..%2Fetc/reveal", headers=H).status_code in (400, 404), True)
        run.assert_not_called()

    def test_another_website_cannot_open_folders(self):
        with patch("subprocess.run") as run:
            self.assertEqual(self.c.post("/api/generations/reveal", headers={"Origin": "https://evil.example"}).status_code, 403)
        run.assert_not_called()
