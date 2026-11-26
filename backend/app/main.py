"""TakeTwo (formerly VoiceForge) API server."""
from __future__ import annotations

import json
import re
import shutil
import threading
import time
import uuid
from pathlib import Path

from fastapi import FastAPI, File, Form, HTTPException, UploadFile, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from . import catalog, db, engines, pipeline, reference, tools, updates, worker
from .textproc import clean_text
from .tools import attachment_header
from .config import APP_VERSION, IDLE_UNLOAD_SECONDS, PIPELINE_VERSION, DESIGNS_DIR, FRONTEND_DIST, GENERATIONS_DIR, MAX_REFERENCE_SECONDS, PROFILES_DIR, TRANSCRIPTS_DIR

MAX_SCRIPT_WORDS = 10_000

app = FastAPI(title="TakeTwo", version=APP_VERSION)
app.add_middleware(CORSMiddleware, allow_origins=["http://localhost:5173"], allow_methods=["*"], allow_headers=["*"])
LOCAL_HOSTS = {"127.0.0.1", "localhost", "[::1]"}


def _host_name(host: str) -> str:
    return host.rsplit(":", 1)[0] if re.search(r":\d+$", host) else host


@app.middleware("http")
async def only_this_computer(request: Request, call_next):
    """The API has no password, so it answers only this computer: the Host must be local (stops DNS
    rebinding), and a web page may change things only if this engine served it. Tools without an
    Origin header (MCP server, curl, other local apps) still work."""
    host, origin = request.headers.get("host", ""), request.headers.get("origin")
    if _host_name(host) not in LOCAL_HOSTS:
        return JSONResponse({"detail": "TakeTwo only answers this computer."}, 403)
    if origin and request.method not in ("GET", "HEAD", "OPTIONS") and origin not in (f"http://{host}", "http://localhost:5173"):
        return JSONResponse({"detail": "Requests from other websites are not allowed."}, 403)
    if request.url.path.startswith("/api/") and any(p in (".", "..") for p in re.split(r"[/\\\\]", request.scope["path"])):
        return JSONResponse({"detail": "Invalid path."}, 400)
    return await call_next(request)


def _item_dir(root: Path, item_id: str) -> Path:
    """The folder of one saved item. Anything but a plain id is refused, so no request can reach outside `root`."""
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,64}", item_id):
        raise HTTPException(400, "Invalid id")
    return root / item_id

engines.set_custom_engines(db.get_settings().get("custom_engines", []))


def _release_models() -> None:
    from . import ml

    ml.unload()
    engines.unload()  # also runs gc and clears the MLX buffer cache
    import gc

    gc.collect()
    try:
        import mlx.core as mx

        mx.clear_cache()
    except Exception:  # noqa: BLE001
        pass
    print(f"Idle for {IDLE_UNLOAD_SECONDS:.0f} s: released the voice and speech models.", flush=True)


worker.on_idle(_release_models, IDLE_UNLOAD_SECONDS)

# Written under both names while other apps still look for VoiceForge.
ENGINE_FILES = [Path.home() / "Library" / "Application Support" / n / "engine.json" for n in ("TakeTwo", "VoiceForge")]


def _publish_address() -> None:
    """Tell other apps (such as the MCP server) where this engine listens; the port varies 8766–8785."""
    import os
    import sys

    argv = sys.argv
    if "--port" not in argv or "uvicorn" not in " ".join(argv):
        return  # imported by tests or tools, not serving
    host = argv[argv.index("--host") + 1] if "--host" in argv else "127.0.0.1"
    port = int(argv[argv.index("--port") + 1])
    if not 8766 <= port <= 8785:
        return  # dev or preview engines (e.g. 8790) must not redirect other apps away from the desktop app
    url = f"http://{host}:{port}"
    info = json.dumps({"url": url, "pid": os.getpid(), "started": time.time()})
    for f in ENGINE_FILES:
        try:
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text(info)
        except OSError:
            pass  # discovery falls back to scanning the port range
    updates.check_in_background()  # only the desktop/start.sh engine asks GitHub; tests and dev engines never do


_publish_address()

# Anything left "running" from a previous session was interrupted.
for _g in db.list_generations():
    if _g["status"] in ("queued", "running"):
        _g["status"] = "interrupted"
        _g["message"] = "Interrupted (app was closed). Press Resume to finish the remaining chunks."
        for _c in _g["chunks"]:
            if _c["status"] == "running":
                _c["status"] = "pending"
        db.put_generation(_g)
for _p in db.list_profiles():
    if _p.get("status") in ("queued", "analyzing"):
        _p["status"] = "error"
        _p["error"] = "Analysis was interrupted. Press Re-analyze."
        db.put_profile(_p)
for _d in tools.TRANSCRIPTS_DIR.iterdir() if tools.TRANSCRIPTS_DIR.exists() else []:
    _doc = tools.transcript_doc(_d.name) if _d.is_dir() else None
    if _doc and _doc.get("status") in ("queued", "running"):
        _doc["status"] = "error"
        _doc["error"] = "Transcription was interrupted. Press Retry."
        tools.save_transcript_doc(_doc)


