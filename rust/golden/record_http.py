"""Record what the Python engine answers over HTTP, for the Rust engine's parity test (steps S06, S07a, S07b).

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

Step S07a adds write phases (c to f): accept and its undo, delete and restore, title and favourite edits,
and every refusal of the routes that would start a job, each change undone or followed by the read that
shows it. Setup steps between phases (a narration "running", a voice "analyzing", and back) are made
through db.py here and through the library in the Rust test. The engine runs as a child of this script
(`--serve`), with db.py's and pipeline.py's clocks pinned to one stepping counter (so `updated`,
`accepted.at` and `deleted_at` are the same on every run). Never recorded: reveal (it opens the file
manager), a speed change.

Step S07b adds job phases (g1 to g4) with the real worker.py. In the engine child, pipeline.run_generation,
regenerate_chunk, fix_remaining and select_take are replaced from the start by a gated fake that does what
the Rust StubRenderer does up to its gate: mark the narration running (its first part to do running) in
one save, then wait until `<gate>/go` exists, raising RuntimeError when `<gate>/fail` exists and marking
the narration cancelled when cancel is asked. engines.load raises, so no model can load. Job and narration
ids are pinned (uuid.uuid4 is a counter in the child). A case may carry `wait_before`: the recorder (and
the Rust replay) polls a GET until a key has a value, so each state exists before it is asked about.
In the job phases, clock values the worker and main.py take from time.time() (job times, event times,
the duplicate notice's `last`, the runtime block) are recorded as "<masked>" (null stays null); see MASKED
in the Rust test. No document is recorded after a stub render finished.
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
import time
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent
WT = HERE.parent.parent
BACKEND = WT / "backend"
OUT = HERE / "http.json"
PORTS = [8790, 8791, 8792, 8793, 8794, 8795, 8798, 8799]  # never 8765-8785 (Franz's app), not 8796/8797 (busy)
HOST = "127.0.0.1:8790"  # sent on every request; the guard compares Origin with it
CLOCK0 = 1800000000.0
ENGINE_CLOCK0 = 1900000000.0  # the engine child's own counter (a second process, as in the Rust test's second library handle)
HEADERS = ("content-type", "content-disposition", "cache-control", "x-preview-kind", "content-range")
JOB_TIMES = ("created", "started", "finished", "phase_started_at", "last_progress_at", "cancel_requested_at",
             "cancel_acknowledged_at", "phase_elapsed_seconds")
STUB_RUNNING = "Stub render"  # the Rust StubRenderer's texts (crates/taketwo-engine/src/render.rs)
STUB_FAILED = "Stub render failed."
STOPPED = "Stopped. Finished parts are kept; press Resume to finish the rest."
SHAPE_ONLY = {"/api/engines", "/api/integrations", "/api/diagnostics"}

sys.path.insert(0, str(WT / "contract"))
import seed_python  # noqa: E402  (reused: seed() and wait_ready())

GENS = ["ctclean01", "ctreview01", "ctaccept01", "ctpending01", "cttrash01"]
VOICE = "ctvoice01"
TEXTS = {g["id"]: g["text"] for g in json.loads((WT / "contract" / "fixture.json").read_text())["generations"]}
TITLES = {g["id"]: g["title"] for g in json.loads((WT / "contract" / "fixture.json").read_text())["generations"]}


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


def _case(method, path, body=None, headers=None) -> dict:
    return {"method": method, "path": path, "headers": {"Host": HOST, "Origin": "http://" + HOST, **(headers or {})}, "body": body}


def cases_phase_c() -> list[dict]:
    """Writes that start no job, from the app's own page (local Origin)."""
    w, G = _case, "/api/generations"
    title = TITLES["ctclean01"]
    return [
        # restore the narration phase b soft-deleted, then accept and its undo, each followed by the read
        w("POST", f"{G}/cttrash01/restore"), w("GET", f"{G}/cttrash01"),
        w("POST", f"{G}/ctaccept01/chunks/1/accept"), w("GET", f"{G}/ctaccept01"),
        w("DELETE", f"{G}/ctaccept01/chunks/1/accept"), w("GET", f"{G}/ctaccept01"),
        # the contract's three accept refusals, and other refusals
        w("POST", f"{G}/ctaccept01/chunks/0/accept"), w("POST", f"{G}/ctpending01/chunks/0/accept"),
        w("POST", f"{G}/ctaccept01/chunks/5/accept"), w("POST", f"{G}/ctaccept01/chunks/x/accept"),
        w("POST", f"{G}/ctaccept01/chunks/-1/accept"), w("POST", f"{G}/nosuchid/chunks/0/accept"),
        w("DELETE", f"{G}/ctaccept01/chunks/9/accept"), w("DELETE", f"{G}/nosuchid/chunks/0/accept"),
        # delete and restore
        w("DELETE", f"{G}/cttrash01"), w("GET", G), w("GET", q(f"{G}/search", q="delete")), w("GET", f"{G}/cttrash01"),
        w("POST", f"{G}/cttrash01/restore"), w("GET", f"{G}/cttrash01"),
        w("DELETE", f"{G}/nosuchid"), w("POST", f"{G}/nosuchid/restore"),
        # title and favourite
        w("PATCH", f"{G}/ctclean01", {"title": "Renamed"}), w("PATCH", f"{G}/ctclean01", {"favorite": True}),
        w("PATCH", f"{G}/ctclean01", {"title": "  Both at once  ", "favorite": False}),
        w("PATCH", f"{G}/ctclean01", {"title": "   "}), w("PATCH", f"{G}/ctclean01", {"title": "T" * 65 + " " + "é" * 64}),
        w("PATCH", f"{G}/ctclean01", {"speed": 1.25}), w("PATCH", f"{G}/ctclean01", {}),
        w("PATCH", f"{G}/ctclean01", {"favorite": "yes"}), w("PATCH", f"{G}/ctclean01", {"title": 5}),
        w("PATCH", f"{G}/ctclean01", {"favorite": "maybe", "title": None}), w("PATCH", f"{G}/ctclean01"),
        w("PATCH", f"{G}/ctclean01", {"title": title, "favorite": False}), w("GET", f"{G}/ctclean01"),
        w("PATCH", f"{G}/nosuchid", {}), w("PATCH", f"{G}/cttrash01", {"speed": 1.0}),
        # create: every refusal before the job
        w("POST", G, {"profile_id": "nosuchvoice", "text": "Hello."}),
        w("POST", G, {"profile_id": VOICE, "text": "Hello.", "engine": "nosuchmodel"}),
        w("POST", G, {"profile_id": VOICE, "text": "word " * 10_001}),
        w("POST", G, {"profile_id": VOICE, "text": "[1] [2]"}),
        w("POST", G, {"profile_id": VOICE, "text": "  \n "}),
        w("POST", G, {"profile_id": VOICE}), w("POST", G), w("POST", G, {"profile_id": 5, "text": ["x"], "params": 1}),
        w("GET", "/api/status"), w("GET", G),
        # re-speak, take switch, fix, resume: every refusal
        w("POST", f"{G}/nosuchid/chunks/0/regenerate", {}), w("POST", f"{G}/ctreview01/chunks/9/regenerate", {}),
        w("POST", f"{G}/ctreview01/chunks/1/regenerate", {"text": "[1] [2]"}),
        w("POST", f"{G}/ctreview01/chunks/1/regenerate", {"text": "x" * 300}),
        w("POST", f"{G}/ctreview01/chunks/x/regenerate"),
        w("POST", f"{G}/ctreview01/chunks/9/select-take", {"take": 1}),
        w("POST", f"{G}/ctreview01/chunks/1/select-take", {"take": 9}),
        w("POST", f"{G}/ctreview01/chunks/1/select-take", {"take": "x"}),
        w("POST", f"{G}/ctreview01/chunks/1/select-take", {}),
        w("POST", f"{G}/ctclean01/fix-remaining"), w("POST", f"{G}/nosuchid/fix-remaining"),
        w("POST", f"{G}/nosuchid/resume"), w("POST", f"{G}/cttrash01/chunks/0/accept"),
        w("GET", "/api/status"),
    ]


