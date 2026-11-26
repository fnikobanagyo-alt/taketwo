"""TakeTwo engine contract: what Re:Create, Media Machine and the MCP server rely on, checked over HTTP only.

Needs TAKETWO_URL pointing at an engine seeded with contract/fixture.json (see README.md). Never import
backend/app here: the same file runs against the Rust engine. Standard library only.
"""
from __future__ import annotations

import http.client
import importlib.util
import json
import os
import sys
import tempfile
import types
import unittest
import urllib.parse
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
URL = os.environ.get("TAKETWO_URL", "").rstrip("/")
_u = urllib.parse.urlsplit(URL)
if not URL or _u.hostname not in ("127.0.0.1", "localhost") or not _u.port or 8765 <= _u.port <= 8785:
    # 8765-8785 is the desktop app with the real library; these tests write to the library
    raise SystemExit("Set TAKETWO_URL to a seeded test engine on this computer, outside ports 8765-8785.")

FIXTURE = json.loads((HERE / "fixture.json").read_text())
GOLDEN = json.loads((HERE / "golden" / "narrations.json").read_text())
GENS = {g["id"]: g for g in FIXTURE["generations"]}
VOICE = FIXTURE["profiles"][0]
LOCAL_ORIGIN = URL  # the engine's own page


def call(method: str, path: str, body=None, headers: dict | None = None):
    """(status, headers, raw body). http.client sends Host and paths exactly as given."""
    c = http.client.HTTPConnection(_u.hostname, _u.port, timeout=30)
    data = json.dumps(body).encode() if body is not None else None
    h = {"Content-Type": "application/json"} if data else {}
    c.request(method, path, body=data, headers={**h, **(headers or {})})
    r = c.getresponse()
    out = (r.status, {k.lower(): v for k, v in r.getheaders()}, r.read())
    c.close()
    return out


def get_json(path: str, expect: int = 200):
    status, _, raw = call("GET", path)
    assert status == expect, f"GET {path}: {status} {raw[:200]!r}"
    return json.loads(raw)


def write(method: str, path: str, body=None):
    """A write from the app's own page (local Origin), as the UI sends it."""
    status, _, raw = call(method, path, body, {"Origin": LOCAL_ORIGIN})
    return status, (json.loads(raw) if raw[:1] in (b"{", b"[") else raw)


class Health(unittest.TestCase):
    def test_health_names_taketwo_and_its_former_name(self):
        h = get_json("/api/health")
        self.assertIs(h["ok"], True)
        self.assertEqual((h["name"], h["formerly"]), ("TakeTwo", "VoiceForge"))
        self.assertIsInstance(h["version"], str)

    def test_pipeline_version_lets_clients_recognise_the_engine(self):
        self.assertIn("version", get_json("/api/pipeline-version"))  # the MCP server's engine probe

    def test_status_reports_an_idle_queue(self):
        s = get_json("/api/status")
        self.assertEqual((s["running"], s["queued"], s["queue"]), (None, 0, []))
        self.assertIsInstance(s["max_script_words"], int)


class Profiles(unittest.TestCase):
    def test_list_shape(self):
        voices = get_json("/api/profiles")
        v = next(v for v in voices if v["id"] == VOICE["id"])
        for k in ("id", "name", "language", "status"):
            self.assertEqual(v[k], VOICE[k], k)