def _404(what="Not found"):
    raise HTTPException(404, what)


# ------------------------------------------------------------------ settings


@app.get("/api/health")
def health():
    return {"ok": True, "name": "TakeTwo", "formerly": "VoiceForge", "version": APP_VERSION, "loaded_engine": engines._loaded["id"]}


# Same voice and same script while it is queued or rendering: the second copy is refused.
_create_lock = threading.Lock()
_dup_notices: dict[str, dict] = {}  # narration id -> {"count", "last"}; shown while it is in the queue


def _active_duplicate(profile_id: str, cleaned: str) -> dict | None:
    for j in _active_jobs():
        if j["kind"] != "generate" or not j.get("ref"):
            continue
        g = db.get_generation(j["ref"]) or {}
        if g.get("profile_id") == profile_id and g.get("cleaned_text") == cleaned:
            return {"gid": j["ref"], "status": j["status"], "title": _narration_progress(j["ref"])["title"]}
    return None


def _narration_progress(gid: str) -> dict:
    """Real progress of a narration (the job itself only knows it started)."""
    g = db.get_generation(gid) or {}
    chunks = g.get("chunks") or []
    done = sum(1 for c in chunks if c.get("status") in ("ok", "flagged"))
    return {"title": g.get("title") or g.get("text_preview") or (g.get("text") or "")[:60],
            "done": done, "total": len(chunks), "gen_message": g.get("message") or "",
            "progress": round(done / len(chunks), 4) if chunks else 0.0}


def _describe_job(j: dict) -> dict:
    d = worker.public(j)
    if j["kind"] == "generate" and j.get("ref"):
        info = _narration_progress(j["ref"])
        d.update(gid=j["ref"], title=info["title"], done=info["done"], total=info["total"],
                 progress=info["progress"], message=info["gen_message"] or d["message"])
    return d


def _active_jobs() -> list[dict]:
    jobs = [j for j in worker.jobs.values() if j["status"] in ("running", "queued")]
    return sorted(jobs, key=lambda j: j["created"])  # the order the worker takes them


@app.get("/api/status")
def status():
    queue = [_describe_job(j) for j in _active_jobs()]
    running = next((q for q in queue if q["status"] == "running"), None)
    active = {q["gid"] for q in queue if q.get("gid")}
    duplicates = [{"gid": gid, "title": _narration_progress(gid)["title"], **n} for gid, n in _dup_notices.items() if gid in active]
    return {"loaded_engine": engines._loaded["id"], "running": running, "queued": sum(1 for q in queue if q["status"] == "queued"),
            "queue": queue, "duplicates": duplicates, "max_script_words": MAX_SCRIPT_WORDS}


@app.get("/api/update")
def update_state():
    return updates.state()


@app.post("/api/update/check")
def update_check():
    updates.check()
    return updates.state()


@app.post("/api/update/start")
def update_start():
    try:
        updates.start()
    except ValueError as e:
        raise HTTPException(409, str(e)) from None
    return updates.state()


@app.post("/api/update/cancel")
def update_cancel():
    j = updates._job()
    if not j or j["status"] != "queued":
        raise HTTPException(409, "Only a queued update can be cancelled; one that has started finishes or rolls back on its own.")
    worker.cancel(j["id"])
    return updates.state()


@app.post("/api/update/restart")
def update_restart():
    """Restart this engine in place (same process id and port), so the desktop window and start.sh both keep working."""
    if _active_jobs():
        raise HTTPException(409, "Something is still running. Restart when the queue is empty.")
    import os
    import sys

    def _exec():
        time.sleep(0.6)  # let this reply reach the window first
        os.execv(sys.executable, [sys.executable, *sys.orig_argv[1:]])

    threading.Thread(target=_exec, daemon=True).start()
    return {"restarting": True}


@app.get("/api/diagnostics")
def diagnostics():
    return worker.diagnostics()


@app.get("/api/settings")
def get_settings():
    return db.get_settings()


@app.put("/api/settings")
def put_settings(patch: dict):
    try:
        return db.save_settings(patch)
    except ValueError as e:
        raise HTTPException(400, str(e))


# ------------------------------------------------------------------ engines

_downloads: dict[str, dict] = {}


@app.get("/api/engines")
def list_engines():
    out = []
    for e in engines.all_engines():
        d = e.public()
        d["loaded"] = engines._loaded["id"] == e.id
        d["download"] = _downloads.get(e.repo)
        out.append(d)
    return out


def _start_download(repo: str, size_gb: float):
    if repo in _downloads and _downloads[repo]["status"] == "running":
        return _downloads[repo]
    st = {"status": "running", "error": None, "expected": int(size_gb * 1e9), "started": time.time()}
    _downloads[repo] = st

    def run():
        try:
            engines.download(repo)
            st["status"] = "done"
        except Exception as e:  # noqa: BLE001
            st["status"] = "error"
            st["error"] = str(e)

    threading.Thread(target=run, daemon=True).start()
    return st


