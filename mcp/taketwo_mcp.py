# /// script
# requires-python = ">=3.10"
# dependencies = ["mcp>=1.2,<2"]  # 2.x renamed FastMCP to MCPServer
# ///
"""TakeTwo (formerly VoiceForge) MCP server: lets Claude and other MCP clients use the local TakeTwo app.

Run: uv run --script taketwo_mcp.py   (stdio transport)
It talks to the running TakeTwo engine over its local HTTP API and opens the app if needed.
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
DOWNLOADS = Path.home() / "Downloads" / "TakeTwo"
mcp = FastMCP("taketwo")
_base: str | None = None


HEADERS = {"Content-Type": "application/json", "X-TakeTwo-Client": "mcp", "X-VoiceForge-Client": "mcp"}


def _q(segment) -> str:
    """Quote one URL path segment (narration ids come from the caller)."""
    return urllib.parse.quote(str(segment), safe="")


def _req(base: str, method: str, path: str, body: dict | None = None, timeout: float = 30):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(base + path, data=data, method=method, headers=HEADERS)
    with urllib.request.urlopen(r, timeout=timeout) as resp:
        return json.loads(resp.read())


def _is_taketwo(base: str) -> bool:
    try:  # other local servers answer on these ports too; check two TakeTwo-only endpoints
        return _req(base, "GET", "/api/health", timeout=1).get("ok") is True and "version" in _req(base, "GET", "/api/pipeline-version", timeout=1)
    except Exception:
        return False


def _engine() -> str:
    global _base
    pinned = os.environ.get("TAKETWO_URL") or os.environ.get("VOICEFORGE_URL")
    if pinned:  # pin a specific engine (tests, remote setups)
        return pinned.rstrip("/")
    if _base and _is_taketwo(_base):
        return _base
    for name in ("TakeTwo", "VoiceForge"):  # the engine writes its address here on start (older builds: VoiceForge only)
        try:
            published = json.loads((Path.home() / f"Library/Application Support/{name}/engine.json").read_text())["url"]
            if _is_taketwo(published):
                _base = published
                return _base
        except (OSError, ValueError, KeyError):
            pass
    for attempt in range(2):
        for p in PORTS:
            if _is_taketwo(f"http://127.0.0.1:{p}"):
                _base = f"http://127.0.0.1:{p}"
                return _base
        if attempt == 0:
            if subprocess.run(["open", "-g", "-a", "TakeTwo"], check=False).returncode:
                subprocess.run(["open", "-g", "-a", "VoiceForge"], check=False)  # not rebuilt yet
            for _ in range(60):  # the engine takes a few seconds to start
                time.sleep(1)
                if any(_is_taketwo(f"http://127.0.0.1:{p}") for p in PORTS):
                    break
    raise RuntimeError("TakeTwo is not running and could not be opened. Open the TakeTwo app, then try again.")


def _detail(e: urllib.error.HTTPError):
    """The "detail" of a TakeTwo error body: text, a dict, or None when the body is not JSON."""
    try:
        body = json.loads(e.read())
    except Exception:
        return None
    return body.get("detail") if isinstance(body, dict) else None


def _error(e: urllib.error.HTTPError, detail) -> RuntimeError:
    msg = detail.get("message") if isinstance(detail, dict) else detail
    return RuntimeError(f"TakeTwo returned {e.code}: {msg or e.reason or 'no details'}")


def _call(method: str, path: str, body: dict | None = None):
    try:
        return _req(_engine(), method, path, body)
    except urllib.error.HTTPError as e:
        detail = _detail(e)
        if isinstance(detail, dict) and detail.get("duplicate_of"):  # generate_narration reports the queued copy
            return {"error": e.code, **detail}
        raise _error(e, detail) from None


def _open(path: str, timeout: float = 30):
    """GET a non-JSON body (subtitles, audio). HTTP errors become the same RuntimeError as _call."""
    try:
        return urllib.request.urlopen(urllib.request.Request(_engine() + path, headers=HEADERS), timeout=timeout)
    except urllib.error.HTTPError as e:
        raise _error(e, _detail(e)) from None


def _part_index(part: int) -> int:
    """MCP parts are numbered from 1, like the app shows them; the engine counts from 0."""
    if isinstance(part, bool) or not isinstance(part, int) or part < 1:
        raise ValueError(f"part must be a whole number from 1 (got {part!r}). See parts_needing_review in get_status.")
    return part - 1


def _review(g: dict) -> list[dict]:
    keys = ("part", "text", "reasons", "issues", "start", "end", "checked_by")
    return [{k: r.get(k) for k in keys} for r in g.get("review") or []]


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
    """List the cloned voices in TakeTwo that are ready to use: id, name, language, description,
    speech_rate_wps (typical words per second, for estimating length) and studio."""
    return [{"id": v["id"], "name": " ".join(v["name"].split()), "language": v.get("language", "en"),
             "description": v.get("description"), "speech_rate_wps": v.get("speech_rate_wps"), "studio": v.get("studio")}
            for v in _call("GET", "/api/profiles") if v.get("status") == "ready"]


@mcp.tool()
def generate_narration(text: str, voice: str, speed: float = 1.0, style: float | None = None, stability: float | None = None) -> dict:
    """Start a narration of `text` in a TakeTwo voice (name or id from list_voices).

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
    """Progress of a narration.

    status: queued, running, done, done_needs_listen, error or cancelled (stopped; resume_narration continues it).
    review_status: the same, except a finished narration with doubtful parts is "done_needs_review".
    done_needs_listen is the legacy name of done_needs_review; prefer review_status.
    Also: parts_done/parts_total, eta_seconds (while rendering), queue_ahead (while queued), open_in_app
    (a link to the narration in the TakeTwo app) and, once done, parts_needing_review: each with its
    1-based part number, text, reasons, issues, start/end seconds in the finished audio and checked_by.
    Doubtful parts are still playable; ask the user to listen to them, or call respeak_part / fix_remaining.
    """
    g = _call("GET", f"/api/generations/{_q(narration_id)}")
    done = sum(1 for c in g["chunks"] if c["status"] in ("ok", "flagged"))
    job = g.get("job")
    status = "running" if job and job.get("status") == "running" else ("queued" if job else g["status"])
    out = {"status": status, "review_status": status, "parts_done": done, "parts_total": len(g["chunks"]),
           "message": g.get("message", ""), "eta_seconds": g.get("eta"), "queue_ahead": (g.get("queue") or {}).get("ahead"),
           "open_in_app": f"{_engine()}/#/history/{_q(narration_id)}"}
    if status == "done":  # "done" means every part passed; otherwise some parts are doubtful
        doubtful = g.get("outcome") == "needs_listen"
        out["status"] = "done_needs_listen" if doubtful else "done"
        out["review_status"] = "done_needs_review" if doubtful else "done"
        out["duration_seconds"] = g.get("duration")
        out["parts_needing_review"] = _review(g)
    return out