class Generations(unittest.TestCase):
    def test_list_has_summaries_newest_first(self):
        items = [g for g in get_json("/api/generations") if g["id"] in GENS]
        self.assertEqual([g["id"] for g in items], sorted(GENS, key=lambda i: -GENS[i]["created"]))
        for g in items:
            want = GENS[g["id"]]
            self.assertNotIn("chunks", g)
            self.assertNotIn("cleaned_text", g)
            self.assertEqual(g["text_preview"], want["text"][:240])
            self.assertEqual(g["outcome"], GOLDEN[g["id"]]["outcome"])
            self.assertEqual(g["review_count"], len(GOLDEN[g["id"]]["review"]))
            for k in ("title", "status", "profile_id", "profile_name", "source"):
                self.assertEqual(g[k], want[k], (g["id"], k))

    def test_get_carries_outcome_and_review_items(self):
        for gid, want in GENS.items():
            g = get_json(f"/api/generations/{gid}")
            self.assertEqual((g["id"], g["status"], g["title"]), (gid, want["status"], want["title"]))
            self.assertEqual([c["text"] for c in g["chunks"]], [c["text"] for c in want["chunks"]])
            self.assertEqual(g["outcome"], GOLDEN[gid]["outcome"], gid)
            self.assertIsNone(g["job"])
            review = [{k: r[k] for k in GOLDEN[gid]["review"][0]} for r in g["review"]] if GOLDEN[gid]["review"] else g["review"]
            self.assertEqual(review, GOLDEN[gid]["review"], gid)

    def test_unknown_narration_is_404(self):
        self.assertEqual(call("GET", "/api/generations/nosuchid")[0], 404)

    def test_creating_for_an_unknown_voice_is_404(self):
        self.assertEqual(write("POST", "/api/generations", {"profile_id": "nosuchvoice", "text": "Hello."})[0], 404)

    def test_an_empty_script_is_refused_before_anything_is_queued(self):
        status, body = write("POST", "/api/generations", {"profile_id": VOICE["id"], "text": "[1] [2]"})
        self.assertEqual(status, 400, body)
        self.assertEqual(get_json("/api/status")["queue"], [])


class Search(unittest.TestCase):
    def ids(self, **q):
        return [r["id"] for r in get_json("/api/generations/search?" + urllib.parse.urlencode(q)) if r["id"] in GENS]

    def test_result_shape(self):
        r = next(r for r in get_json("/api/generations/search?q=rivers") if r["id"] == "ctclean01")
        self.assertEqual({k: r[k] for k in ("title", "voice", "profile_id", "status", "outcome", "source")},
                         {"title": "Rivers and towns", "voice": VOICE["name"], "profile_id": VOICE["id"], "status": "done",
                          "outcome": "clean", "source": "mcp"})

    def test_filters(self):
        self.assertEqual(self.ids(q="Nile"), ["ctclean01"])
        self.assertEqual(self.ids(q="review"), ["ctreview01"])  # title words count too
        self.assertEqual(set(self.ids(status="needs_review")), {"ctreview01", "ctaccept01"})
        self.assertEqual(self.ids(status="cancelled"), ["ctpending01"])
        self.assertEqual(len(self.ids(voice=VOICE["name"].lower())), len(GENS))
        self.assertEqual(self.ids(voice="nobody"), [])

    def test_same_script_matches_after_text_cleaning(self):
        """Text cleaning, observed black-box: the raw script must clean to the stored cleaned_text."""
        for gid, g in GENS.items():
            self.assertIn(gid, self.ids(same_script=g["text"]), gid)
        self.assertEqual(self.ids(same_script="Welcome to the show"), [])


class TimingsAndSubtitles(unittest.TestCase):
    def test_timings_match_golden(self):
        for gid, gold in GOLDEN.items():
            if "timings" not in gold:
                continue
            t, want = get_json(f"/api/generations/{gid}/timings"), gold["timings"]
            self.assertEqual((t["speed"], t["duration"], t["estimated"]), (want["speed"], want["duration"], want["estimated"]), gid)
            for key, fields in (("words", ("word",)), ("sentences", ("text",))):
                self.assertEqual(len(t[key]), len(want[key]), (gid, key))
                for a, b in zip(t[key], want[key]):
                    self.assertEqual([a[f] for f in fields], [b[f] for f in fields])
                    self.assertAlmostEqual(a["start"], b["start"], places=3)
                    self.assertAlmostEqual(a["end"], b["end"], places=3)

    def test_subtitles_match_golden(self):
        for gid, gold in GOLDEN.items():
            if "srt" not in gold:
                continue
            for fmt in ("srt", "vtt", "txt"):
                status, headers, raw = call("GET", f"/api/generations/{gid}/subtitles?fmt={fmt}")
                self.assertEqual(status, 200)
                self.assertEqual(raw.decode("utf-8"), gold[fmt], (gid, fmt))
                self.assertIn(f".{fmt}", headers.get("content-disposition", ""))

    def test_not_rendered_yet_is_404(self):
        for route in ("timings", "subtitles", "audio"):
            self.assertEqual(call("GET", f"/api/generations/ctpending01/{route}")[0], 404, route)