def _spec_or_404(engine_id: str) -> engines.EngineSpec:
    try:
        return engines.get_spec(engine_id)
    except KeyError:
        raise HTTPException(404, "Unknown model") from None


@app.post("/api/engines/{engine_id}/download")
def download_engine(engine_id: str):
    spec = _spec_or_404(engine_id)
    return _start_download(spec.repo, spec.size_gb)


@app.get("/api/engines/{engine_id}/download")
def download_progress(engine_id: str):
    spec = _spec_or_404(engine_id)
    st = _downloads.get(spec.repo) or {"status": "done" if engines.is_downloaded(spec.repo) else "idle"}
    have = engines.repo_cache_bytes(spec.repo)
    exp = st.get("expected") or int(spec.size_gb * 1e9) or 1
    return {**st, "bytes": have, "progress": min(0.99 if st["status"] == "running" else 1.0, have / exp)}


class CustomEngine(BaseModel):
    repo: str
    name: str | None = None


def _custom_engine_id(repo: str, items: list[dict]) -> str:
    """Stable id from the full owner/repo, so two models with the same name never collide."""
    existing = next((c["id"] for c in items if c["repo"] == repo), None)
    if existing:
        return existing
    base = "custom-" + re.sub(r"[^a-z0-9]+", "-", repo.lower()).strip("-")
    taken, eid, n = {c["id"] for c in items}, base, 2
    while eid in taken:
        eid, n = f"{base}-{n}", n + 1
    return eid


@app.post("/api/engines/custom")
def add_custom(body: CustomEngine):
    s = db.get_settings()
    items = [c for c in s["custom_engines"] if c["repo"] != body.repo]
    eid = _custom_engine_id(body.repo, s["custom_engines"])
    items.append({"id": eid, "repo": body.repo, "name": body.name or body.repo.split("/")[-1], "family": engines.guess_family(body.repo)})
    db.save_settings({"custom_engines": items})
    engines.set_custom_engines(items)
    return {"id": eid}


@app.delete("/api/engines/custom/{engine_id}")
def remove_custom(engine_id: str):
    s = db.get_settings()
    items = [c for c in s["custom_engines"] if c["id"] != engine_id]
    db.save_settings({"custom_engines": items})
    engines.set_custom_engines(items)
    return {"ok": True}


@app.get("/api/catalog/library")
def library():
    return catalog.library_status()


_upgrade = {"status": "idle", "message": ""}


@app.post("/api/catalog/upgrade")
def upgrade():
    if _upgrade["status"] == "running":
        return _upgrade

    def run():
        _upgrade.update(status="running", message="Upgrading mlx-audio…")
        try:
            v = catalog.upgrade_library()
            _upgrade.update(status="done", message=f"Installed mlx-audio {v}. Restart TakeTwo to use it.")
        except Exception as e:  # noqa: BLE001
            _upgrade.update(status="error", message=str(e))

    threading.Thread(target=run, daemon=True).start()
    return _upgrade


@app.get("/api/catalog/upgrade")
def upgrade_status():
    return _upgrade


@app.get("/api/catalog/discover")
def discover(sort: str = "lastModified"):
    known = {e.repo for e in engines.all_engines()}
    try:
        return catalog.discover(known, sort=sort)
    except Exception as e:  # noqa: BLE001
        raise HTTPException(502, f"Could not reach Hugging Face: {e}")


# ------------------------------------------------------------------ profiles


def _run_analysis(pid: str, src: Path, denoise: bool, window: float, studio_mode: bool = False):
    def fn(progress, cancel):
        p = db.get_profile(pid)
        p["status"] = "analyzing"
        pipeline.drop_preview(p)
        db.put_profile(p)
        try:
            a = reference.analyze(src, PROFILES_DIR / pid, language=p.get("language") or "en", denoise=denoise, target_window=window, progress=progress, studio_mode=studio_mode)
        except Exception as e:
            p = db.get_profile(pid)
            p.update(status="error", error=str(e))
            db.put_profile(p)
            raise
        utts = a["utterances"]
        speech = sum(u["dur"] for u in utts) or 1
        p = db.get_profile(pid)
        p.pop("noise_baseline", None)
        p.pop("pitch_hz", None)  # re-measured from the new sample before the next narration
        p.update(
            status="ready", error=None, duration=a["duration"], snr_db=a["snr_db"], denoised=a["denoised"],
            self_similarity=a["self_similarity"], usable_speech_seconds=a["usable_speech_seconds"],
            speech_rate_wps=round(sum(u["n_words"] for u in utts) / speech, 3), variants=a["variants"],
            active_variant=0, ref_text_override={}, tips=a["tips"], transcript=a["transcript"],
            studio=a["studio"], room_echo_db=a["room_echo_db"], room_echo_after_db=a["room_echo_after_db"],
            pipeline_version=PIPELINE_VERSION,
        )
        db.put_profile(p)

    job = worker.submit("analyze", fn, ref=pid, label="Analyzing voice")
    queue_preview(pid)  # runs right after the analysis in the same queue
    return job


