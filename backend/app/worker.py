"""One background thread owns the GPU: all MLX work (STT, TTS, embeddings) runs here."""
from __future__ import annotations

import copy
import hashlib
import os
from contextlib import contextmanager
from pathlib import Path

import queue
import threading
import time
import traceback
import uuid

_q: "queue.Queue[dict]" = queue.Queue()
jobs: dict[str, dict] = {}
_idle_hooks: list = []  # run on this thread after the queue has been idle for `idle_after` seconds
idle_after = 0.0
_lock = threading.RLock()
_current = threading.local()
_started_at = time.time()
# Stamp the source present at process startup, not whatever is on disk at diagnosis time.
_source_sha256 = hashlib.sha256(b"".join(p.name.encode() + p.read_bytes() for p in sorted(Path(__file__).parent.glob("*.py")))).hexdigest()


def _event(job, event):
    events = job.setdefault("_events", [])
    now = time.time()
    events.append({"at": now, "event": event, "phase": job.get("phase")})
    if event in ("started", "phase_started", "phase_finished", "finished"):
        job["last_progress_at"] = now
    del events[:-32]


@contextmanager
def phase(name: str):
    """Annotate a blocking operation on the ML worker; a no-op outside that worker."""
    job = getattr(_current, "job", None)
    if job is None:
        yield
        return
    with _lock:
        previous = (job.get("phase"), job.get("phase_started_at"), job.get("_phase_clock"))
        job.update(phase=name, phase_started_at=time.time(), _phase_clock=time.monotonic())
        _event(job, "phase_started")
    try:
        yield
    finally:
        with _lock:
            _event(job, "phase_finished")
            job["phase"], job["phase_started_at"], job["_phase_clock"] = previous


def diagnostics() -> dict:
    """Bounded operational evidence; omit script, voice labels, callbacks and error text."""
    fields = ("id", "kind", "ref", "source", "status", "created", "started", "finished", "phase",
              "phase_started_at", "last_progress_at", "cancel_requested_at", "cancel_acknowledged_at", "error_type")
    with _lock:
        recent = sorted(jobs.values(), key=lambda j: ({"running": 2, "queued": 1}.get(j.get("status"), 0), j["created"]), reverse=True)[:20]
        result = []
        for job in recent:
            entry = {k: job.get(k) for k in fields}
            entry["phase_elapsed_seconds"] = round(max(0, time.monotonic() - job["_phase_clock"]), 3) if job.get("_phase_clock") is not None else None
            entry["events"] = copy.deepcopy(job.get("_events", []))
            result.append(entry)
    return {"runtime": {"pid": os.getpid(), "started_at": _started_at, "source_sha256": _source_sha256},
            "jobs": result, "job_limit": 20, "event_limit": 32}


def _cancel_requested(job):
    with _lock:
        if job["cancel"] and job.get("cancel_acknowledged_at") is None:
            job["cancel_acknowledged_at"] = time.time()
            _event(job, "cancel_acknowledged")
        return job["cancel"]


def on_idle(fn, after: float) -> None:
    global idle_after
    idle_after = after
    _idle_hooks.append(fn)


def submit(kind: str, fn, *, ref: str | None = None, label: str = "", on_cancel=None, source: str = "unknown") -> dict:
    jid = uuid.uuid4().hex[:10]
    job = {"id": jid, "kind": kind, "ref": ref, "label": label, "status": "queued", "progress": 0.0,
           "message": "Queued", "error": None, "created": time.time(), "cancel": False}
    job["_fn"] = fn
    job["_on_cancel"] = on_cancel  # runs when a job is cancelled before it starts
    job.update(source=source if source in ("api", "mcp", "desktop") else "unknown", phase="queued",
               phase_started_at=job["created"], last_progress_at=None, cancel_requested_at=None,
               cancel_acknowledged_at=None, _phase_clock=time.monotonic())
    with _lock:
        _event(job, "submitted")
        jobs[jid] = job
    _q.put(job)
    return public(job)


def public(job: dict) -> dict:
    with _lock:
        return {k: v for k, v in job.items() if not k.startswith("_")}


def cancel(jid: str) -> None:
    with _lock:
        job = jobs.get(jid)
        if job and job["status"] in ("queued", "running"):
            if not job["cancel"]:
                job["cancel_requested_at"] = time.time()
                _event(job, "cancel_requested")
            job["cancel"] = True
            if job["status"] == "queued":
                job["status"] = "cancelled"  # loop performs acknowledgement/cleanup when reached


def active_for(ref: str) -> dict | None:
    for j in jobs.values():
        if j["ref"] == ref and j["status"] in ("queued", "running"):
            return public(j)
    return None


def _loop():
    last_work, idle_done = time.time(), True
    while True:
        try:
            job = _q.get(timeout=min(15.0, idle_after) if idle_after > 0 else 15.0)
        except queue.Empty:
            # Models are freed on the thread that owns them, never while a job runs.
            if not idle_done and idle_after > 0 and time.time() - last_work >= idle_after:
                for fn in _idle_hooks:
                    try:
                        fn()
                    except Exception:  # noqa: BLE001
                        traceback.print_exc()
                idle_done = True
            continue
        idle_done = False
        if _cancel_requested(job):
            job["status"] = "cancelled"
            if job.get("_on_cancel"):
                try:
                    job["_on_cancel"]()
                except Exception:  # noqa: BLE001
                    traceback.print_exc()
            with _lock:
                job.update(finished=time.time(), phase="cancelled", _phase_clock=None)
                _event(job, "finished")
            continue
        job["status"] = "running"
        job["started"] = time.time()
        _current.job = job
        with _lock:
            job.update(phase="processing", phase_started_at=job["started"], _phase_clock=time.monotonic())
            _event(job, "started")

        def progress(p: float, msg: str = "", _job=job):
            with _lock:
                _job["progress"] = float(p)
                _job["last_progress_at"] = time.time()
                if msg:
                    _job["message"] = msg

        try:
            job["_fn"](progress=progress, cancel=lambda _job=job: _cancel_requested(_job))
            job["status"] = "cancelled" if _cancel_requested(job) else "done"
            job["progress"] = 1.0
        except Exception as e:  # noqa: BLE001
            traceback.print_exc()
            job["status"] = "error"
            job["error_type"] = e.__class__.__name__
            job["error"] = str(e) or e.__class__.__name__
            job["message"] = job["error"]
            if job.get("_on_error"):
                try:
                    job["_on_error"](job["error"])
                except Exception:
                    traceback.print_exc()
        with _lock:
            job.update(finished=time.time(), phase=job["status"], _phase_clock=None)
            _event(job, "finished")
        _current.job = None
        last_work = job["finished"]


threading.Thread(target=_loop, name="mlx-worker", daemon=True).start()