@mcp.tool()
def get_timings(narration_id: str) -> dict:
    """Start and end (seconds, in the finished file after speed) of every sentence and script word of a finished
    narration, for cutting video to the words. "estimated": true means some parts predate word timing and were
    shared out by character count."""
    return _call("GET", f"/api/generations/{_q(narration_id)}/timings")


@mcp.tool()
def get_audio(narration_id: str, format: str = "mp3", output_path: str | None = None, overwrite: bool = False) -> dict:
    """Save a finished narration and return the file path. format: "mp3" or "wav".

    By default the file goes to ~/Downloads/TakeTwo. output_path (optional) saves it there instead: it must end
    in .mp3 or .wav matching format; ~ is expanded and missing folders are created. An existing file is never
    replaced unless overwrite=True. The result includes outcome ("clean" or "needs_listen"); when it is
    "needs_listen" it also has a warning and parts_needing_review, which the user should hear before using it.
    """
    fmt = "wav" if format.lower() == "wav" else "mp3"
    dest = None
    if output_path:
        dest = Path(output_path).expanduser()
        if dest.suffix.lower() != f".{fmt}":
            raise ValueError(f"output_path must end in .{fmt} to match format={fmt!r} (got {output_path!r}).")
        if dest.exists() and not overwrite:
            raise FileExistsError(f"{dest} already exists. Choose another output_path or pass overwrite=True.")
    g = _call("GET", f"/api/generations/{_q(narration_id)}")
    if g["status"] != "done" or not g.get("file"):
        raise RuntimeError(f"The narration is not finished yet (status: {g['status']}). Check get_status first.")
    if dest is None:
        title = "".join(ch for ch in (g.get("title") or "Narration").replace("…", "") if ch.isalnum() or ch in " -_,.'").strip()[:60] or "Narration"
        dest = DOWNLOADS / f"{' '.join(g['profile_name'].split())} - {title} ({narration_id}).{fmt}"
    dest.parent.mkdir(parents=True, exist_ok=True)
    with _open(f"/api/generations/{_q(narration_id)}/audio?fmt={fmt}", timeout=120) as r, open(dest, "wb") as f:
        shutil.copyfileobj(r, f)
    out = {"path": str(dest), "duration_seconds": g.get("duration"), "format": fmt, "outcome": g.get("outcome")}
    if g.get("outcome") == "needs_listen":
        out["warning"] = "Some parts failed a quality check. Ask the user to listen to parts_needing_review before using this audio."
        out["parts_needing_review"] = _review(g)
    return out