def _preview_job(pid: str):
    def run(progress, cancel):
        if pipeline.make_preview(pid, cancel) == "stale" and db.get_profile(pid):
            # the voice changed while this preview was being made: make one from the new settings
            worker.submit("preview", _preview_job(pid), ref=f"{pid}:preview", label="Making voice preview")
    return run


def queue_preview(pid: str):
    ref = f"{pid}:preview"
    if worker.active_for(ref):
        return None
    return worker.submit("preview", _preview_job(pid), ref=ref, label="Making voice preview")


# Existing voices without a clone-spoken preview get one in the background.
for _p in db.list_profiles():
    if _p.get("status") == "ready" and not (PROFILES_DIR / _p["id"] / "preview.wav").exists():
        queue_preview(_p["id"])


@app.get("/api/profiles")
def list_profiles():
    return db.list_profiles()


@app.post("/api/profiles")
async def create_profile(
    name: str = Form(...), language: str = Form("en"), denoise: bool = Form(False), studio: bool = Form(True),
    file: UploadFile = File(...),
):
    pid = uuid.uuid4().hex[:12]
    pdir = PROFILES_DIR / pid
    pdir.mkdir(parents=True)
    ext = Path(file.filename or "upload.wav").suffix.lower() or ".wav"
    src = pdir / f"original{ext}"
    with src.open("wb") as f:
        shutil.copyfileobj(file.file, f)
    dur = _probe_duration(src)
    p = {"id": pid, "name": name.strip() or "Untitled voice", "language": language, "status": "queued",
         "original_file": src.name, "original_duration": dur, "truncated": bool(dur and dur > MAX_REFERENCE_SECONDS)}
    db.put_profile(p)
    p["studio"] = studio
    db.put_profile(p)
    job = _run_analysis(pid, src, denoise, 12.0, studio)
    return {**p, "job": job}


def _probe_duration(path: Path) -> float | None:
    import subprocess

    try:
        r = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)], capture_output=True, text=True)
        return round(float(r.stdout.strip()), 2)
    except Exception:
        return None


@app.get("/api/profiles/{pid}")
def get_profile(pid: str):
    p = db.get_profile(pid) or _404()
    p["job"] = worker.active_for(pid)
    return p


@app.get("/api/profiles/{pid}/analysis")
def get_analysis(pid: str):
    f = PROFILES_DIR / pid / "analysis.json"
    if not f.exists():
        _404()
    return JSONResponse(json.loads(f.read_text()))


class ProfilePatch(BaseModel):
    name: str | None = None
    description: str | None = None
    active_variant: int | None = None
    ref_text: str | None = None
    default_engine: str | None = None


@app.patch("/api/profiles/{pid}")
def patch_profile(pid: str, body: ProfilePatch):
    p = db.get_profile(pid) or _404()
    if body.name is not None:
        if not body.name.strip():
            raise HTTPException(400, "Name cannot be empty")
        p["name"] = body.name.strip()[:60]
    if body.description is not None:
        p["description"] = body.description.strip()[:300]
    stale = False
    if body.active_variant is not None:
        if not 0 <= body.active_variant < len(p.get("variants", [])):
            raise HTTPException(400, "Bad variant")
        stale |= body.active_variant != p.get("active_variant", 0)
        p["active_variant"] = body.active_variant
    if body.ref_text is not None:
        p.setdefault("ref_text_override", {})[str(p.get("active_variant", 0))] = body.ref_text.strip()
        stale = True
    if body.default_engine is not None:
        stale |= body.default_engine != p.get("default_engine")
        p["default_engine"] = body.default_engine
    if stale:  # what the voice sounds like changed -> re-make its preview
        pipeline.drop_preview(p)
    p = db.put_profile(p)
    if stale and p.get("status") == "ready":
        queue_preview(pid)
    return p


@app.get("/api/profiles/{pid}/preview")
def profile_preview(pid: str):
    """Clone-spoken sample if ready, otherwise the reference clip the clone learns from."""
    p = db.get_profile(pid) or _404()
    pdir = PROFILES_DIR / pid
    f = pdir / "preview.wav"
    kind = "generated"
    if not f.exists():
        kind = "reference"
        vs = p.get("variants") or []
        f = pdir / vs[p.get("active_variant", 0)]["file"] if vs else pdir / "clean.wav"
    if not f.exists():
        _404("No audio for this voice yet")
    return FileResponse(f, media_type="audio/wav", headers={"X-Preview-Kind": kind, "Cache-Control": "no-store"})


class Reanalyze(BaseModel):
    denoise: bool = False
    target_window: float = 12.0
    studio: bool | None = None


