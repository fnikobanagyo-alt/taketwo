"""The MCP tools map to the engine's HTTP API correctly. No network: _req, _engine and urlopen are stubbed."""
import importlib.util
import io
import sys
import tempfile
import types
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import patch


def load_mcp():
    fastmcp = types.ModuleType("mcp.server.fastmcp")
    fastmcp.FastMCP = lambda name: types.SimpleNamespace(tool=lambda: (lambda f: f), run=lambda: None)
    mods = {"mcp": types.ModuleType("mcp"), "mcp.server": types.ModuleType("mcp.server"), "mcp.server.fastmcp": fastmcp}
    with patch.dict(sys.modules, mods):
        spec = importlib.util.spec_from_file_location("taketwo_mcp_tools", Path(__file__).resolve().parents[2] / "mcp" / "taketwo_mcp.py")
        m = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(m)
    m._engine = lambda: "http://engine"
    return m


def http_error(code, body: bytes):
    return urllib.error.HTTPError("http://engine/x", code, "Reason", {}, io.BytesIO(body))


class FakeResponse(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *a):
        self.close()


DONE = {"status": "done", "file": "a.mp3", "outcome": "needs_listen", "duration": 12.5, "title": "T", "profile_name": "Ann",
        "chunks": [{"status": "ok"}, {"status": "flagged"}],
        "review": [{"part": 2, "text": "Hi.", "issues": ["repeat"], "reasons": ["Repeated words"], "start": 1.0, "end": 2.0,
                    "heard": "hi hi", "pitch_hz": 200, "voice_pitch_hz": 190, "checked_by": "whisper", "take": 1}]}