class Audio(unittest.TestCase):
    def test_final_wav_is_served(self):
        status, headers, raw = call("GET", "/api/generations/ctclean01/audio")
        self.assertEqual(status, 200)
        self.assertEqual(raw[:4], b"RIFF")
        self.assertIn("wav", headers["content-type"])

    def test_download_names_the_file(self):
        status, headers, _ = call("GET", "/api/generations/ctclean01/audio?download=true")
        self.assertEqual(status, 200)
        self.assertIn("Contract Voice - Rivers and towns.wav", urllib.parse.unquote(headers["content-disposition"]))

    def test_part_and_take_audio(self):
        self.assertEqual(call("GET", "/api/generations/ctreview01/chunks/1/audio")[2][:4], b"RIFF")
        self.assertEqual(call("GET", "/api/generations/ctreview01/chunks/1/takes/1/audio")[2][:4], b"RIFF")
        self.assertEqual(call("GET", "/api/generations/ctreview01/chunks/1/takes/9/audio")[0], 404)
        self.assertEqual(call("GET", "/api/generations/ctreview01/chunks/7/audio")[0], 404)


class ReviewActions(unittest.TestCase):
    def test_sounds_fine_clears_the_part_and_undo_brings_it_back(self):
        try:
            status, g = write("POST", "/api/generations/ctaccept01/chunks/1/accept")
            self.assertEqual(status, 200, g)
            self.assertEqual((g["outcome"], g["review"]), ("clean", []))
            self.assertEqual(g["chunks"][1]["accepted"]["by"], "person")
            self.assertEqual(get_json("/api/generations/ctaccept01")["outcome"], "clean")
        finally:
            status, g = write("DELETE", "/api/generations/ctaccept01/chunks/1/accept")
        self.assertEqual(status, 200)
        self.assertEqual((g["outcome"], [r["part"] for r in g["review"]]), ("needs_listen", [2]))

    def test_sounds_fine_is_refused_where_it_means_nothing(self):
        self.assertEqual(write("POST", "/api/generations/ctaccept01/chunks/0/accept")[0], 400)  # passed its checks
        self.assertEqual(write("POST", "/api/generations/ctpending01/chunks/0/accept")[0], 400)  # no audio yet
        self.assertEqual(write("POST", "/api/generations/ctaccept01/chunks/5/accept")[0], 404)

    def test_an_unknown_take_cannot_be_selected(self):
        self.assertEqual(write("POST", "/api/generations/ctreview01/chunks/1/select-take", {"take": 9})[0], 404)

    def test_fix_remaining_with_nothing_to_fix_is_refused(self):
        self.assertEqual(write("POST", "/api/generations/ctclean01/fix-remaining")[0], 400)

    def test_delete_hides_and_restore_brings_back(self):
        try:
            status, body = write("DELETE", "/api/generations/cttrash01")
            self.assertEqual((status, body["ok"]), (200, True))
            self.assertEqual(body["undo"], "/api/generations/cttrash01/restore")
            self.assertEqual(call("GET", "/api/generations/cttrash01")[0], 404)
            self.assertNotIn("cttrash01", [g["id"] for g in get_json("/api/generations")])
            self.assertNotIn("cttrash01", [g["id"] for g in get_json("/api/generations/search?q=delete")])
        finally:
            self.assertEqual(write("POST", "/api/generations/cttrash01/restore")[0], 200)
        self.assertEqual(get_json("/api/generations/cttrash01")["status"], "done")
        self.assertEqual(call("GET", "/api/generations/cttrash01/audio")[0], 200)  # files kept

    def test_title_and_favourite_edits(self):
        try:
            status, g = write("PATCH", "/api/generations/ctclean01", {"title": "Renamed", "favorite": True})
            self.assertEqual((status, g["title"], g["favorite"]), (200, "Renamed", True))
        finally:
            write("PATCH", "/api/generations/ctclean01", {"title": GENS["ctclean01"]["title"], "favorite": False})