@app.post("/api/profiles/{pid}/reanalyze")
def reanalyze(pid: str, body: Reanalyze):
    p = db.get_profile(pid) or _404()
    p["status"] = "queued"
    db.put_profile(p)
    studio_mode = p.get("studio", False) if body.studio is None else body.studio
    return _run_analysis(pid, PROFILES_DIR / pid / p["original_file"], body.denoise, max(6.0, min(25.0, body.target_window)), studio_mode)


class UpgradeAll(BaseModel):
    mode: str = "refresh"  # refresh = keep choices, re-cut clips | full = re-analyze from scratch
    studio: bool = True
    only_outdated: bool = True


def _refresh_voice(pid: str):
    """Light upgrade, safe for saved voices: nothing you chose or edited changes."""
    def fn(progress, cancel):
        p = db.get_profile(pid)
        progress(0.1, "Re-cutting reference clips at natural pauses")
        rep = reference.refresh_clips(PROFILES_DIR / pid, p)  # updates p["variants"] timing only
        p["pipeline_version"] = PIPELINE_VERSION
        p["refresh_report"] = rep
        p.pop("noise_baseline", None)
        pipeline.drop_preview(p)
        db.put_profile(p)
        queue_preview(pid)

    return worker.submit("refresh", fn, ref=pid, label="Updating voice")


@app.post("/api/profiles/upgrade-all")
def upgrade_all(body: UpgradeAll):
    """Bring saved voices up to the latest pipeline. Default is the safe 'refresh': only the
    internal reference clips are re-cut; names, descriptions, chosen clip, transcript edits
    and model choices stay exactly as they are."""
    queued = []
    for p in db.list_profiles():
        if p.get("status") != "ready" or not p.get("original_file"):
            continue
        if body.only_outdated and p.get("pipeline_version", 0) >= PIPELINE_VERSION:
            continue
        if body.mode == "full":
            p["studio"] = body.studio
            p["status"] = "queued"
            db.put_profile(p)
            _run_analysis(p["id"], PROFILES_DIR / p["id"] / p["original_file"], False, 12.0, body.studio)
        else:
            _refresh_voice(p["id"])
        queued.append(p["name"])
    return {"queued": queued}


@app.post("/api/profiles/{pid}/refresh")
def refresh_profile(pid: str):
    p = db.get_profile(pid) or _404()
    if p.get("status") != "ready" or not (PROFILES_DIR / pid / "clean.wav").exists():
        raise HTTPException(400, "This voice isn't ready to update.")
    if worker.active_for(pid):
        raise HTTPException(409, "This voice is busy.")
    return _refresh_voice(pid)


@app.get("/api/integrations")
def integrations():
    """Is the TakeTwo MCP server registered in Claude Desktop and Claude Code? (read-only)"""
    import shlex

    home = Path.home()
    script = Path(__file__).resolve().parents[2] / "mcp" / "taketwo_mcp.py"
    uv = shutil.which("uv") or str(home / ".local" / "bin" / "uv")

    def registered(cfg: Path) -> bool:
        try:
            return bool({"taketwo", "voiceforge"} & set(json.loads(cfg.read_text()).get("mcpServers") or {}))
        except (OSError, ValueError):
            return False

    desktop_cfg = home / "Library" / "Application Support" / "Claude" / "claude_desktop_config.json"
    desktop_snippet = json.dumps({"mcpServers": {"taketwo": {"command": uv, "args": ["run", "--script", str(script)]}}}, indent=2)
    return {
        "script_found": script.exists(),
        "apps": [
            {"id": "claude-desktop", "name": "Claude Desktop", "connected": registered(desktop_cfg),
             "how": f"Add this to {desktop_cfg} (merge into an existing \"mcpServers\"), then quit and reopen Claude Desktop.",
             "setup": desktop_snippet},
            {"id": "claude-code", "name": "Claude Code", "connected": registered(home / ".claude.json"),
             "how": "Run this once in Terminal, then start a new Claude Code session.",
             "setup": f"claude mcp add --scope user taketwo -- uv run --script {shlex.quote(str(script))}"},
        ],
    }


@app.get("/api/pipeline-version")
def pipeline_version():
    return {"version": PIPELINE_VERSION}


@app.delete("/api/profiles/{pid}")
def delete_profile(pid: str):
    pdir = _item_dir(PROFILES_DIR, pid)
    for ref in (pid, f"{pid}:preview"):
        j = worker.active_for(ref)
        if j:
            worker.cancel(j["id"])
    db.delete_profile(pid)
    shutil.rmtree(pdir, ignore_errors=True)
    return {"ok": True}


@app.get("/api/profiles/{pid}/audio/{name}")
def profile_audio(pid: str, name: str):
    f = (_item_dir(PROFILES_DIR, pid) / name).resolve()
    if f.parent != (PROFILES_DIR / pid).resolve() or not f.exists():
        _404()
    return FileResponse(f)


# ------------------------------------------------------------------ generations