def cases_phase_d() -> list[dict]:
    """ctaccept01 stored as running: the busy refusals."""
    G = "/api/generations"
    return [_case("POST", f"{G}/ctaccept01/chunks/1/accept"), _case("DELETE", f"{G}/ctaccept01/chunks/1/accept"),
            _case("POST", f"{G}/ctaccept01/chunks/1/select-take", {"take": 1}), _case("GET", f"{G}/ctaccept01")]


def cases_phase_e() -> list[dict]:
    """ctaccept01 done again; the voice stored as still analyzing."""
    return [_case("POST", "/api/generations", {"profile_id": VOICE, "text": "Hello."}), _case("GET", f"/api/profiles/{VOICE}"),
            _case("GET", "/api/generations/ctaccept01")]


def cases_phase_f() -> list[dict]:
    """The voice ready again."""
    return [_case("GET", f"/api/profiles/{VOICE}"), _case("GET", "/api/profiles"), _case("GET", "/api/status")]


def job_id(n: int) -> str:
    """The n-th pinned uuid4 of the engine child, as a job id (hex[:10])."""
    return f"{n:010x}"


def gen_id(n: int) -> str:
    """The n-th pinned uuid4 of the engine child, as a narration id (hex[:12])."""
    return (f"{n:010x}" + f"{n:022x}")[:12]