class McpToolTests(unittest.TestCase):
    def setUp(self):
        self.m = load_mcp()
        self.calls = []
        self.reply = {}
        self.m._req = lambda base, method, path, body=None, timeout=30: self.calls.append((method, path, body)) or self.reply

    def test_error_shapes_become_runtime_errors(self):
        for body, want in ((b'{"detail": "Already running"}', "409: Already running"),
                           (b'{"detail": {"message": "Busy now", "x": 1}}', "409: Busy now"),
                           (b"<html>oops</html>", "409: Reason"),
                           (b'["a list"]', "409: Reason"),
                           (b"", "409: Reason")):
            def boom(*a, body=body, **k):
                raise http_error(409, body)
            self.m._req = boom
            with self.assertRaises(RuntimeError) as cm:
                self.m._call("POST", "/api/x")
            self.assertIn(want, str(cm.exception))

    def test_duplicate_narration_is_still_returned(self):
        def dup(*a, **k):
            raise http_error(409, b'{"detail": {"message": "Already in the queue", "duplicate_of": "g9"}}')
        self.m._req = dup
        self.assertEqual(self.m._call("POST", "/api/generations")["duplicate_of"], "g9")

    def test_respeak_part_two_is_chunk_one(self):
        self.m.respeak_part("g1", 2, "New words.")
        self.assertEqual(self.calls[0], ("POST", "/api/generations/g1/chunks/1/regenerate", {"text": "New words."}))
        self.m.respeak_part("g1", 1)
        self.assertEqual(self.calls[1][1:], ("/api/generations/g1/chunks/0/regenerate", {}))
        for bad in (0, -1, 1.5, True):
            with self.assertRaises(ValueError):
                self.m.respeak_part("g1", bad)

    def test_mark_sounds_fine_and_undo(self):
        self.m.mark_sounds_fine("g1", 3)
        self.m.mark_sounds_fine("g1", 3, undo=True)
        self.assertEqual([c[:2] for c in self.calls], [("POST", "/api/generations/g1/chunks/2/accept"),
                                                       ("DELETE", "/api/generations/g1/chunks/2/accept")])

    def test_other_actions_hit_their_endpoints(self):
        self.m.fix_remaining("g1")
        self.m.stop_narration("g1")
        self.m.resume_narration("g1")
        self.assertEqual([c[1] for c in self.calls], ["/api/generations/g1/fix-remaining", "/api/generations/g1/cancel",
                                                      "/api/generations/g1/resume"])

    def test_find_narration_passes_only_given_filters(self):
        self.reply = [{"id": "g1"}]
        self.assertEqual(self.m.find_narration(text="big cats", status="needs_review"), [{"id": "g1"}])
        self.assertEqual(self.calls[0][1], "/api/generations/search?q=big+cats&status=needs_review")
        self.m.find_narration(voice="Ann")
        self.assertEqual(self.calls[1][1], "/api/generations/search?voice=Ann")

    def test_ids_are_quoted(self):
        self.reply = {"status": "queued", "chunks": [], "job": {"status": "queued"}}
        out = self.m.get_status("../a b/?")
        self.assertEqual(self.calls[0][1], "/api/generations/..%2Fa%20b%2F%3F")
        self.assertEqual(out["open_in_app"], "http://engine/#/history/..%2Fa%20b%2F%3F")
        self.m.stop_narration("x/y")
        self.assertEqual(self.calls[1][1], "/api/generations/x%2Fy/cancel")

    def test_status_while_queued_and_running(self):
        self.reply = {"status": "running", "chunks": [{"status": "ok"}, {"status": "pending"}], "eta": 40,
                      "job": {"status": "queued"}, "queue": {"ahead": 2, "waiting_for": ["A"]}}
        out = self.m.get_status("g1")
        self.assertEqual((out["status"], out["review_status"], out["queue_ahead"], out["eta_seconds"], out["parts_done"]),
                         ("queued", "queued", 2, 40, 1))
        self.assertNotIn("parts_needing_review", out)

    def test_status_when_done_needs_review(self):
        self.reply = DONE
        out = self.m.get_status("g1")
        self.assertEqual((out["status"], out["review_status"]), ("done_needs_listen", "done_needs_review"))
        self.assertEqual(out["open_in_app"], "http://engine/#/history/g1")
        self.assertEqual(out["parts_needing_review"], [{"part": 2, "text": "Hi.", "reasons": ["Repeated words"], "issues": ["repeat"],
                                                        "start": 1.0, "end": 2.0, "checked_by": "whisper"}])
        self.reply = {**DONE, "outcome": "clean", "review": []}
        out = self.m.get_status("g1")
        self.assertEqual((out["status"], out["review_status"], out["parts_needing_review"]), ("done", "done", []))

    def test_list_voices_includes_new_fields_for_ready_voices(self):
        self.reply = [{"id": "v1", "name": " Ann  B ", "status": "ready", "description": "warm", "speech_rate_wps": 2.6, "studio": True},
                      {"id": "v2", "name": "Cal", "status": "processing"}]
        self.assertEqual(self.m.list_voices(), [{"id": "v1", "name": "Ann B", "language": "en", "description": "warm",
                                                 "speech_rate_wps": 2.6, "studio": True}])

    def test_subtitles_validate_format_and_return_text(self):
        with self.assertRaises(ValueError):
            self.m.get_subtitles("g1", "docx")
        with patch.object(self.m.urllib.request, "urlopen", return_value=FakeResponse("1\n00:00 --> 00:01\nHi\n".encode())) as op:
            self.assertEqual(self.m.get_subtitles("g1", "VTT"), "1\n00:00 --> 00:01\nHi\n")
        self.assertEqual(op.call_args[0][0].full_url, "http://engine/api/generations/g1/subtitles?fmt=vtt")

    def test_subtitles_error_before_audio_is_clean(self):
        with patch.object(self.m.urllib.request, "urlopen", side_effect=http_error(404, b'{"detail": "Subtitles are available once the audio is rendered"}')):
            with self.assertRaisesRegex(RuntimeError, "404: Subtitles are available"):
                self.m.get_subtitles("g1")

    def test_get_audio_warns_and_saves_to_output_path(self):
        self.reply = DONE
        with tempfile.TemporaryDirectory() as d, \
                patch.object(self.m.urllib.request, "urlopen", side_effect=lambda *a, **k: FakeResponse(b"ID3audio")):
            dest = Path(d) / "new" / "out.mp3"
            out = self.m.get_audio("g1", output_path=str(dest))
            self.assertEqual(dest.read_bytes(), b"ID3audio")
            self.assertEqual((out["path"], out["outcome"]), (str(dest), "needs_listen"))
            self.assertIn("listen", out["warning"])
            self.assertEqual(out["parts_needing_review"][0]["part"], 2)
            with self.assertRaises(FileExistsError):
                self.m.get_audio("g1", output_path=str(dest))
            self.assertEqual(dest.read_bytes(), b"ID3audio")
            self.m.get_audio("g1", output_path=str(dest), overwrite=True)
            with self.assertRaises(ValueError):
                self.m.get_audio("g1", format="wav", output_path=str(Path(d) / "out.mp3"))

    def test_get_audio_clean_has_no_warning(self):
        self.reply = {**DONE, "outcome": "clean", "review": []}
        with tempfile.TemporaryDirectory() as d, \
                patch.object(self.m.urllib.request, "urlopen", side_effect=lambda *a, **k: FakeResponse(b"RIFF")):
            out = self.m.get_audio("g1", format="wav", output_path=str(Path(d) / "a.WAV"))
        self.assertEqual(out["outcome"], "clean")
        self.assertNotIn("warning", out)


if __name__ == "__main__":
    unittest.main()
