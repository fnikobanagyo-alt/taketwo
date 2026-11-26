"""Safety and install checks for a public release. Temporary data only; no models, downloads or network."""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

_data = tempfile.TemporaryDirectory(prefix="voiceforge-release-tests-")
os.environ.setdefault("TAKETWO_DATA", _data.name)
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "backend"))
from fastapi.testclient import TestClient  # noqa: E402
from app import catalog, config, db, main  # noqa: E402

if not config.DATA_DIR.resolve().is_relative_to(Path(tempfile.gettempdir()).resolve()):
    raise unittest.SkipTest("TAKETWO_DATA points at a real library; these tests write and delete files")

LOCAL = "http://127.0.0.1:8766"


class LibraryCannotBeEscapedTests(unittest.TestCase):
    def setUp(self):
        self.c = TestClient(main.app, base_url=LOCAL, raise_server_exceptions=False)
        self.canary = config.DATA_DIR / "profiles" / "canary.txt"
        self.canary.write_text("keep me")

    def test_dot_dot_never_deletes_the_library(self):
        for kind in ("transcripts", "generations", "profiles"):
            r = self.c.delete(f"/api/{kind}/%2e%2e")
            self.assertEqual(r.status_code, 400, kind)
            self.assertTrue(self.canary.exists(), kind)

    def test_the_database_cannot_be_read_as_audio(self):
        for path in ("/api/profiles/%2e%2e/audio/voiceforge.db", "/api/designs/%2e%2e/audio/voiceforge.db"):
            self.assertEqual(self.c.get(path).status_code, 400)

    def test_delete_functions_refuse_bad_ids_themselves(self):
        for fn in (main.delete_transcript, main.delete_generation, main.delete_profile):
            for bad in ("..", ".", "", "a/../..", "x\x00"):
                with self.assertRaises(main.HTTPException):
                    fn(bad)
        self.assertTrue(self.canary.exists())


class FormerNameTests(unittest.TestCase):
    def test_health_names_taketwo_and_its_former_name(self):
        r = TestClient(main.app, base_url=LOCAL).get("/api/health").json()
        self.assertEqual((r["name"], r["formerly"]), ("TakeTwo", "VoiceForge"))

    def test_address_file_is_written_under_both_names(self):
        self.assertEqual([f.parent.name for f in main.ENGINE_FILES], ["TakeTwo", "VoiceForge"])

    def test_old_data_variable_still_works(self):
        with patch.dict(os.environ, {"VOICEFORGE_DATA": "/old"}, clear=False):
            os.environ.pop("TAKETWO_DATA", None)
            self.assertEqual(config._env("DATA", "x"), "/old")
            os.environ["TAKETWO_DATA"] = "/new"
            self.assertEqual(config._env("DATA", "x"), "/new")


class OnlyThisComputerTests(unittest.TestCase):
    def call(self, method, path, **headers):
        return TestClient(main.app, base_url=LOCAL, raise_server_exceptions=False).request(method, path, headers=headers)

    def test_a_foreign_host_name_is_refused(self):
        self.assertEqual(self.call("GET", "/api/profiles", Host="evil.example").status_code, 403)
        self.assertEqual(self.call("GET", "/api/profiles", Host="127.0.0.1.evil.example:8766").status_code, 403)

    def test_another_website_cannot_post(self):
        for origin in ("https://evil.example", "null", "http://127.0.0.1:3000", "http://localhost.evil.example:8766"):
            self.assertEqual(self.call("POST", "/api/generations/none/cancel", Origin=origin).status_code, 403, origin)

    def test_the_app_and_local_tools_still_work(self):
        self.assertEqual(self.call("GET", "/api/health").status_code, 200)
        self.assertEqual(self.call("GET", "/api/profiles", Host="localhost:8766").status_code, 200)
        self.assertNotEqual(self.call("POST", "/api/generations/none/cancel", Origin=LOCAL).status_code, 403)
        self.assertNotEqual(self.call("POST", "/api/generations/none/cancel").status_code, 403)  # MCP server, curl


