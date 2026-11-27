"""Record what the Python engine answers over HTTP, for the Rust engine's parity test (step S06).

    backend/.venv/bin/python -B rust/golden/record_http.py

Seeds a fresh temporary library with contract/seed_python.py `seed()` (db.py's clock pinned, so `updated`
is the same on every run), starts the Python engine on it on a free dev port (8790-8795, 8798, 8799),
offline, sends the fixed list of requests below and writes rust/golden/http.json. Then it stops the
engine by its pid and deletes the library.

Every request carries an explicit Host header, so the file does not depend on the port.
Audio answers are recorded by byte length. A few answers hold machine-specific values (paths of this
computer, the process id, the engine list of the Python app); for those only the shape (top-level keys)
is recorded. `/api/catalog/library` is never requested: Python asks PyPI there.
Between the two phases the recorder soft-deletes `cttrash01` through db.py, as the Rust test does
through the library, so the soft-deleted 404s are covered without a write route.
"""
from __future__ import annotations

import http.client
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent
WT = HERE.parent.parent
BACKEND = WT / "backend"
OUT = HERE / "http.json"
PORTS = [8790, 8791, 8792, 8793, 8794, 8795, 8798, 8799]  # never 8765-8785 (Franz's app), not 8796/8797 (busy)
HOST = "127.0.0.1:8790"  # sent on every request; the guard compares Origin with it
CLOCK0 = 1800000000.0
HEADERS = ("content-type", "content-disposition", "cache-control", "x-preview-kind", "content-range")
SHAPE_ONLY = {"/api/engines", "/api/integrations", "/api/diagnostics"}

sys.path.insert(0, str(WT / "contract"))
import seed_python  # noqa: E402  (reused: seed() and wait_ready())

GENS = ["ctclean01", "ctreview01", "ctaccept01", "ctpending01", "cttrash01"]
VOICE = "ctvoice01"
TEXTS = {g["id"]: g["text"] for g in json.loads((WT / "contract" / "fixture.json").read_text())["generations"]}


def q(path: str, **params) -> str:
    import urllib.parse

    return path + "?" + urllib.parse.urlencode(params)


