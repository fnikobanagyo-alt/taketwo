"""New-version check and in-place update from GitHub Releases.

The check asks GitHub once at launch whether a newer release exists (nothing about the user is sent).
The update runs as a worker job, so it waits for narrations ahead of it, then fast-forwards this
checkout to the release tag and runs `start.sh --setup`. If setup fails, the checkout goes back to
where it was. It refuses checkouts with local changes or commits, so a developer's tree is never touched.
The library (data/) is ignored by git and never changed.
"""
from __future__ import annotations

import json
import re
import subprocess
import threading
import time
import urllib.request

from . import config, db, worker

LATEST_URL = "https://api.github.com/repos/fnikobanagyo-alt/taketwo/releases/latest"  # fixed: never from settings or responses
RELEASES_URL = "https://github.com/fnikobanagyo-alt/taketwo/releases"
SETUP_CMD = ["./start.sh", "--setup"]
TAG = re.compile(r"^v(\d{1,4})\.(\d{1,4})\.(\d{1,4})$")

_lock = threading.Lock()
_check: dict | None = None  # last check result
_installed: str | None = None  # version installed and waiting for a restart


def parse(v: str) -> tuple[int, int, int] | None:
    m = TAG.match(v if v.startswith("v") else f"v{v}")
    return tuple(int(x) for x in m.groups()) if m else None


def _fetch_latest() -> dict:
    r = urllib.request.Request(LATEST_URL, headers={"User-Agent": "taketwo-update-check", "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(r, timeout=8) as resp:
        return json.loads(resp.read())


def check() -> dict:
    """Ask GitHub for the latest release. Offline, no releases yet, or odd replies all mean "no update"."""
    global _check
    result = {"checked_at": time.time(), "latest": None, "notes": "", "url": RELEASES_URL, "error": None}
    try:
        rel = _fetch_latest()
        tag = str(rel.get("tag_name", ""))
        if parse(tag) and not rel.get("draft") and not rel.get("prerelease"):
            result.update(latest=tag[1:], notes=str(rel.get("body") or "")[:4000], tag=tag)
    except Exception as e:  # noqa: BLE001  (404 = no release published yet)
        result["error"] = e.__class__.__name__
    with _lock:
        _check = result
    return result


def check_in_background() -> None:
    if db.get_settings().get("check_updates", True):
        threading.Thread(target=check, name="update-check", daemon=True).start()


def _job() -> dict | None:
    jobs = [j for j in worker.jobs.values() if j["kind"] == "update"]
    return worker.public(max(jobs, key=lambda j: j["created"])) if jobs else None


def blocking_reason() -> str | None:
    """Why new narrations must wait: an update is queued, running, or installed but not yet restarted."""
    j = _job()
    if j and j["status"] in ("queued", "running"):
        return "TakeTwo is updating. Start this narration after the update, or cancel the update in the sidebar."
    if _installed:
        return "An update is installed. Restart TakeTwo, then start this narration."
    return None


def state() -> dict:
    with _lock:
        c = dict(_check) if _check else None
    latest = c and c.get("latest")
    newer = bool(latest and parse(latest) and parse(latest) > parse(config.APP_VERSION))
    return {"current": config.APP_VERSION, "check": c, "available": newer and not _installed,
            "dismissed": db.get_settings().get("update_dismissed") == latest, "installed": _installed, "job": _job()}


# ------------------------------------------------------------------ the update itself
def _git(*args: str, timeout: float = 120) -> str:
    out = subprocess.run(["git", *args], cwd=config.ROOT, capture_output=True, text=True, timeout=timeout)
    if out.returncode:
        raise RuntimeError((out.stderr or out.stdout).strip()[-400:] or f"git {args[0]} failed")
    return out.stdout.strip()


def preflight() -> str | None:
    """A plain reason this copy can't update itself, or None."""
    if not (config.ROOT / ".git").exists():
        return f"This copy wasn't installed with git. Download the new version from {RELEASES_URL}."
    try:
        if _git("rev-parse", "--abbrev-ref", "HEAD") != "main":
            return "This copy isn't on the main branch, so it isn't updated automatically."
        if _git("status", "--porcelain", "--untracked-files=no"):
            return "TakeTwo's own files have local changes, so the update would overwrite them. Nothing was changed."
    except (RuntimeError, OSError, subprocess.TimeoutExpired) as e:
        return f"Couldn't read this copy's git status: {e}"
    return None


def _run_update(tag: str, progress, cancel) -> None:
    global _installed
    reason = preflight()
    if reason:
        raise RuntimeError(reason)
    old = _git("rev-parse", "HEAD")
    progress(0.1, f"Downloading {tag[1:]}")
    _git("fetch", "--no-tags", "origin", f"refs/tags/{tag}:refs/tags/{tag}", timeout=300)
    if cancel():
        return
    try:
        _git("merge", "--ff-only", tag)  # refuses if this copy has its own commits
    except RuntimeError:
        raise RuntimeError("This copy has its own changes that aren't in the release, so it wasn't updated. Nothing was changed.") from None
    progress(0.35, "Installing parts (a few minutes)")
    try:
        out = subprocess.run(SETUP_CMD, cwd=config.ROOT, capture_output=True, text=True, timeout=1800)
        if out.returncode:
            raise RuntimeError((out.stderr or out.stdout).strip()[-400:] or "setup failed")
    except (RuntimeError, OSError, subprocess.TimeoutExpired) as e:
        _git("reset", "--keep", old)  # back to the version that worked
        raise RuntimeError(f"Installing the new parts failed, so TakeTwo stayed on {config.APP_VERSION}. {e}") from None
    _installed = tag[1:]
    progress(1.0, f"{tag[1:]} is installed. Restart TakeTwo to use it.")


def start() -> dict:
    s = state()
    if not s["available"]:
        raise ValueError("No newer version to install.")
    j = s["job"]
    if j and j["status"] in ("queued", "running"):
        return j
    reason = preflight()
    if reason:
        raise ValueError(reason)
    tag = s["check"]["tag"]
    assert TAG.match(tag)  # only a validated vX.Y.Z ever reaches git
    return worker.submit("update", lambda progress, cancel: _run_update(tag, progress, cancel), ref="update", label=f"Updating to {tag[1:]}")