@mcp.tool()
def get_subtitles(narration_id: str, format: str = "srt") -> str:
    """Subtitles of a finished narration as text. format: "srt", "vtt" or "txt" (the spoken script without timings)."""
    fmt = format.lower()
    if fmt not in ("srt", "vtt", "txt"):
        raise ValueError(f'format must be "srt", "vtt" or "txt" (got {format!r}).')
    with _open(f"/api/generations/{_q(narration_id)}/subtitles?fmt={fmt}") as r:
        return r.read().decode("utf-8")


@mcp.tool()
def find_narration(text: str | None = None, voice: str | None = None, status: str | None = None) -> list[dict]:
    """Search past narrations, newest first. All filters are optional.

    text: words from the title or script. voice: a voice name or id.
    status: needs_review, done, running, queued, error or cancelled.
    Each result has id (use it as narration_id), title, voice, status, outcome, created, duration and source.
    """
    params = {k: v for k, v in (("q", text), ("voice", voice), ("status", status)) if v}
    return _call("GET", "/api/generations/search?" + urllib.parse.urlencode(params))


@mcp.tool()
def respeak_part(narration_id: str, part: int, text: str | None = None) -> dict:
    """Record one part of a narration again, keeping every other part as it is.

    part: the 1-based number from parts_needing_review in get_status. text (optional): new wording for that
    part; leave unset to re-record the same words. Fails while the narration is busy. Poll get_status afterwards.
    """
    ci = _part_index(part)
    _call("POST", f"/api/generations/{_q(narration_id)}/chunks/{ci}/regenerate", {"text": text} if text else {})
    return {"narration_id": narration_id, "part": part, "status": "queued"}


@mcp.tool()
def fix_remaining(narration_id: str) -> dict:
    """Re-record every part of a narration that still needs review (the app's Fix button). Poll get_status afterwards.
    Fails with "Nothing left to fix." when no part needs it."""
    _call("POST", f"/api/generations/{_q(narration_id)}/fix-remaining")
    return {"narration_id": narration_id, "status": "queued"}


@mcp.tool()
def stop_narration(narration_id: str) -> dict:
    """Stop a queued or running narration. Finished parts are kept; resume_narration continues it later."""
    _call("POST", f"/api/generations/{_q(narration_id)}/cancel")
    return {"narration_id": narration_id, "status": "stopping"}


@mcp.tool()
def resume_narration(narration_id: str) -> dict:
    """Continue a stopped or failed narration from its unfinished parts. Fails if it is already running."""
    _call("POST", f"/api/generations/{_q(narration_id)}/resume")
    return {"narration_id": narration_id, "status": "queued"}


@mcp.tool()
def mark_sounds_fine(narration_id: str, part: int, undo: bool = False) -> dict:
    """Mark a part as checked by a person, so it no longer needs review. undo=True removes the mark.

    Use this ONLY when the user has listened to that part and says it sounds fine. Never call it on your own
    judgement or because a measurement looks good: you cannot hear the audio.
    part: the 1-based number from parts_needing_review in get_status.
    """
    ci = _part_index(part)
    _call("DELETE" if undo else "POST", f"/api/generations/{_q(narration_id)}/chunks/{ci}/accept")
    return {"narration_id": narration_id, "part": part, "sounds_fine": not undo}


if __name__ == "__main__":
    mcp.run()
