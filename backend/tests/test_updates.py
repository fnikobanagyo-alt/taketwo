"""The update check and in-place update, run only on throwaway git repos (never this checkout)."""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
_data = tempfile.TemporaryDirectory(prefix="taketwo-update-tests-")
os.environ.setdefault("TAKETWO_DATA", _data.name)

from fastapi.testclient import TestClient  # noqa: E402
from app import config, db, main, updates  # noqa: E402

GIT_ENV = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
RELEASE = {"tag_name": "v2.1.0", "body": "Faster first narration.\n- Fixed subtitles", "draft": False, "prerelease": False}


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, env=GIT_ENV, check=True, capture_output=True, text=True).stdout.strip()


class UpdateTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        t = Path(self.tmp.name)
        git(t, "init", "-q", "--bare", "-b", "main", "origin.git")
        git(t, "clone", "-q", "origin.git", "work")
        self.work = t / "work"
        (self.work / "app.txt").write_text("2.0\n")
        git(self.work, "add", "."); git(self.work, "commit", "-qm", "2.0"); git(self.work, "push", "-q", "origin", "HEAD:main")
        git(t, "clone", "-q", "origin.git", "dev")  # the maintainer publishes 2.1 from elsewhere
        (t / "dev" / "app.txt").write_text("2.1\n")
        git(t / "dev", "commit", "-qam", "2.1"); git(t / "dev", "tag", "v2.1.0"); git(t / "dev", "push", "-q", "origin", "main", "v2.1.0")
        self.old = git(self.work, "rev-parse", "HEAD")
        self.p = [patch.object(config, "ROOT", self.work), patch.object(config, "APP_VERSION", "2.0.0"),
                  patch.object(updates, "_fetch_latest", return_value=RELEASE), patch.object(updates, "SETUP_CMD", ["true"])]
        for p in self.p:
            p.start()
        updates._installed = None
        updates.check()

    def tearDown(self):
        for p in self.p:
            p.stop()
        updates._installed = None
        updates._check = None
        self.tmp.cleanup()

    def run_update(self):
        updates._run_update("v2.1.0", lambda *a: None, lambda: False)

    def test_newer_release_is_offered(self):
        s = updates.state()
        self.assertTrue(s["available"])
        self.assertEqual(s["check"]["latest"], "2.1.0")

    def test_offline_or_no_release_means_no_update(self):
        with patch.object(updates, "_fetch_latest", side_effect=OSError("offline")):
            updates.check()
        self.assertFalse(updates.state()["available"])

    def test_update_moves_to_the_release_and_blocks_narrations_until_restart(self):
        self.run_update()
        self.assertEqual((self.work / "app.txt").read_text(), "2.1\n")
        self.assertEqual(updates._installed, "2.1.0")
        self.assertIn("Restart TakeTwo", updates.blocking_reason())

    def test_failed_setup_goes_back_to_the_old_version(self):
        with patch.object(updates, "SETUP_CMD", ["false"]):
            with self.assertRaisesRegex(RuntimeError, "stayed on 2.0.0"):
                self.run_update()
        self.assertEqual(git(self.work, "rev-parse", "HEAD"), self.old)
        self.assertIsNone(updates._installed)

    def test_local_changes_are_never_overwritten(self):
        (self.work / "app.txt").write_text("my edit\n")
        with self.assertRaisesRegex(RuntimeError, "local changes"):
            self.run_update()
        self.assertEqual((self.work / "app.txt").read_text(), "my edit\n")

    def test_own_commits_are_never_replaced(self):
        (self.work / "app.txt").write_text("mine\n")
        git(self.work, "commit", "-qam", "mine")
        mine = git(self.work, "rev-parse", "HEAD")
        with self.assertRaisesRegex(RuntimeError, "its own changes"):
            self.run_update()
        self.assertEqual(git(self.work, "rev-parse", "HEAD"), mine)

    def test_not_a_git_copy_points_to_the_download_page(self):
        with patch.object(config, "ROOT", Path(self.tmp.name)):
            self.assertIn("Download the new version", updates.preflight())


class UpdateApiTests(unittest.TestCase):
    def test_start_without_a_newer_release_is_refused(self):
        updates._check = None
        c = TestClient(main.app, base_url="http://127.0.0.1:8766")
        r = c.post("/api/update/start", headers={"Origin": "http://127.0.0.1:8766"})
        self.assertEqual(r.status_code, 409)

    def test_settings_reject_bad_update_values(self):
        with self.assertRaises(ValueError):
            db.save_settings({"check_updates": "yes"})
        with self.assertRaises(ValueError):
            db.save_settings({"update_dismissed": "../../x"})
        self.assertEqual(db.save_settings({"update_dismissed": "2.1.0"})["update_dismissed"], "2.1.0")

    def test_only_a_vX_Y_Z_tag_is_accepted(self):
        for bad in ("v2.1", "main", "v1.0.0; rm -rf /", "--upload-pack=x"):
            self.assertIsNone(updates.parse(bad))
