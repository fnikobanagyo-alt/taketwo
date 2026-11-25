# /// script
# requires-python = ">=3.10"
# dependencies = ["mcp>=1.2,<2"]  # 2.x renamed FastMCP to MCPServer
# ///
"""VoiceForge MCP server: lets Claude and other MCP clients use the local VoiceForge app.

Run: uv run --script voiceforge_mcp.py   (stdio transport)
It talks to the running VoiceForge engine over its local HTTP API and opens the app if needed.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from mcp.server.fastmcp import FastMCP

PORTS = range(8765, 8786)  # the desktop app takes the first free port from 8766
DOWNLOADS = Path.home() / "Downloads" / "VoiceForge"
mcp = FastMCP("voiceforge")
_base: str | None = None


def _req(base: str, method: str, path: str, body: dict | None = None, timeout: float = 30):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(base + path, data=data, method=method, headers={"Content-Type": "application/json", "X-VoiceForge-Client": "mcp"})
    with urllib.request.urlopen(r, timeout=timeout) as resp:
        return json.loads(resp.read())


def _is_voiceforge(base: str) -> bool:
    try:  # other local servers answer on these ports too; check two VoiceForge-only endpoints
        return _req(base, "GET", "/api/health", timeout=1).get("ok") is True and "version" in _req(base, "GET", "/api/pipeline-version", timeout=1)
    except Exception:
        return False


def _engine() -> str:
    global _base
    if os.environ.get("VOICEFORGE_URL"):  # pin a specific engine (tests, remote setups)
        return os.environ["VOICEFORGE_URL"].rstrip("/")
    if _base and _is_voiceforge(_base):
        return _base
    try:  # the engine writes its address here on start
        published = json.loads((Path.home() / "Library/Application Support/VoiceForge/engine.json").read_text())["url"]
        if _is_voiceforge(published):
            _base = published
            return _base
    except (OSError, ValueError, KeyError):
        pass
    for attempt in range(2):
        for p in PORTS:
            if _is_voiceforge(f"http://127.0.0.1:{p}"):
                _base = f"http://127.0.0.1:{p}"
                return _base
        if attempt == 0:
            subprocess.run(["open", "-g", "-a", "VoiceForge"], check=False)
            for _ in range(60):  # the engine takes a few seconds to start
                time.sleep(1)
                if any(_is_voiceforge(f"http://127.0.0.1:{p}") for p in PORTS):
                    break
    raise RuntimeError("VoiceForge is not running and could not be opened. Open the VoiceForge app, then try again.")


def _call(method: str, path: str, body: dict | None = None):
    try:
        return _req(_engine(), method, path, body)
    except urllib.error.HTTPError as e:
        try:
            detail = json.loads(e.read()).get("detail")
        except Exception:
            detail = None
        if isinstance(detail, dict):
            return {"error": e.code, **detail}
        raise RuntimeError(f"VoiceForge returned {e.code}: {detail or e.reason}") from None


def _voice_id(voice: str) -> str:
    voices = [v for v in _call("GET", "/api/profiles") if v.get("status") == "ready"]
    want = " ".join(voice.lower().split())
    for v in voices:
        if v["id"] == voice or " ".join(v["name"].lower().split()) == want:
            return v["id"]
    matches = [v for v in voices if want in " ".join(v["name"].lower().split())]
    if len(matches) == 1:
        return matches[0]["id"]
    names = ", ".join(" ".join(v["name"].split()) for v in (matches or voices))
    raise ValueError(f"{'Several voices match' if matches else 'No voice matches'} {voice!r}. Choose one of: {names}")


@mcp.tool()
def list_voices() -> list[dict]:
    """List the cloned voices in VoiceForge that are ready to use (id, name, language)."""
    return [{"id": v["id"], "name": " ".join(v["name"].split()), "language": v.get("language", "en")}
            for v in _call("GET", "/api/profiles") if v.get("status") == "ready"]


@mcp.tool()
def generate_narration(text: str, voice: str, speed: float = 1.0, style: float | None = None, stability: float | None = None) -> dict:
    """Start a narration of `text` in a VoiceForge voice (name or id from list_voices).

    Returns at once with a narration id; rendering takes about one to two seconds per second
    of audio. Poll get_status with the id, then call get_audio when the status is "done".
    Send a whole script as one narration and cut video with get_timings, rather than joining several.
    speed: 0.7 to 1.3 (1.0 = the voice's natural pace; faster also shortens pauses).
    style: 0 to 1, how expressive (default 0 = the voice's chosen clip). Leave unset for the voice's default.
    stability: 0 to 1, more varied <-> steadier delivery (default 0.5). Leave unset for the voice's default.
    """
    body = {"profile_id": _voice_id(voice), "text": text, "speed": speed}
    vs = {k: float(v) for k, v in (("style", style), ("stability", stability)) if v is not None}
    if vs:
        body["voice_settings"] = vs
    r = _call("POST", "/api/generations", body)
    if r.get("duplicate_of"):  # the same script and voice are already queued or rendering
        return {"narration_id": r["duplicate_of"], "note": r.get("message")}
    return {"narration_id": r["id"], "parts": len(r.get("chunks", [])), "status": "queued"}


@mcp.tool()
def get_status(narration_id: str) -> dict:
    """Progress of a narration: status (queued, running, done, done_needs_listen, error, cancelled), parts done, and each part needing a listen with its text and reason."""
    g = _call("GET", f"/api/generations/{urllib.parse.quote(narration_id)}")
    done = sum(1 for c in g["chunks"] if c["status"] in ("ok", "flagged"))
    status = "running" if g.get("job") and g["job"].get("status") == "running" else ("queued" if g.get("job") else g["status"])
    out = {"status": status, "parts_done": done, "parts_total": len(g["chunks"]), "message": g.get("message", "")}
    if g["status"] == "done":  # "done" means every part passed; "done_needs_listen" means some parts are doubtful
        out["status"] = "done_needs_listen" if g.get("outcome") == "needs_listen" else "done"
        out["duration_seconds"] = g.get("duration")
        out["parts_needing_review"] = [{"part": r["part"], "text": r["text"], "reasons": r["reasons"]} for r in g.get("review") or []]
    return out


@mcp.tool()
def get_timings(narration_id: str) -> dict:
    """Start and end (seconds, in the finished file after speed) of every sentence and script word of a finished
    narration, for cutting video to the words. "estimated": true means some parts predate word timing and were
    shared out by character count."""
    return _call("GET", f"/api/generations/{urllib.parse.quote(narration_id)}/timings")


@mcp.tool()
def get_audio(narration_id: str, format: str = "mp3") -> dict:
    """Save a finished narration to ~/Downloads/VoiceForge and return the file path. format: "mp3" or "wav"."""
    fmt = "wav" if format.lower() == "wav" else "mp3"
    g = _call("GET", f"/api/generations/{urllib.parse.quote(narration_id)}")
    if g["status"] != "done" or not g.get("file"):
        raise RuntimeError(f"The narration is not finished yet (status: {g['status']}). Check get_status first.")
    DOWNLOADS.mkdir(parents=True, exist_ok=True)
    title = "".join(ch for ch in (g.get("title") or "Narration").replace("…", "") if ch.isalnum() or ch in " -_,.'").strip()[:60] or "Narration"
    dest = DOWNLOADS / f"{' '.join(g['profile_name'].split())} - {title} ({narration_id}).{fmt}"
    with urllib.request.urlopen(f"{_engine()}/api/generations/{narration_id}/audio?fmt={fmt}", timeout=120) as r, open(dest, "wb") as f:
        shutil.copyfileobj(r, f)
    return {"path": str(dest), "duration_seconds": g.get("duration"), "format": fmt}


if __name__ == "__main__":
    mcp.run()
