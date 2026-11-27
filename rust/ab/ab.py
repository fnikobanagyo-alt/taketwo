"""Blind A/B listening test: the Python app against the Rust engine on the same 10 scripts (rust/PLAN.md S20).

    python3 rust/ab/ab.py plan [--refresh] [--source URL] [--out DIR]
    python3 rust/ab/ab.py render --franz-ok N [--source URL] [--out DIR] [--engine-a-cmd CMD] [--engine-b-cmd CMD]
    python3 rust/ab/ab.py page [--out DIR]
    python3 rust/ab/ab.py selftest

Standard library only. Everything stored goes under the git-ignored rust/ab/private/ (or --out DIR).
The source app (Franz's running app unless --source says otherwise) is only ever sent GET requests, by
source_get() and nothing else. Standard output never shows a title, script text, voice name or id.

THE RULE (plan applies it in this order):
 1. status is "done";
 2. engine is "qwen3-tts-1.7b";
 3. outcome is "clean";
 4. an ordinary narration. Nothing to filter: create_generation (backend/app/pipeline.py) writes source "app",
    "mcp" or "api" only (older rows have none, which main.py reads as "api"); voice previews live on the voice
    (pipeline.make_preview) and a re-speak changes a part of an existing narration (regenerate_chunk), so
    neither is ever a narration of its own;
 5. its voice still exists and is "ready" in GET /api/profiles;
 6. 2 to 5 parts (chunks);
 7. no two with the same script after lower-casing and squeezing spaces (the newest one is kept);
 8. newest first ("created"), ties by id.
Pick 3 with 2 parts, 4 with 3 parts, 3 with 4-5 parts. Within a bucket voices take turns (round-robin, in order
of each voice's newest script) and at most 5 voices are used in all. A short bucket is filled from the nearest
bucket (2 and 4-5 from 3; 3 from 2, then 4-5) and plan says so.
--selftest-rule (selftest only: the contract fixture has 4 finished narrations of 1 voice) drops filter 3,
lets filter 6 take 1 part (counted in the 2-part bucket), and accepts fewer than 10 scripts.

THE TAKE BOUND (synthesis calls, what TAKETWO_MAX_TAKES counts), from backend/app/pipeline.py and config.py:
  per part   render_chunk: max_attempts + compare_takes - 1 takes (the script's stored settings; config.py
             defaults 4 and 1 when a stored value is missing); the plain fallback take after the loop only runs
             when every take failed the ending guard, which is off;
           + run_generation's final check re-records each flagged part once, one more render_chunk:
           = 2 * (max_attempts + compare_takes - 1)
  per voice  + 3 when the voice has no preview file: main.py queues make_preview at startup (range(3) takes);
             its "stale" re-queue needs a voice edit during the preview, which render never makes
  per engine E = sum of the above; N = 2 * E (two engines); each engine runs with TAKETWO_MAX_TAKES = E.
  The bound holds only with experimental_qwen_ending_guard off: with it on, _guarded_take makes up to 4 takes per
  sentence per take. That setting comes from the temp library (default False, config.py), not from the script;
  render checks it on each engine before each script and stops if it is on.
"""
from __future__ import annotations

import argparse
import base64
import html
import json
import os
import random
import re
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
WT = HERE.parent.parent
PRIVATE = HERE / "private"
DEFAULT_SOURCE = "http://127.0.0.1:8766"  # Franz's running app: GET only, through source_get()
DEV_PORTS = (8790, 8791, 8792, 8793, 8794, 8795, 8798, 8799)  # 8796 and 8797 serve pages
ENGINE = "qwen3-tts-1.7b"
DEFAULT_MAX_ATTEMPTS, DEFAULT_COMPARE_TAKES = 4, 1  # backend/app/config.py DEFAULTS
PREVIEW_TAKES = 3  # pipeline.make_preview: for k in range(3)
WANT = (("2", 3), ("3", 4), ("4-5", 3))
NEAREST = {"2": ("3",), "3": ("2", "4-5"), "4-5": ("3",)}
MAX_VOICES = 5
# Voice files the render path reads by a fixed name, not named in the profile document (profile_assets,
# noise_limit, delivery.backfill, assemble). Copied when the source has them; a 404 is skipped.
FIXED_FILES = ("fingerprint.npy", "mic.json", "clean.wav")
SETTING_KEYS = ("max_chunk_chars", "max_attempts", "compare_takes", "max_wer", "min_speaker_similarity",
                "max_internal_silence", "sentence_pause_ms", "paragraph_pause_ms", "target_lufs", "seed")