def cases_phase_a() -> list[dict]:
    c: list[dict] = []

    def add(method, path, body=None, headers=None):
        c.append({"method": method, "path": path, "headers": {"Host": HOST, **(headers or {})}, "body": body})

    for p in ("/api/health", "/api/pipeline-version", "/api/status", "/api/update", "/api/engines",
              "/api/integrations", "/api/diagnostics", "/api/settings"):
        add("GET", p)
    # voices
    add("GET", "/api/profiles")
    add("GET", f"/api/profiles/{VOICE}")
    add("GET", "/api/profiles/nosuchvoice")
    add("GET", f"/api/profiles/{VOICE}/analysis")
    add("GET", "/api/profiles/%2Ftmp/analysis")
    add("GET", f"/api/profiles/{VOICE}/preview")
    add("GET", "/api/profiles/nosuchvoice/preview")
    add("GET", f"/api/profiles/{VOICE}/audio/preview.wav")
    add("GET", f"/api/profiles/{VOICE}/audio/nosuch.wav")
    add("GET", f"/api/profiles/{VOICE}/audio/%2Fetc%2Fhosts")
    add("GET", "/api/profiles/bad.id/audio/preview.wav")
    add("GET", "/api/profiles/upgrade-all")
    # narrations: list, detail and every read route for each of the five
    add("GET", "/api/generations")
    add("GET", "/api/generations/nosuchid")
    add("GET", "/api/generations/reveal")
    for g in GENS:
        add("GET", f"/api/generations/{g}")
        add("GET", f"/api/generations/{g}/timings")
        add("GET", f"/api/generations/{g}/subtitles")
        add("GET", f"/api/generations/{g}/audio")
        add("GET", f"/api/generations/{g}/chunks/0/audio")
        add("GET", f"/api/generations/{g}/chunks/0/takes/1/audio")
    for g in ("ctclean01", "ctreview01"):
        for fmt in ("srt", "vtt", "txt", "xyz"):
            add("GET", q(f"/api/generations/{g}/subtitles", fmt=fmt))
    add("GET", "/api/generations/nosuchid/timings")
    add("GET", "/api/generations/nosuchid/audio")
    # audio
    add("GET", q("/api/generations/ctclean01/audio", download="true"))
    add("GET", q("/api/generations/ctreview01/audio", download="1"))
    add("GET", q("/api/generations/ctclean01/audio", download="false"))
    add("GET", q("/api/generations/ctclean01/audio", download="maybe"))
    add("GET", q("/api/generations/ctclean01/audio", fmt="mp3"))
    add("GET", q("/api/generations/ctclean01/audio", fmt="ogg", download="true"))
    add("GET", "/api/generations/ctclean01/audio", headers={"Range": "bytes=0-99"})
    add("GET", "/api/generations/ctclean01/audio", headers={"Range": "bytes=-10"})
    add("GET", "/api/generations/ctreview01/chunks/1/audio")
    add("GET", "/api/generations/ctreview01/chunks/7/audio")
    add("GET", "/api/generations/ctreview01/chunks/x/audio")
    add("GET", "/api/generations/ctreview01/chunks/-1/audio")
    add("GET", "/api/generations/%2Ftmp/chunks/0/audio")
    add("GET", "/api/generations/ctreview01/chunks/1/takes/1/audio")
    add("GET", "/api/generations/ctreview01/chunks/1/takes/9/audio")
    add("GET", "/api/generations/ctreview01/chunks/7/takes/1/audio")
    add("GET", "/api/generations/ctreview01/chunks/1/takes/x/audio")
    add("GET", "/api/generations/nosuchid/chunks/0/takes/1/audio")
    # search: every filter and limit
    for params in ({}, {"q": "Nile"}, {"q": "review"}, {"q": "RIVERS"}, {"status": "needs_review"}, {"status": "done"},
                   {"status": "cancelled"}, {"status": "error"}, {"voice": "contract voice"}, {"voice": VOICE.upper()},
                   {"voice": "nobody"}, {"same_script": "Welcome to the show"}, {"same_script": "   "},
                   {"limit": "2"}, {"limit": "0"}, {"limit": "500"}, {"limit": "abc"}, {"q": "the", "status": "done", "limit": "1"}):
        add("GET", q("/api/generations/search", **params))
    for g in GENS:
        add("GET", q("/api/generations/search", same_script=TEXTS[g]))
    # jobs, cancel, errors in FastAPI's shape
    add("GET", "/api/jobs/nosuchjob")
    add("POST", "/api/generations/nosuchid/cancel")
    add("POST", "/api/generations/ctclean01/cancel", headers={"Origin": "http://" + HOST})
    add("GET", "/api/nosuch")
    add("GET", "/nosuch")
    add("DELETE", "/api/health")
    add("POST", "/api/generations/search")
    # ids checked before anything else
    add("DELETE", "/api/profiles/bad.id")
    add("DELETE", "/api/generations/bad.id")
    add("DELETE", "/api/transcripts/bad.id")
    add("GET", "/api/designs/bad.id/audio/x.wav")
    # guards: hosts, origins, paths
    for host in ("evil.example", "127.0.0.1.evil.example:8766", "localhost:abc", "localhost:8790", "[::1]:8790", "localhost"):
        add("GET", "/api/health", headers={"Host": host})
    add("GET", "/api/nosuch", headers={"Host": "evil.example"})
    for origin in ("https://evil.example", "null", "http://127.0.0.1:3000", "http://localhost.evil.example:8766",
                   "http://localhost:5173", "http://127.0.0.1:8790"):
        add("POST", "/api/generations/nosuchid/cancel", headers={"Origin": origin})
    add("POST", "/api/generations/nosuchid/cancel", headers={"Host": "localhost:8790", "Origin": "http://127.0.0.1:8790"})
    add("GET", "/api/health", headers={"Origin": "https://evil.example"})
    for path in ("/api/generations/%2e%2e", "/api/profiles/%2e%2e", "/api/transcripts/%2e%2e", "/api/generations/a%2f..%2f..",
                 "/api/generations/a%5c..", "/api/generations/../x", "/api/nosuch/%2e", "/api/profiles/%2e%2e/audio/voiceforge.db",
                 "/api/generations/..%2f..%2fx/audio"):
        add("DELETE" if "audio" not in path else "GET", path)
    add("GET", "/x/%2e%2e")
    # settings: refused by the guard, refused by the rules, accepted
    add("PUT", "/api/settings", {"sentence_pause_ms": 350}, {"Origin": "https://evil.example"})
    add("PUT", "/api/settings", {"max_wer": 5})
    add("PUT", "/api/settings", {"sentence_pause_ms": "350", "not_a_setting": 1}, {"Origin": "http://" + HOST})
    add("GET", "/api/settings")
    return c