class GenRequest(BaseModel):
    profile_id: str
    text: str
    engine: str | None = None
    language: str | None = None
    params: dict = {}
    speed: float = 1.0
    voice_settings: dict = {}


def _mark_cancelled_before_start(gid: str) -> None:
    g = db.get_generation(gid)
    if g and g.get("status") in ("queued", "running"):
        g.update(status="cancelled", message="Cancelled before it started")
        for c in g["chunks"]:
            if c["status"] == "running":
                c["status"] = "pending"
        db.put_generation_live(g)


def _submit_generation(gid: str, only: list[int] | None = None, regen: tuple[int, str | None] | None = None, fix: bool = False, source: str = "api"):
    def fn(progress, cancel):
        try:
            if regen is not None:
                pipeline.regenerate_chunk(gid, regen[0], regen[1], cancel)
            elif fix:
                pipeline.fix_remaining(gid, cancel)
            else:
                pipeline.run_generation(gid, cancel, only_chunks=only)
        except Exception as e:
            # A re-speak restores its own previous state; everything else is marked as an error.
            g = db.get_generation(gid)
            if g and regen is None:
                g.update(status="error", message=str(e))
                for c in g["chunks"]:
                    if c["status"] == "running":
                        c["status"] = "pending"
                db.put_generation_live(g)
            raise

    return worker.submit("generate", fn, ref=gid, label="Generating", source=source, on_cancel=lambda: _mark_cancelled_before_start(gid))


@app.post("/api/generations")
def create_generation(req: GenRequest, request: Request = None):
    p = db.get_profile(req.profile_id) or _404("Voice not found")
    if p.get("status") != "ready":
        raise HTTPException(400, "This voice is still being analyzed.")
    if (why := updates.blocking_reason()):
        raise HTTPException(409, why)
    s = db.get_settings()
    engine = req.engine or p.get("default_engine") or s["default_engine"]
    _spec_or_404(engine)
    words = len(req.text.split())
    if words > MAX_SCRIPT_WORDS:
        raise HTTPException(400, f"Script is {words:,} words. The limit is {MAX_SCRIPT_WORDS:,} per generation; split it into parts.")
    cleaned = clean_text(req.text, s.get("pronunciations"), s.get("strip_citations", True))
    with _create_lock:  # check and add in one step, so two quick clicks cannot both get in
        dup = _active_duplicate(p["id"], cleaned)
        if dup:
            n = _dup_notices.setdefault(dup["gid"], {"count": 0, "last": 0.0})
            n["count"] += 1
            n["last"] = time.time()
            where = "rendering now" if dup["status"] == "running" else "waiting its turn"
            raise HTTPException(409, detail={
                "message": f"Already in the queue: this narration is {where}. It wasn't added twice.",
                "duplicate_of": dup["gid"], "status": dup["status"], "title": dup["title"]})
        try:
            g = pipeline.create_generation(p, req.text, engine, req.language or p.get("language") or "en", req.params, s, req.speed, req.voice_settings)
        except ValueError as e:
            raise HTTPException(400, str(e))
        if request and "mcp" in (request.headers.get("X-TakeTwo-Client"), request.headers.get("X-VoiceForge-Client")):
            job = _submit_generation(g["id"], source="mcp")
        else:
            job = _submit_generation(g["id"])
    return {**g, "job": job}


@app.get("/api/generations")
def list_generations():
    out = []
    for g in db.list_generations():
        extra = {"outcome": pipeline.outcome(g), "review_count": len(pipeline.fix_targets(g)) if g.get("status") == "done" else 0}
        g = {k: v for k, v in g.items() if k not in ("chunks", "cleaned_text")} | extra
        g["text_preview"] = g.pop("text", "")[:240]
        out.append(g)
    return out


@app.get("/api/generations/{gid}")
def get_generation(gid: str):
    g = db.get_generation(gid) or _404()
    g["outcome"] = pipeline.outcome(g)
    g["review"] = pipeline.review(g) if g["outcome"] else []
    g["job"] = worker.active_for(gid)
    if g["job"] and gid in _dup_notices:
        g["duplicate_notice"] = _dup_notices[gid]
    if g["job"] and g["job"]["status"] == "queued":
        # what this narration is waiting for, in the order the worker takes them
        mine = g["job"]["created"]
        ahead = [j for j in _active_jobs() if j["created"] < mine]
        g["queue"] = {"ahead": len(ahead), "waiting_for": [
            _narration_progress(j["ref"])["title"] if j["kind"] == "generate" and j.get("ref") else j["label"] for j in ahead[:3]]}
    return g


@app.post("/api/generations/{gid}/cancel")
def cancel_generation(gid: str):
    j = worker.active_for(gid)
    if j:
        worker.cancel(j["id"])
        if j["status"] == "queued":
            _mark_cancelled_before_start(gid)  # a queued narration stops now, not when its turn would come
    return {"ok": True}