def until(path: str, key: str, value) -> dict:
    return {"path": path, "key": key, "value": value}


SCRIPT1 = "The first job script. It has two sentences."
SCRIPT2 = "A second script, queued behind the first."
SCRIPT3 = "This one is told to fail."


def cases_phase_g1() -> list[dict]:
    """A running job at the closed gate and a second one queued behind it; every busy refusal; both cancelled."""
    w, G = _case, "/api/generations"
    n1, j1, n2, j2 = gen_id(1), job_id(2), gen_id(3), job_id(4)
    c = [w("POST", G, {"profile_id": VOICE, "text": SCRIPT1, "title": "Job one"})]
    c.append({**w("GET", f"{G}/{n1}"), "wait_before": until(f"{G}/{n1}", "status", "running")})
    c += [w("GET", "/api/status"), w("POST", G, {"profile_id": VOICE, "text": SCRIPT1}), w("GET", "/api/status"),
          w("POST", G, {"profile_id": VOICE, "text": SCRIPT2}, {"X-TakeTwo-Client": "mcp"}),
          w("GET", f"{G}/{n2}"), w("GET", f"/api/jobs/{j2}"), w("GET", f"/api/jobs/{j1}"),
          w("POST", G, {"profile_id": VOICE, "text": SCRIPT2}), w("GET", f"{G}/{n1}"), w("GET", f"{G}/{n2}"),
          # every busy refusal, on the running and on the queued narration
          w("POST", f"{G}/{n1}/resume"), w("POST", f"{G}/{n1}/chunks/0/regenerate", {}), w("POST", f"{G}/{n1}/fix-remaining"),
          w("POST", f"{G}/{n1}/chunks/0/select-take", {"take": 1}), w("POST", f"{G}/{n1}/chunks/0/accept"),
          w("DELETE", f"{G}/{n1}/chunks/0/accept"), w("PATCH", f"{G}/{n1}", {"speed": 1.3}),
          w("POST", f"{G}/{n2}/resume"), w("POST", f"{G}/{n2}/chunks/0/regenerate", {"text": "New words."}),
          w("POST", f"{G}/{n2}/fix-remaining"), w("PATCH", f"{G}/{n2}", {"speed": 0.8}),
          w("GET", "/api/status"),
          # cancel the queued one: cancelled at once
          w("POST", f"{G}/{n2}/cancel"), w("GET", f"{G}/{n2}"), w("GET", f"/api/jobs/{j2}"), w("GET", "/api/status"),
          # cancel the running one; read once the loop has reaped both
          w("POST", f"{G}/{n1}/cancel")]
    c.append({**w("GET", f"/api/jobs/{j2}"), "wait_before": until(f"/api/jobs/{j2}", "finished", "<set>")})
    c += [w("GET", f"/api/jobs/{j1}"), w("GET", f"{G}/{n1}"), w("GET", "/api/status")]
    return c


def cases_phase_g2() -> list[dict]:
    """The gate opened after the cancel: nothing changes."""
    G = "/api/generations"
    return [_case("GET", f"{G}/{gen_id(1)}"), _case("GET", f"/api/jobs/{job_id(2)}"), _case("GET", "/api/status")]


def cases_phase_g3() -> list[dict]:
    """The gate closed again and the fail switch on: a job that fails."""
    G = "/api/generations"
    n3, j3 = gen_id(5), job_id(6)
    return [_case("POST", G, {"profile_id": VOICE, "text": SCRIPT3}),
            {**_case("GET", f"/api/jobs/{j3}"), "wait_before": until(f"/api/jobs/{j3}", "finished", "<set>")},
            _case("GET", f"{G}/{n3}"), _case("GET", "/api/status")]