def cases_phase_b() -> list[dict]:
    """After cttrash01 is soft-deleted."""
    c = []
    for path in ("/api/generations/cttrash01", "/api/generations/cttrash01/timings", "/api/generations/cttrash01/subtitles",
                 "/api/generations/cttrash01/audio", "/api/generations/cttrash01/chunks/0/audio",
                 "/api/generations/cttrash01/chunks/0/takes/1/audio", "/api/generations", q("/api/generations/search", q="delete")):
        c.append({"method": "GET", "path": path, "headers": {"Host": HOST}, "body": None})
    return c


def send(port: int, case: dict) -> dict:
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
    data = json.dumps(case["body"]).encode() if case["body"] is not None else None
    headers = {**({"Content-Type": "application/json"} if data is not None else {}), **case["headers"]}
    conn.putrequest(case["method"], case["path"], skip_host=True, skip_accept_encoding=True)
    for k, v in headers.items():
        conn.putheader(k, v)
    if data is not None:
        conn.putheader("Content-Length", str(len(data)))
    conn.endheaders(data)
    r = conn.getresponse()
    raw = r.read()
    got = {k.lower(): v for k, v in r.getheaders()}
    conn.close()
    out = {"status": r.status, "headers": {h: got.get(h) for h in HEADERS}}
    ctype = got.get("content-type", "")
    if ctype.startswith("application/json"):
        body = json.loads(raw)
        if case["path"].split("?")[0] in SHAPE_ONLY and r.status == 200:
            out["shape"] = list((body[0] if isinstance(body, list) else body).keys())
            out["count"] = len(body) if isinstance(body, list) else None
        else:
            out["json"] = body
    elif ctype.startswith("text/"):
        out["text"] = raw.decode("utf-8")
    else:
        out["bytes"] = len(raw)
    return out


def pin_clock(db) -> None:
    t = [CLOCK0]

    def now() -> float:
        t[0] += 0.25
        return t[0]

    db.time = types.SimpleNamespace(time=now)


def free_port() -> int:
    for port in PORTS:
        with socket.socket() as s:
            try:
                s.bind(("127.0.0.1", port))
                return port
            except OSError:
                continue
    raise SystemExit("No free dev port.")


def main() -> int:
    tmp = Path(tempfile.mkdtemp(prefix="taketwo-http-"))
    proc = None
    try:
        data = tmp / "library"
        data.mkdir()
        os.environ["TAKETWO_DATA"] = os.environ["VOICEFORGE_DATA"] = str(data)
        os.environ["HF_HUB_OFFLINE"] = os.environ["TRANSFORMERS_OFFLINE"] = "1"
        sys.path.insert(0, str(BACKEND))
        from app import config, db

        assert config.DATA_DIR.resolve() == data.resolve(), config.DATA_DIR  # never the real library
        pin_clock(db)
        seed_python.seed(data)
        port = free_port()
        env = {**os.environ, "HF_HOME": str(tmp / "hf")}
        log = open(tmp / "engine.log", "w+b")
        proc = subprocess.Popen([sys.executable, "-B", "-m", "uvicorn", "app.main:app", "--host", "127.0.0.1", "--port", str(port)],
                                cwd=BACKEND, env=env, stdout=log, stderr=subprocess.STDOUT)
        seed_python.wait_ready(f"http://127.0.0.1:{port}", proc)
        phases = []
        for name, cases in (("a", cases_phase_a()), ("b", cases_phase_b())):
            if name == "b":
                assert db.set_generation_deleted("cttrash01", True)
            phases.append({"name": name, "setup": [] if name == "a" else [{"soft_delete": "cttrash01"}],
                           "cases": [{**c, "response": send(port, c)} for c in cases]})
        OUT.write_text(json.dumps({"clock_start": CLOCK0, "clock_step": 0.25, "phases": phases}, indent=1, ensure_ascii=False) + "\n")
        print(f"wrote {OUT.relative_to(WT)}: {sum(len(p['cases']) for p in phases)} cases")
        return 0
    finally:
        if proc is not None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