@app.post("/api/generations/{gid}/resume")
def resume_generation(gid: str):
    db.get_generation(gid) or _404()
    if worker.active_for(gid):
        raise HTTPException(409, "Already running")
    return _submit_generation(gid)


class RegenBody(BaseModel):
    text: str | None = None


@app.post("/api/generations/{gid}/chunks/{ci}/regenerate")
def regenerate(gid: str, ci: int, body: RegenBody):
    g = db.get_generation(gid) or _404()
    if not 0 <= ci < len(g["chunks"]):
        _404("No such chunk")
    if worker.active_for(gid):
        raise HTTPException(409, "This generation is busy")
    if body.text is not None and body.text.strip():
        try:
            pipeline.clean_replacement(body.text, g, db.get_settings())  # reject before anything changes
        except ValueError as e:
            raise HTTPException(400, str(e))
    return _submit_generation(gid, regen=(ci, body.text))


@app.post("/api/generations/{gid}/fix-remaining")
def fix_remaining(gid: str):
    g = db.get_generation(gid) or _404()
    if worker.active_for(gid):
        raise HTTPException(409, "This generation is busy")
    if not pipeline.fix_targets(g):
        raise HTTPException(400, "Nothing left to fix.")
    return _submit_generation(gid, fix=True)


class GenPatch(BaseModel):
    title: str | None = None
    favorite: bool | None = None
    speed: float | None = None


@app.patch("/api/generations/{gid}")
def patch_generation(gid: str, body: GenPatch):
    g = db.get_generation(gid) or _404()
    fields = {}
    if body.title is not None and body.title.strip():
        fields["title"] = body.title.strip()[:120]
    if body.favorite is not None:
        fields["favorite"] = body.favorite
    if body.speed is not None and abs(body.speed - float(g.get("speed") or 1.0)) > 1e-3:
        if worker.active_for(gid):
            raise HTTPException(409, "This generation is busy")
        try:
            pipeline.retime(g, max(0.7, min(1.3, body.speed)))
        except ValueError as e:
            raise HTTPException(400, str(e))
        db.put_generation(g)  # no render is running, so the whole document is safe to write
    doc = db.patch_generation_meta(gid, fields) if fields else db.get_generation(gid)
    return doc or _404()


@app.get("/api/generations/{gid}/subtitles")
def subtitles(gid: str, fmt: str = "srt"):
    from fastapi.responses import Response

    g = db.get_generation(gid) or _404()
    if not g.get("timeline"):
        _404("Subtitles are available once the audio is rendered")
    caps = tools.generation_captions(g)
    body = {"srt": tools.to_srt, "vtt": tools.to_vtt, "txt": tools.to_txt}.get(fmt, tools.to_srt)(caps)
    name = f"{g.get('title') or g['id']}".replace("/", "-").replace("…", "")[:60].strip()
    return Response(body, media_type="text/plain; charset=utf-8",
                    headers={"Content-Disposition": attachment_header(name, fmt)})


@app.get("/api/generations/{gid}/timings")
def generation_timings(gid: str):
    g = db.get_generation(gid) or _404()
    if not g.get("timeline"):
        _404("Timings are available once the audio is rendered")
    return tools.generation_timings(g)


@app.delete("/api/generations/{gid}")
def delete_generation(gid: str):
    gdir = _item_dir(GENERATIONS_DIR, gid)
    j = worker.active_for(gid)
    if j:
        worker.cancel(j["id"])  # a running render stops at its next save and cannot recreate the row
    db.delete_generation(gid)
    shutil.rmtree(gdir, ignore_errors=True)
    return {"ok": True}


@app.get("/api/generations/{gid}/audio")
def generation_audio(gid: str, fmt: str = "wav", download: bool = False):
    g = db.get_generation(gid) or _404()
    f = GENERATIONS_DIR / gid / ("final.mp3" if fmt == "mp3" else "final.wav")
    if not f.exists():
        _404("Not rendered yet")
    import re as _re

    title = _re.sub(r"[^\w\s\-',.!?]", "", (g.get("title") or "Narration").replace("…", "")).strip()[:60].rstrip(" .,")
    fname = f"{g['profile_name']} - {title or 'Narration'}.{fmt}"
    return FileResponse(f, filename=fname if download else None)


@app.get("/api/generations/{gid}/chunks/{ci}/audio")
def chunk_audio(gid: str, ci: int):
    f = GENERATIONS_DIR / gid / f"chunk_{ci:03d}.wav"
    if not f.exists():
        _404()
    return FileResponse(f, headers={"Cache-Control": "no-store"})


@app.get("/api/jobs/{jid}")
def get_job(jid: str):
    j = worker.jobs.get(jid) or _404()
    return worker.public(j)


# ------------------------------------------------------------------ speech to text