class SettingsNeverBlockStartupTests(unittest.TestCase):
    def tearDown(self):
        for f in config.DATA_DIR.glob("settings.json*"):
            f.unlink()

    def test_a_broken_settings_file_is_kept_aside_and_defaults_are_used(self):
        for text in ('{"a":', "[]", '{"custom_engines": "x"}', '{"custom_engines": [1]}'):
            config.SETTINGS_PATH.write_text(text)
            s = db.get_settings()
            self.assertEqual(s["max_attempts"], config.DEFAULT_SETTINGS["max_attempts"], text)
            self.assertIsInstance(s["custom_engines"], list)
            main.engines.set_custom_engines(s["custom_engines"])
        config.SETTINGS_PATH.write_text('{"a":')
        db.get_settings()
        self.assertEqual((config.DATA_DIR / "settings.json.broken").read_text(), '{"a":')

    def test_bad_custom_engines_are_rejected_when_saved(self):
        for bad in ("x", [1], [{"id": "a"}]):
            with self.assertRaises(ValueError):
                db.save_settings({"custom_engines": bad})
        db.save_settings({"custom_engines": [{"id": "a", "repo": "o/r", "name": "n", "family": "qwen3"}]})

    def test_a_failed_save_keeps_the_previous_settings(self):
        db.save_settings({"max_attempts": 3})
        with patch.object(db.json, "dumps", side_effect=RuntimeError("disk full")), self.assertRaises(RuntimeError):
            db.save_settings({"max_attempts": 5})
        self.assertEqual(json.loads(config.SETTINGS_PATH.read_text())["max_attempts"], 3)


class EngineTests(unittest.TestCase):
    def test_an_unknown_model_is_a_clear_404(self):
        c = TestClient(main.app, base_url=LOCAL, raise_server_exceptions=False)
        self.assertEqual(c.post("/api/engines/nope/download").status_code, 404)
        self.assertEqual(c.get("/api/engines/nope/download").status_code, 404)

    def test_library_updates_stay_in_the_tested_range(self):
        for latest, offered in (("0.5.9", True), ("0.6.8", False), ("1.0.0", False)):
            with patch.object(catalog, "_get", return_value={"info": {"version": latest}}), patch.object(catalog.metadata, "version", return_value="0.5.7"):
                self.assertEqual(catalog.library_status()["update_available"], offered, latest)
        with patch.object(catalog.subprocess, "run") as run, patch.object(catalog.metadata, "version", return_value="0.5.9"):
            run.return_value.returncode = 0
            catalog.upgrade_library()
        self.assertIn("mlx-audio>=0.5.6,<0.6", run.call_args.args[0])