def cases_phase_g4() -> list[dict]:
    """Resume, re-speak, fix and take switch accepted (the job answers only), then all cancelled; diagnostics."""
    w, G = _case, "/api/generations"
    c = [w("POST", f"{G}/ctpending01/resume"), w("POST", f"{G}/ctreview01/chunks/1/regenerate", {}),
         w("POST", f"{G}/ctaccept01/fix-remaining"), w("POST", f"{G}/ctclean01/chunks/0/select-take", {"take": 1})]
    c.append({**w("GET", "/api/status"), "wait_before": until(f"{G}/ctpending01", "status", "running")})
    c += [w("POST", f"{G}/{g}/cancel") for g in ("ctclean01", "ctaccept01", "ctreview01", "ctpending01")]
    c.append({**w("GET", "/api/diagnostics"), "wait_before": until(f"/api/jobs/{job_id(10)}", "finished", "<set>")})
    c += [w("GET", "/api/status"), w("GET", f"/api/jobs/{job_id(7)}")]
    return c


JOB_PHASES = {"g1": cases_phase_g1, "g2": cases_phase_g2, "g3": cases_phase_g3, "g4": cases_phase_g4}


SETUP = {
    "a": [], "b": [{"soft_delete": "cttrash01"}],
    "c": [], "d": [{"set_status": ["generations", "ctaccept01", "running"]}],
    "e": [{"set_status": ["generations", "ctaccept01", "done"]}, {"set_status": ["profiles", VOICE, "analyzing"]}],
    "f": [{"set_status": ["profiles", VOICE, "ready"]}],
    "g1": [], "g2": [{"gate": "open"}], "g3": [{"gate": "close"}, {"fail": True}],
    "g4": [{"fail": False}, {"add_take": ["ctclean01", 0, 1]}],
}


def run_setup(db, step: dict, gate: Path) -> None:
    if "gate" in step:
        (gate / "go").touch() if step["gate"] == "open" else (gate / "go").unlink()
        return
    if "fail" in step:
        (gate / "fail").touch() if step["fail"] else (gate / "fail").unlink()
        return
    if "add_take" in step:  # a kept earlier take, so a take switch passes its checks (the fixture keeps none)
        gid, ci, take = step["add_take"]
        doc = db.get_generation(gid)
        doc["chunks"][ci]["takes"] = [{"take": take, "text": doc["chunks"][ci]["text"]}]
        db.put_generation(doc)
        return
    if "soft_delete" in step:
        assert db.set_generation_deleted(step["soft_delete"], True)
        return
    table, id_, status = step["set_status"]
    get, put = (db.get_generation, db.put_generation) if table == "generations" else (db.get_profile, db.put_profile)
    doc = get(id_)
    doc["status"] = status
    put(doc)


def mask(v):
    """Clock values of the worker and main.py, by position: job objects, events, notices, the runtime block."""
    if isinstance(v, list):
        return [mask(x) for x in v]
    if not isinstance(v, dict):
        return v
    job, notice, event, runtime = ("kind" in v and "phase" in v), ("count" in v and "last" in v), ("event" in v and "at" in v), "source_sha256" in v
    out = {}
    for k, x in v.items():
        hide = (job and k in JOB_TIMES) or (notice and k == "last") or (event and k == "at") or (runtime and k in ("pid", "started_at", "source_sha256"))
        out[k] = ("<masked>" if x is not None else None) if hide else mask(x)
    return out


def wait_for(port: int, w: dict) -> None:
    for _ in range(1000):  # 20 s
        r = send(port, _case("GET", w["path"]))
        v = r.get("json")
        for k in w["key"].split("."):
            v = v.get(k) if isinstance(v, dict) else None
        if (w["value"] == "<set>" and v is not None) or v == w["value"]:
            return
        time.sleep(0.02)
    raise SystemExit(f"timed out waiting for {w}")


def send(port: int, case: dict, masked: bool = False) -> dict:
    if case.get("wait_before"):
        wait_for(port, case["wait_before"])
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
        if case["path"].split("?")[0] in SHAPE_ONLY and r.status == 200 and not masked:
            out["shape"] = list((body[0] if isinstance(body, list) else body).keys())
            out["count"] = len(body) if isinstance(body, list) else None
        else:
            out["json"] = mask(body) if masked else body
    elif ctype.startswith("text/"):
        out["text"] = raw.decode("utf-8")
    else:
        out["bytes"] = len(raw)
    return out


def pin_clock(db, start: float = CLOCK0, pipeline=None) -> None:
    t = [start]

    def now() -> float:
        t[0] += 0.25
        return t[0]

    db.time = types.SimpleNamespace(time=now)
    if pipeline is not None:  # "Sounds fine" stamps with pipeline.time; the same counter, as the Rust library clock
        pipeline.time = types.SimpleNamespace(**{k: getattr(time, k) for k in ("sleep", "monotonic", "perf_counter")}, time=now)