ENGINE_A = "{wt}/backend/.venv/bin/python -B -m uvicorn app.main:app --host 127.0.0.1 --port {port}"
ENGINE_B = "{wt}/rust/target/release/taketwo-engine serve --port {port}"
STUB_ENGINE = "{wt}/rust/target/debug/taketwo-engine serve --stub-renderer --port {port}"
ENGINE_WORDS = ("python", "rust", "taketwo-engine", "uvicorn", "mlx", "candle", "engine a", "engine b")


# ------------------------------------------------------------------ talking to the source (GET only)

def source_get(base: str, path: str) -> tuple[int, bytes]:
    """The only code that talks to the source app. GET only. Errors never print the URL (it holds ids)."""
    req = urllib.request.Request(base.rstrip("/") + path, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, b""
    except (urllib.error.URLError, OSError):
        return 0, b""


def source_json(base: str, path: str):
    code, body = source_get(base, path)
    if code != 200:
        raise SystemExit(f"The source app answered {code or 'nothing'} to a GET; plan needs it running.")
    return json.loads(body)


def wait_source_idle(base: str) -> None:
    while True:
        st = source_json(base, "/api/status")
        if st.get("running") is None and st.get("queued") == 0:
            return
        print("The source app is busy; waiting 30 s.", flush=True)
        time.sleep(30)


# ------------------------------------------------------------------ plan

def norm(text: str) -> str:
    return " ".join((text or "").lower().split())


def bucket_of(parts: int, selftest: bool) -> str | None:
    if parts == 1 and selftest:
        return "2"
    return {2: "2", 3: "3", 4: "4-5", 5: "4-5"}.get(parts)


def pick(cands: list[dict], want: int, voices: list[str], taken: set[str]) -> list[dict]:
    """Round-robin over voices (in order of their newest script), at most MAX_VOICES voices in all."""
    queues: dict[str, list[dict]] = {}
    for c in cands:
        if c["id"] not in taken:
            queues.setdefault(c["profile_id"], []).append(c)
    out: list[dict] = []
    while len(out) < want:
        moved = False
        for v, q in queues.items():
            if len(out) >= want:
                break
            if not q or (v not in voices and len(voices) >= MAX_VOICES):
                continue
            out.append(q.pop(0))
            taken.add(out[-1]["id"])
            if v not in voices:
                voices.append(v)
            moved = True
        if not moved:
            break
    return out


def select(source: str, selftest: bool) -> dict:
    profiles = {p["id"]: p for p in source_json(source, "/api/profiles") if p.get("status") == "ready"}
    rows = [g for g in source_json(source, "/api/generations")
            if g.get("status") == "done" and g.get("engine") == ENGINE
            and (selftest or g.get("outcome") == "clean") and g.get("profile_id") in profiles]
    rows.sort(key=lambda g: g["id"])
    rows.sort(key=lambda g: -(g.get("created") or 0))  # stable: newest first, ties by id
    seen, buckets = set(), {b: [] for b, _ in WANT}
    for row in rows:
        g = source_json(source, f"/api/generations/{urllib.parse.quote(row['id'], safe='')}")
        b = bucket_of(len(g.get("chunks") or []), selftest)
        key = norm(g.get("text"))
        if b is None or key in seen:
            continue
        seen.add(key)
        buckets[b].append(g)
    voices: list[str] = []
    taken: set[str] = set()
    chosen = {b: pick(buckets[b], n, voices, taken) for b, n in WANT}
    fills = []
    for b, n in WANT:
        for nb in NEAREST[b]:
            short = n - len(chosen[b])
            if short > 0:
                extra = pick(buckets[nb], short, voices, taken)
                chosen[b] += extra
                if extra:
                    fills.append({"bucket": b, "from": nb, "count": len(extra)})
        if n - len(chosen[b]) > 0:
            fills.append({"bucket": b, "from": None, "count": 0, "short": n - len(chosen[b])})
    scripts = []
    for b, _ in WANT:
        for g in chosen[b]:
            scripts.append({
                "ordinal": len(scripts) + 1, "bucket": b, "id": g["id"], "title": g.get("title"),
                "text": g.get("text"), "cleaned_text": g.get("cleaned_text"), "profile_id": g["profile_id"],
                "engine": g.get("engine"), "language": g.get("language"), "speed": g.get("speed", 1.0),
                "params": g.get("params") or {}, "settings": g.get("settings") or {},
                "voice_settings": g.get("voice_settings") or {}, "parts": len(g["chunks"]),
            })
    vlist = []
    for v in voices:
        p = profiles[v]
        vlist.append({"id": v, "name": p.get("name"), "files": named_files(p), "has_preview": bool((p.get("preview") or {}).get("file"))})
    return {"rule": "selftest" if selftest else "standard", "scripts": scripts, "voices": vlist, "fills": fills}


def named_files(p: dict) -> list[str]:
    names = [v.get("file") for v in p.get("variants") or []]
    names += [(p.get("preview") or {}).get("file"), p.get("original_file")]
    return sorted({n for n in names if n})


def per_part(s: dict) -> int:
    st = s.get("settings") or {}
    return 2 * (int(st.get("max_attempts", DEFAULT_MAX_ATTEMPTS)) + max(1, int(st.get("compare_takes", DEFAULT_COMPARE_TAKES))) - 1)


def bound(plan: dict) -> tuple[int, int, int, int]:
    """(parts P, takes per engine E, files to copy, voices). N = 2 * E."""
    parts = sum(s["parts"] for s in plan["scripts"])
    e = sum(s["parts"] * per_part(s) for s in plan["scripts"])
    e += PREVIEW_TAKES * sum(1 for v in plan["voices"] if not v["has_preview"])
    files = sum(len(v["files"]) + len(FIXED_FILES) for v in plan["voices"])
    return parts, e, files, len(plan["voices"])


def load_plan(out: Path) -> dict:
    f = out / "scripts.json"
    if not f.exists():
        raise SystemExit("No plan yet: run  python3 rust/ab/ab.py plan  first.")
    return json.loads(f.read_text())


def cmd_plan(a) -> int:
    out = Path(a.out)
    f = out / "scripts.json"
    if a.refresh or not f.exists():
        plan = select(a.source, a.selftest_rule)
        if not a.selftest_rule and len(plan["scripts"]) != 10:
            raise SystemExit(f"Only {len(plan['scripts'])} scripts meet the rule; 10 are needed. Nothing saved.")
        out.mkdir(parents=True, exist_ok=True)
        f.write_text(json.dumps(plan, indent=1, sort_keys=True, ensure_ascii=False) + "\n")
    plan = load_plan(out)
    vnum = {v["id"]: i + 1 for i, v in enumerate(plan["voices"])}
    if plan["rule"] == "selftest":
        print("rule: selftest (outcome filter off, 1 part allowed, fewer than 10 allowed)")
    print(f"scripts: {len(plan['scripts'])}")
    for s in plan["scripts"]:
        chars = len(s.get("cleaned_text") or s.get("text") or "")
        print(f"script {s['ordinal']}: bucket {s['bucket']} parts, {s['parts']} parts, {chars} characters, voice {vnum[s['profile_id']]}")
    for fl in plan["fills"]:
        if fl.get("from"):
            print(f"bucket {fl['bucket']} parts was short: {fl['count']} filled from bucket {fl['from']} parts")
        else:
            print(f"bucket {fl['bucket']} parts is still short by {fl['short']}")
    p, e, files, nv = bound(plan)
    n = 2 * e
    print(f"voices: {nv}")
    print(f"reference clip files to copy: {files}")
    print(f"parts: {p}")
    print(f"takes, least: {2 * p}")
    print(f"takes, most: {n}")
    print("take bound: no unbounded term while the ending guard is off (render checks it); "
          "per part 2 x (max_attempts + compare_takes - 1), plus 3 per voice without a preview, times 2 engines")
    if n > 20:
        print(f"This is more than the 20-take limit; rendering needs Franz's OK for {n} takes.")
    return 0


# ------------------------------------------------------------------ the engines under test

def engine_call(base: str, method: str, path: str, body=None, timeout: float = 60) -> tuple[int, bytes]:
    """One client for both engines under test. Refuses anything but a dev port."""
    port = urllib.parse.urlsplit(base).port
    if port not in DEV_PORTS:
        raise SystemExit("Refusing an engine that is not on a dev port.")
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()
    except (urllib.error.URLError, OSError):
        return 0, b""


def port_free(port: int) -> bool:
    with socket.socket() as s:
        try:
            s.bind(("127.0.0.1", port))
            return True
        except OSError:
            return False


def listening(port: int) -> bool:
    """Something accepts connections there (a closed engine leaves TIME_WAIT, which blocks bind but not this)."""
    with socket.socket() as s:
        s.settimeout(1)
        return s.connect_ex(("127.0.0.1", port)) == 0


def free_port() -> int:
    for p in DEV_PORTS:
        if port_free(p):
            return p
    raise SystemExit("No free dev port (8790-8795, 8798, 8799).")


def child_env(lib: Path, max_takes: int | None) -> dict:
    env = {k: v for k, v in os.environ.items() if k not in ("TAKETWO_URL", "VOICEFORGE_URL", "TAKETWO_STUB_GATE")}
    env.update(TAKETWO_DATA=str(lib), VOICEFORGE_DATA=str(lib), PYTHONDONTWRITEBYTECODE="1",
               HF_HUB_OFFLINE="1")  # the local model cache is used; nothing may download
    if max_takes is not None:
        env["TAKETWO_MAX_TAKES"] = str(max_takes)
    return env


def start_engine(template: str, lib: Path, max_takes: int | None, log: Path) -> tuple[subprocess.Popen, str, int]:
    port = free_port()
    argv = [t.format(wt=WT, port=port) for t in shlex.split(template)]
    if "--live" in argv:
        raise SystemExit("--live is never used here.")
    proc = subprocess.Popen(argv, cwd=WT / "backend", env=child_env(lib, max_takes),
                            stdin=subprocess.DEVNULL, stdout=log.open("ab"), stderr=subprocess.STDOUT)
    base = f"http://127.0.0.1:{port}"
    end = time.time() + 120
    while time.time() < end:
        if proc.poll() is not None:
            raise SystemExit(f"An engine exited with code {proc.returncode} before it answered; its log is in {log.name}.")
        code, body = engine_call(base, "GET", "/api/health", timeout=2)
        if code == 200 and json.loads(body).get("ok"):
            return proc, base, port
        time.sleep(0.3)
    stop(proc)
    raise SystemExit("An engine did not answer /api/health in 120 s.")


def stop(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


SEED = r"""
import json, os, shutil, sys
from pathlib import Path
job = json.load(sys.stdin)
data = Path(os.environ["TAKETWO_DATA"])
sys.path.insert(0, job["backend"])
from app import config, db
assert config.DATA_DIR.resolve() == data.resolve(), config.DATA_DIR  # never the real library
for p in job["profiles"]:
    d = config.PROFILES_DIR / p["id"]
    d.mkdir(parents=True, exist_ok=True)
    for f in Path(job["staging"], p["id"]).iterdir():
        shutil.copy2(f, d / f.name)
    db.put_profile(p)
"""


def plain(name: str) -> bool:
    return bool(name) and "/" not in name and "\\" not in name and name not in (".", "..")


def build_library(source: str, plan: dict, tmp: Path) -> Path:
    """A temp library with only the planned voices, written by the Python app's own db module."""
    staging, lib = tmp / "staging", tmp / "library"
    lib.mkdir()
    docs = []
    for v in plan["voices"]:
        pid = urllib.parse.quote(v["id"], safe="")
        doc = source_json(source, f"/api/profiles/{pid}")
        doc.pop("job", None)
        (staging / v["id"]).mkdir(parents=True)
        for name in sorted(set(named_files(doc)) | set(FIXED_FILES)):
            if not plain(name):
                raise SystemExit("A voice names a file outside its folder; stopping.")
            code, body = source_get(source, f"/api/profiles/{pid}/audio/{urllib.parse.quote(name, safe='')}")
            if code == 200:
                (staging / v["id"] / name).write_bytes(body)
            elif not (code == 404 and name in FIXED_FILES):
                raise SystemExit(f"The source answered {code or 'nothing'} for a voice file; stopping.")
        docs.append(doc)
    py = WT / "backend" / ".venv" / "bin" / "python"
    job = json.dumps({"backend": str(WT / "backend"), "staging": str(staging), "profiles": docs})
    subprocess.run([str(py), "-B", "-c", SEED], input=job.encode(), env=child_env(lib, None), check=True, cwd=tmp)
    shutil.rmtree(staging)
    return lib


ACTIVE = ("queued", "running")


def render_one(base: str, s: dict, folder: Path) -> dict:
    stored = {k: s["settings"][k] for k in SETTING_KEYS if k in s["settings"]}
    # the bound's defaults, sent explicitly so a previous script's value never carries over
    stored.setdefault("max_attempts", DEFAULT_MAX_ATTEMPTS)
    stored.setdefault("compare_takes", DEFAULT_COMPARE_TAKES)
    if stored:
        code, body = engine_call(base, "PUT", "/api/settings", stored)
        if code != 200:
            raise SystemExit(f"Settings were refused ({code}): {body[:300].decode(errors='replace')}")
    code, body = engine_call(base, "GET", "/api/settings")
    if code != 200 or json.loads(body).get("experimental_qwen_ending_guard") is True:
        raise SystemExit("The ending guard is on (or settings unreadable): the take bound does not hold. Stopping.")
    req = {"profile_id": s["profile_id"], "text": s["text"], "engine": s["engine"], "language": s["language"],
           "params": s["params"], "speed": s["speed"], "voice_settings": s["voice_settings"]}
    t0 = time.time()
    code, body = engine_call(base, "POST", "/api/generations", req)
    if code == 501:
        raise NotReady(body[:300].decode(errors="replace"))
    if code != 200:
        raise SystemExit(f"Creating script {s['ordinal']} answered {code}: {body[:300].decode(errors='replace')}")
    g = json.loads(body)
    gid = urllib.parse.quote(g["id"], safe="")
    parts = len(g.get("chunks") or [])
    if parts > s["parts"]:  # fewer parts stay inside the bound; more would not (the Python engine has no take cap)
        engine_call(base, "POST", f"/api/generations/{gid}/cancel", {})
        raise SystemExit(f"Script {s['ordinal']} was split into {parts} parts, more than the planned {s['parts']}: "
                         "the take bound no longer holds. Cancelled; stopping.")
    while g.get("status") in ACTIVE:
        time.sleep(0.5)
        code, body = engine_call(base, "GET", f"/api/generations/{gid}")
        if code != 200:
            raise SystemExit(f"Reading script {s['ordinal']} answered {code}.")
        g = json.loads(body)
    wall = round(time.time() - t0, 2)
    code, body = engine_call(base, "GET", "/api/generations")
    row = next((r for r in json.loads(body) if r["id"] == g["id"]), {}) if code == 200 else {}
    rec = {"ordinal": s["ordinal"], "status": g.get("status"), "wall_seconds": wall, "parts": parts,
           # every seed a part ever used (render, retakes and the final check add to it); 0 when the engine keeps none
           "takes_used": sum(len(c.get("used_seeds") or []) for c in g.get("chunks") or []),
           "outcome": row.get("outcome"), "review_count": row.get("review_count")}
    for path, name in ((f"/api/generations/{gid}/audio", f"{s['ordinal']:02d}.wav"),
                       (f"/api/generations/{gid}/subtitles?fmt=srt", f"{s['ordinal']:02d}.srt")):
        code, body = engine_call(base, "GET", path, timeout=300)
        if code == 200:
            (folder / name).write_bytes(body)
        rec[name.split(".")[1]] = code == 200
    return rec


class NotReady(Exception):
    pass


def cmd_render(a) -> int:
    out = Path(a.out)
    plan = load_plan(out)
    p, e, files, nv = bound(plan)
    n = 2 * e
    if a.franz_ok != n:
        print(f"Rendering waits for Franz's OK ({n} takes at most, {files} reference clips copied from {nv} voices).")
        return 3
    tmp = Path(tempfile.mkdtemp(prefix="taketwo-ab-"))
    results = {"engines": {"a": a.engine_a_cmd, "b": a.engine_b_cmd}, "max_takes_per_engine": e, "pids": [], "ports": [], "runs": {}}
    proc = None
    try:
        base_lib = build_library(a.source, plan, tmp)
        for label, template in (("a", a.engine_a_cmd), ("b", a.engine_b_cmd)):
            lib = tmp / f"library-{label}"
            shutil.copytree(base_lib, lib)
            folder = out / "render" / label
            if folder.exists():
                shutil.rmtree(folder)
            folder.mkdir(parents=True)
            wait_source_idle(a.source)
            proc, base, port = start_engine(template, lib, e, out / "render" / f"{label}.log")
            results["pids"].append(proc.pid)
            results["ports"].append(port)
            runs = results["runs"][label] = []
            try:
                for s in plan["scripts"]:
                    runs.append(render_one(base, s, folder))
                    print(f"engine {label}: script {s['ordinal']} {runs[-1]['status']} in {runs[-1]['wall_seconds']} s", flush=True)
            except NotReady as exc:
                print(f"Engine {label} answered 501 to creating a narration: {exc}. It cannot render yet; stopping.")
                return 4
            finally:
                stop(proc)
    finally:
        if proc is not None:
            stop(proc)
        shutil.rmtree(tmp, ignore_errors=True)
        out.mkdir(parents=True, exist_ok=True)
        (out / "render.json").write_text(json.dumps(results, indent=1) + "\n")
    print(f"render done: {len(plan['scripts'])} scripts on each engine; results in render.json")
    return 0


# ------------------------------------------------------------------ page

PAGE = """<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Blind listening test</title><style>:root{--bg:#fbf9f6;--card:#fff;--fg:#1b1815;--fg2:#5f5851;--line:#e7e1d9;--ink:#b9471c}
@media(prefers-color-scheme:dark){:root{--bg:#0b0a09;--card:#151311;--fg:#f5f0ea;--fg2:#b4aba1;--line:#2a2622;--ink:#ff9a5c}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 -apple-system,system-ui,sans-serif;padding:24px 16px 60px}
main{max-width:760px;margin:0 auto}h1{font:600 26px/1.2 Georgia,serif;margin:0 0 6px}p{color:var(--fg2);margin:0 0 14px}
.card{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:16px 18px;margin:14px 0}
.q{font-weight:600;margin-bottom:10px}.row{display:grid;grid-template-columns:28px 1fr;gap:8px;align-items:center;margin:6px 0}
audio{width:100%}.pick{display:flex;flex-wrap:wrap;gap:8px;margin-top:10px}
button{font:inherit;border:1px solid var(--line);background:var(--bg);color:var(--fg);border-radius:9px;padding:7px 12px;cursor:pointer;min-height:36px}
button[aria-pressed=true]{border-color:var(--ink);color:var(--ink);font-weight:600}
textarea{width:100%;min-height:60px;font:inherit;border:1px solid var(--line);border-radius:9px;padding:8px;background:var(--bg);color:var(--fg)}
details{color:var(--fg2);margin-bottom:8px}</style></head><body><main><h1>Blind listening test</h1>
<p>Each script was rendered twice, by two different builds, with the same voice and settings. They are labelled A and B in a random order that changes from script to script. Pick the one you prefer, or no difference, and note anything you hear.</p>
CARDS
<div class="card"><div class="pick"><button id="copy">Copy my answers</button></div><pre id="out" style="white-space:pre-wrap"></pre></div></main>
<script>
const N=COUNT, picks={};
document.querySelectorAll('.pick[data-s] button').forEach(b=>b.onclick=()=>{const s=b.parentNode.dataset.s;picks[s]=b.dataset.v;
b.parentNode.querySelectorAll('button').forEach(x=>x.setAttribute('aria-pressed',x===b))});
document.getElementById('copy').onclick=()=>{let t='';for(let s=1;s<=N;s++){t+='Script '+s+': '+(picks[s]||'no answer')+'\\n';
const n=document.getElementById('n'+s).value.trim();if(n)t+='  notes: '+n.replace(/\\n/g,' ')+'\\n'}
document.getElementById('out').textContent=t;if(navigator.clipboard)navigator.clipboard.writeText(t).catch(()=>{})};
</script></body></html>
"""


def cmd_page(a) -> int:
    out = Path(a.out)
    plan = load_plan(out)
    rnd = random.SystemRandom()
    key, cards = {}, []
    for s in plan["scripts"]:
        files = {lab: out / "render" / lab / f"{s['ordinal']:02d}.wav" for lab in ("a", "b")}
        if not all(f.exists() for f in files.values()):
            raise SystemExit(f"Script {s['ordinal']} has no audio from both engines; run render first.")
        order = ["a", "b"]
        rnd.shuffle(order)
        key[str(s["ordinal"])] = {"A": order[0], "B": order[1]}
        # shortcut: WAV is embedded as is (about 2.9 MB a minute); convert to AAC first if the page gets too heavy
        rows = "".join(f'<div class="row"><b>{x}</b><audio controls preload="none" src="data:audio/wav;base64,'
                       f'{base64.b64encode(files[lab].read_bytes()).decode()}"></audio></div>' for x, lab in zip("AB", order))
        o = s["ordinal"]
        cards.append(f'<div class="card"><div class="q">Script {o}: {html.escape(s.get("title") or "")}</div>'
                     f'<details><summary>Script text</summary>{html.escape(s.get("cleaned_text") or s.get("text") or "")}</details>{rows}'
                     f'<div class="pick" data-s="{o}"><button data-v="A">A</button><button data-v="B">B</button>'
                     f'<button data-v="no difference">No difference</button></div>'
                     f'<textarea id="n{o}" aria-label="Notes for script {o}" placeholder="Notes"></textarea></div>')
    page = PAGE.replace("CARDS", "\n".join(cards)).replace("COUNT", str(len(plan["scripts"])))
    (out / "page.html").write_text(page)
    (out / "key.json").write_text(json.dumps({"engines": {"a": "engine a (--engine-a-cmd, Python app by default)",
                                                          "b": "engine b (--engine-b-cmd, Rust taketwo-engine by default)"},
                                              "scripts": key}, indent=1) + "\n")
    print(f"page: {len(cards)} scripts, 2 players each; key kept apart in key.json")
    return 0


# ------------------------------------------------------------------ selftest

def snapshot(d: Path) -> dict:
    return {str(f.relative_to(d)): (f.stat().st_size, f.stat().st_mtime_ns) for f in sorted(d.rglob("*")) if f.is_file()}


def cmd_selftest(a) -> int:
    exe = WT / "rust" / "target" / "debug" / "taketwo-engine"
    if not exe.exists():
        raise SystemExit("Build it first: cd rust && source scripts/env.sh && cargo build -p taketwo-engine")
    tmp = Path(tempfile.mkdtemp(prefix="taketwo-ab-selftest-"))
    src_lib, out = tmp / "source", tmp / "out"
    src_lib.mkdir()
    me = [sys.executable, "-B", str(Path(__file__).resolve())]
    proc = None
    try:
        subprocess.run([str(exe), "seed", "--fixture", str(WT / "contract" / "fixture.json")],
                       env=child_env(src_lib, None), check=True, stdout=subprocess.DEVNULL)
        proc, source, src_port = start_engine(STUB_ENGINE, src_lib, None, tmp / "source.log")
        before = snapshot(src_lib)
        ab = ["--source", source, "--out", str(out)]
        r = subprocess.run(me + ["plan", "--selftest-rule"] + ab, capture_output=True, text=True)
        print(r.stdout, end="")
        assert r.returncode == 0, r.stderr
        n = int(re.search(r"^takes, most: (\d+)$", r.stdout, re.M).group(1))
        assert subprocess.run(me + ["render"] + ab, capture_output=True).returncode == 3
        r = subprocess.run(me + ["render", "--franz-ok", str(n), "--engine-a-cmd", STUB_ENGINE, "--engine-b-cmd", STUB_ENGINE] + ab,
                           capture_output=True, text=True)
        print(r.stdout, end="")
        assert r.returncode == 0, (r.returncode, r.stdout[-2000:], r.stderr[-2000:])
        r = subprocess.run(me + ["page", "--out", str(out)], capture_output=True, text=True)
        print(r.stdout, end="")
        assert r.returncode == 0, r.stderr
        after = snapshot(src_lib)
        assert before == after, f"source library changed: {sorted(set(before.items()) ^ set(after.items()))}"
        plan = json.loads((out / "scripts.json").read_text())
        scripts = plan["scripts"]
        assert scripts, "the plan is empty"
        for s in scripts:
            for lab in ("a", "b"):
                assert (out / "render" / lab / f"{s['ordinal']:02d}.wav").stat().st_size > 44, (s["ordinal"], lab)
        page = (out / "page.html").read_text()
        assert page.count("<audio ") == 2 * len(scripts), page.count("<audio ")
        assert (out / "key.json").exists()
        low = page.lower()
        assert not [w for w in ENGINE_WORDS if w in low], [w for w in ENGINE_WORDS if w in low]
        res = json.loads((out / "render.json").read_text())
        assert all(r["status"] == "done" for runs in res["runs"].values() for r in runs), res["runs"]
        assert res["max_takes_per_engine"] * 2 == n
        stop(proc)
        for pid in res["pids"] + [proc.pid]:
            try:
                os.kill(pid, 0)
                raise AssertionError(f"process {pid} is still running")
            except ProcessLookupError:
                pass
        for port in res["ports"] + [src_port]:
            assert not listening(port), f"port {port} is still taken"
    finally:
        if proc is not None:
            stop(proc)
        shutil.rmtree(tmp, ignore_errors=True)
    print("AB SELFTEST PASS")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("plan", "render", "page", "selftest"):
        sp = sub.add_parser(name)
        sp.add_argument("--out", default=str(PRIVATE))
        if name in ("plan", "render"):
            sp.add_argument("--source", default=DEFAULT_SOURCE)
        if name == "plan":
            sp.add_argument("--refresh", action="store_true")
            sp.add_argument("--selftest-rule", action="store_true")
        if name == "render":
            sp.add_argument("--franz-ok", type=int)
            sp.add_argument("--engine-a-cmd", default=ENGINE_A)
            sp.add_argument("--engine-b-cmd", default=ENGINE_B)
    a = ap.parse_args()
    return {"plan": cmd_plan, "render": cmd_render, "page": cmd_page, "selftest": cmd_selftest}[a.cmd](a)


if __name__ == "__main__":
    raise SystemExit(main())