class InstallerTests(unittest.TestCase):
    def test_every_imported_package_is_declared_and_pinned(self):
        import ast
        req = (ROOT / "backend/requirements.txt").read_text()
        names = {}
        for line in req.splitlines():
            line = line.split("#")[0].strip()
            if line:
                self.assertRegex(line, r"^[A-Za-z0-9_.-]+(\[[a-z]+\])?==\S+$")
                names[re.split(r"[\[=]", line)[0].lower().replace("_", "-")] = line
        dist = {"mlx_audio": "mlx-audio", "huggingface_hub": "huggingface-hub"}
        for f in (ROOT / "backend/app").glob("*.py"):
            for n in ast.walk(ast.parse(f.read_text())):
                mods = [a.name for a in n.names] if isinstance(n, ast.Import) else [n.module] if isinstance(n, ast.ImportFrom) and n.module and not n.level else []
                for m in (m.split(".")[0] for m in mods):
                    if m not in sys.stdlib_module_names and m != "app":
                        self.assertIn(dist.get(m, m), names, f"{f.name} imports {m}")
        lock = (ROOT / "backend/requirements.lock").read_text()
        for line in names.values():
            self.assertIn(re.sub(r"\[.*\]", "", line).lower().replace("_", "-"), lock.lower().replace("_", "-"))

    def _start(self, bin_dir, **env):
        with tempfile.TemporaryDirectory() as t:
            (Path(t) / "start.sh").write_text((ROOT / "start.sh").read_text())
            for d in ("backend", "frontend"):
                (Path(t) / d).mkdir()
            return subprocess.run(["/bin/zsh", "start.sh", "--check"], cwd=t, capture_output=True, text=True, timeout=30,
                                  env={"HOME": t, "PATH": f"{bin_dir}:/usr/bin:/bin", "VOICEFORGE_PATH": f"{bin_dir}:/usr/bin:/bin", **env})

    def _bin(self, t, **scripts):
        for name, body in scripts.items():
            f = Path(t) / name.replace("_dot_", ".")
            f.write_text(f"#!/bin/sh\n{body}\n")
            f.chmod(0o755)
        return t

    OK = dict(uname="[ \"$1\" = -m ] && echo arm64 || echo Darwin", sw_vers="echo 15.1", ffmpeg="", ffprobe="", npm="", node="echo v22.12.0", python3_dot_11="echo 3.11")

    def test_requirements_are_checked_before_anything_is_installed(self):
        cases = {"uname": ("[ \"$1\" = -m ] && echo x86_64 || echo Darwin", "Apple Silicon"), "sw_vers": ("echo 13.6", "macOS 14"),
                 "node": ("echo v18.0.0", "Node")}
        for tool, (body, words) in cases.items():
            with tempfile.TemporaryDirectory() as t:
                r = self._start(self._bin(t, **{**self.OK, tool: body}))
                self.assertNotEqual(r.returncode, 0, tool)
                self.assertIn(words, r.stdout + r.stderr, tool)
        for tool, words in (("ffmpeg", "ffmpeg"), ("npm", "Node"), ("python3_dot_11", "Python 3.11")):
            with tempfile.TemporaryDirectory() as t:
                r = self._start(self._bin(t, **{k: v for k, v in self.OK.items() if k != tool}))
                self.assertNotEqual(r.returncode, 0, tool)
                self.assertIn(words, r.stdout + r.stderr, tool)
        with tempfile.TemporaryDirectory() as t:
            r = self._start(self._bin(t, **self.OK))
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_private_material_is_ignored_by_git(self):
        paths = ["data/voiceforge.db", "eval/probes/a.wav", "docs/design-mockup-20261125/x.html", "backups/x", "desktop/config.json",
                 "CLAUDE.md", "AGENTS.md", ".claude/settings.local.json", ".DS_Store", "docs/agent-context.json", "frontend/tsconfig.tsbuildinfo", "sample.wav", "sub/clip.mp3"]
        with tempfile.TemporaryDirectory() as t:
            subprocess.run(["git", "init", "-q", t], check=True)
            (Path(t) / ".gitignore").write_text((ROOT / ".gitignore").read_text())
            r = subprocess.run(["git", "-C", t, "check-ignore", "--stdin"], input="\n".join(paths), capture_output=True, text=True)
        self.assertEqual(set(paths) - set(r.stdout.split("\n")), set())
        for keep in ("backend/app/main.py", "README.md", "LICENSE", "frontend/src/App.tsx", "desktop/build/icon.png", "start.sh"):
            self.assertNotIn(keep, r.stdout)

    def test_published_files_hold_no_personal_paths(self):
        with tempfile.TemporaryDirectory() as t:
            subprocess.run(["git", "init", "-q", "--bare", t], check=True)
            files = subprocess.run(["git", f"--git-dir={t}", f"--work-tree={ROOT}", "ls-files", "-o", "--exclude-standard", "-z"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.split("\0")
        home = str(Path.home())
        for f in filter(None, files):
            try:
                text = (ROOT / f).read_text()
            except (UnicodeDecodeError, OSError):
                continue
            self.assertNotIn(home, text, f)
            self.assertFalse(f.endswith((".wav", ".mp3", ".db")), f)


if __name__ == "__main__":
    unittest.main()