def install_fakes(db, pipeline, engines, gate: Path) -> None:
    """The gated fake of the four pipeline calls (what the Rust StubRenderer does up to its gate)."""

    def no_model(*args, **kwargs):
        raise RuntimeError("record_http.py must never load a model")

    engines.load = no_model

    def fake(gid, first, cancel):
        g = db.get_generation(gid)
        g["status"], g["message"] = "running", STUB_RUNNING
        if first is None:
            first = next((c["i"] for c in g["chunks"] if c["status"] != "ok"), None)
        for c in g["chunks"]:
            if first is not None and c["i"] == first:
                c["status"] = "running"
        db.put_generation_live(g)
        while True:
            if (gate / "fail").exists():
                raise RuntimeError(STUB_FAILED)
            if cancel():
                g["status"], g["message"] = "cancelled", STOPPED
                for c in g["chunks"]:
                    if c["status"] == "running":
                        c["status"] = "pending"
                db.put_generation_live(g)
                return
            if (gate / "go").exists():
                return  # the Rust stub writes a finished narration here; never recorded
            time.sleep(0.02)

    pipeline.run_generation = lambda gid, cancel, only_chunks=None: fake(gid, only_chunks[0] if only_chunks else None, cancel)
    pipeline.regenerate_chunk = lambda gid, ci, text, cancel: fake(gid, ci, cancel)
    pipeline.fix_remaining = lambda gid, cancel: fake(gid, None, cancel)
    pipeline.select_take = lambda gid, ci, take: fake(gid, ci, lambda: False)  # main.py passes no cancel here


def pin_uuid() -> None:
    """uuid.uuid4 as a counter: narration ids (hex[:12]) and job ids (hex[:10]) are the same on every run."""
    import uuid

    n = [0]

    def uuid4():
        n[0] += 1
        return types.SimpleNamespace(hex=f"{n[0]:010x}{n[0]:022x}")

    uuid.uuid4 = uuid4


def serve_child(port: int) -> None:
    """The Python engine with pinned clocks and ids and a gated fake renderer, in this process (started by main())."""
    sys.path.insert(0, str(BACKEND))
    pin_uuid()
    from app import config, db, engines, pipeline

    assert config.DATA_DIR.resolve() == Path(os.environ["TAKETWO_DATA"]).resolve(), config.DATA_DIR
    pin_clock(db, ENGINE_CLOCK0, pipeline)
    install_fakes(db, pipeline, engines, Path(os.environ["TAKETWO_STUB_GATE"]))
    import uvicorn
    from app import main as engine

    uvicorn.run(engine.app, host="127.0.0.1", port=port, log_level="warning")


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
        gate = tmp / "gate"
        gate.mkdir()
        env = {**os.environ, "HF_HOME": str(tmp / "hf"), "TAKETWO_STUB_GATE": str(gate)}
        log = open(tmp / "engine.log", "w+b")
        proc = subprocess.Popen([sys.executable, "-B", str(Path(__file__).resolve()), "--serve", str(port)],
                                cwd=BACKEND, env=env, stdout=log, stderr=subprocess.STDOUT)
        seed_python.wait_ready(f"http://127.0.0.1:{port}", proc)
        phases = []
        for name, cases in (("a", cases_phase_a()), ("b", cases_phase_b()), ("c", cases_phase_c()), ("d", cases_phase_d()),
                            ("e", cases_phase_e()), ("f", cases_phase_f())):
            for step in SETUP[name]:
                run_setup(db, step, gate)
            phases.append({"name": name, "setup": SETUP[name], "cases": [{**c, "response": send(port, c)} for c in cases]})
        status_f = phases[-1]["cases"][-1]["response"]["json"]
        assert status_f["queue"] == [] and status_f["running"] is None, "a case before the job phases started a job"
        for name, cases in JOB_PHASES.items():
            for step in SETUP[name]:
                run_setup(db, step, gate)
            phases.append({"name": name, "masked": True, "setup": SETUP[name],
                           "cases": [{**c, "response": send(port, c, masked=True)} for c in cases()]})
        for p in phases:
            for c in p["cases"]:
                assert c["response"]["status"] != 500, c
        log.seek(0)
        assert b"must never load a model" not in log.read(), "a recorded request tried to load a model"
        OUT.write_text(json.dumps({"clock_start": CLOCK0, "engine_clock_start": ENGINE_CLOCK0, "clock_step": 0.25, "phases": phases},
                                  indent=1, ensure_ascii=False) + "\n")
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
    if sys.argv[1:2] == ["--serve"]:
        serve_child(int(sys.argv[2]))
    else:
        raise SystemExit(main())