class SafetyGuards(unittest.TestCase):
    def test_a_foreign_host_is_refused(self):
        for host in ("evil.example", "127.0.0.1.evil.example:8766"):
            self.assertEqual(call("GET", "/api/profiles", headers={"Host": host})[0], 403, host)

    def test_other_websites_cannot_write(self):
        for origin in ("https://evil.example", "null", "http://127.0.0.1:3000", "http://localhost.evil.example:8766"):
            self.assertEqual(call("POST", "/api/generations/ctclean01/cancel", headers={"Origin": origin})[0], 403, origin)
            self.assertEqual(call("DELETE", "/api/generations/cttrash01", headers={"Origin": origin})[0], 403, origin)
        self.assertEqual(call("GET", "/api/generations/cttrash01")[0], 200)

    def test_local_tools_without_origin_still_work(self):
        self.assertEqual(call("POST", "/api/generations/nosuchid/cancel")[0], 200)  # MCP server, curl

    def test_ids_cannot_leave_the_library(self):
        for path in ("/api/generations/%2e%2e", "/api/profiles/%2e%2e", "/api/transcripts/%2e%2e", "/api/generations/a%2f..%2f.."):
            self.assertEqual(write("DELETE", path)[0], 400, path)
        for path in ("/api/profiles/%2e%2e/audio/voiceforge.db", "/api/generations/..%2f..%2fx/audio"):
            status = call("GET", path)[0]
            self.assertTrue(400 <= status < 500, (path, status))
        self.assertEqual(len([g for g in get_json("/api/generations") if g["id"] in GENS]), len(GENS))


def load_mcp():
    """The MCP server module with FastMCP stubbed (only its tool functions are used). It reaches the engine through TAKETWO_URL."""
    fastmcp = types.ModuleType("mcp.server.fastmcp")
    fastmcp.FastMCP = lambda name: types.SimpleNamespace(tool=lambda: (lambda f: f), run=lambda: None)
    mods = {"mcp": types.ModuleType("mcp"), "mcp.server": types.ModuleType("mcp.server"), "mcp.server.fastmcp": fastmcp}
    with patch.dict(sys.modules, mods):
        spec = importlib.util.spec_from_file_location("taketwo_mcp_contract", HERE.parent / "mcp" / "taketwo_mcp.py")
        m = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(m)
    return m


class McpTools(unittest.TestCase):
    """The MCP tools Media Machine and Claude call, run against the engine under test."""

    @classmethod
    def setUpClass(cls):
        cls.m = load_mcp()
        assert cls.m._engine() == URL  # pinned: never scans ports or opens the desktop app

    def test_list_voices(self):
        v = next(v for v in self.m.list_voices() if v["id"] == VOICE["id"])
        self.assertEqual((v["name"], v["language"], v["speech_rate_wps"]), (VOICE["name"], "en", VOICE["speech_rate_wps"]))

    def test_get_status_reports_review(self):
        s = self.m.get_status("ctreview01")
        self.assertEqual((s["status"], s["review_status"], s["parts_done"], s["parts_total"]), ("done_needs_listen", "done_needs_review", 3, 3))
        self.assertEqual(s["parts_needing_review"], GOLDEN["ctreview01"]["review"])
        self.assertEqual(self.m.get_status("ctclean01")["status"], "done")

    def test_timings_subtitles_and_search(self):
        self.assertEqual(self.m.get_timings("ctclean01")["estimated"], GOLDEN["ctclean01"]["timings"]["estimated"])
        self.assertEqual(self.m.get_subtitles("ctclean01", "vtt"), GOLDEN["ctclean01"]["vtt"])
        self.assertIn("ctreview01", [r["id"] for r in self.m.find_narration(status="needs_review")])

    def test_get_audio_saves_the_wav_with_a_review_warning(self):
        with tempfile.TemporaryDirectory() as t:
            out = self.m.get_audio("ctreview01", "wav", output_path=str(Path(t) / "n.wav"))
            self.assertEqual(Path(out["path"]).read_bytes()[:4], b"RIFF")
            self.assertEqual(out["outcome"], "needs_listen")
            self.assertIn("parts_needing_review", out)

    def test_mark_sounds_fine_round_trip(self):
        try:
            self.m.mark_sounds_fine("ctaccept01", 2)
            self.assertEqual(self.m.get_status("ctaccept01")["status"], "done")
        finally:
            self.m.mark_sounds_fine("ctaccept01", 2, undo=True)
        self.assertEqual(self.m.get_status("ctaccept01")["status"], "done_needs_listen")


if __name__ == "__main__":
    unittest.main()