@app.post("/api/transcripts")
async def create_transcript(language: str = Form(""), file: UploadFile = File(...)):
    doc = tools.new_transcript(file.filename or "audio.wav")
    with (TRANSCRIPTS_DIR / doc["id"] / doc["file"]).open("wb") as f:
        shutil.copyfileobj(file.file, f)
    tid = doc["id"]
    doc["language_hint"] = language or None
    tools.save_transcript_doc(doc)
    worker.submit("transcribe", lambda progress, cancel: tools.run_transcription(tid, language or None, progress), ref=tid, label="Transcribing")
    return doc


@app.post("/api/transcripts/{tid}/retry")
def retry_transcript(tid: str):
    d = tools.transcript_doc(tid) or _404()
    if worker.active_for(tid):
        raise HTTPException(409, "Already running")
    if d.get("status") != "error":
        raise HTTPException(400, "Nothing to retry.")
    if not (TRANSCRIPTS_DIR / tid / d["file"]).exists():
        raise HTTPException(400, "The uploaded file is no longer there. Upload it again.")
    language = d.get("language_hint")
    d.update(status="queued", error=None)
    tools.save_transcript_doc(d)
    worker.submit("transcribe", lambda progress, cancel: tools.run_transcription(tid, language, progress), ref=tid, label="Transcribing")
    return d


@app.get("/api/transcripts")
def list_transcripts():
    return tools.list_transcripts()


@app.get("/api/transcripts/{tid}")
def get_transcript(tid: str):
    d = tools.transcript_doc(tid) or _404()
    d["job"] = worker.active_for(tid)
    return d


@app.get("/api/transcripts/{tid}/export")
def export_transcript(tid: str, fmt: str = "srt"):
    from fastapi.responses import Response

    d = tools.transcript_doc(tid) or _404()
    caps = d.get("captions") or []
    body = {"srt": tools.to_srt, "vtt": tools.to_vtt}.get(fmt, lambda c: d.get("text", ""))(caps)
    return Response(body, media_type="text/plain; charset=utf-8",
                    headers={"Content-Disposition": attachment_header(d["name"], fmt)})


@app.get("/api/transcripts/{tid}/audio")
def transcript_audio(tid: str):
    f = TRANSCRIPTS_DIR / tid / "audio.wav"
    return FileResponse(f) if f.exists() else _404()


@app.delete("/api/transcripts/{tid}")
def delete_transcript(tid: str):
    shutil.rmtree(_item_dir(TRANSCRIPTS_DIR, tid), ignore_errors=True)
    return {"ok": True}


# ------------------------------------------------------------------ voice design


class DesignReq(BaseModel):
    description: str
    text: str | None = None
    language: str = "en"
    count: int = 3


@app.post("/api/designs")
def create_design(req: DesignReq):
    if len(req.description.strip()) < 8:
        raise HTTPException(400, "Describe the voice in a few more words.")
    doc = tools.new_design(req.description, req.text, req.language, req.count)
    did = doc["id"]
    worker.submit("design", lambda progress, cancel: tools.run_design(did, progress, cancel), ref=did, label="Designing voice")
    return doc


@app.get("/api/designs/{did}")
def get_design(did: str):
    d = tools.design_doc(did) or _404()
    d["job"] = worker.active_for(did)
    return d


@app.get("/api/designs/{did}/audio/{name}")
def design_audio(did: str, name: str):
    f = (_item_dir(DESIGNS_DIR, did) / name).resolve()
    if f.parent != (DESIGNS_DIR / did).resolve() or not f.exists():
        _404()
    return FileResponse(f)


class DesignSave(BaseModel):
    index: int
    name: str


@app.post("/api/designs/{did}/save")
def save_design(did: str, body: DesignSave):
    d = tools.design_doc(did) or _404()
    prev = next((p for p in d["previews"] if p["index"] == body.index), None) or _404("No such preview")
    pid = uuid.uuid4().hex[:12]
    pdir = PROFILES_DIR / pid
    pdir.mkdir(parents=True)
    src = pdir / "original.wav"
    shutil.copyfile(DESIGNS_DIR / did / prev["file"], src)
    p = {"id": pid, "name": body.name.strip() or "Designed voice", "language": d["language"], "status": "queued",
         "original_file": src.name, "original_duration": prev["duration"], "truncated": False,
         "description": d["description"], "designed": True}
    db.put_profile(p)
    job = _run_analysis(pid, src, False, min(12.0, max(6.0, prev["duration"] * 0.7)), False)
    return {**p, "job": job}


# ------------------------------------------------------------------ frontend

if FRONTEND_DIST.exists():
    app.mount("/assets", StaticFiles(directory=FRONTEND_DIST / "assets"), name="assets")

    @app.get("/{path:path}")
    def spa(path: str):
        root = FRONTEND_DIST.resolve()
        f = (root / path).resolve()
        # only files inside the built frontend; "../" paths must never reach the rest of the disk
        if path and f.is_file() and f.is_relative_to(root):
            return FileResponse(f)
        # index.html names the current hashed bundle; a cached copy kept showing an old UI after updates
        return FileResponse(root / "index.html", headers={"Cache-Control": "no-cache"})
