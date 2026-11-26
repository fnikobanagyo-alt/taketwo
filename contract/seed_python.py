"""Run the contract suite against the Python engine. Engine-specific: a Rust engine brings its own seeder.

    backend/.venv/bin/python -B contract/seed_python.py

1. Writes contract/fixture.json into a fresh temporary library (TAKETWO_DATA), with synthetic tones as audio.
2. Starts the Python engine on a free port 8790-8799 with that library, offline (no model can load or download).
3. Runs the black-box suite with TAKETWO_URL pointing at it, then stops the engine and deletes the library.
"""
from __future__ import annotations

import json
import math
import os
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request
import wave
from pathlib import Path

HERE = Path(__file__).resolve().parent
BACKEND = HERE.parent / "backend"
PORTS = range(8790, 8800)  # never the desktop app's 8766-8785


def tone(path: Path, seconds: float, hz: float = 220.0, sr: int = 24000) -> None:
    n = max(1, int(seconds * sr))
    frames = b"".join(struct.pack("<h", int(8000 * math.sin(2 * math.pi * hz * i / sr))) for i in range(n))
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(frames)


def seed(data: Path) -> None:
    os.environ["TAKETWO_DATA"] = os.environ["VOICEFORGE_DATA"] = str(data)
    sys.path.insert(0, str(BACKEND))
    from app import config, db

    assert config.DATA_DIR.resolve() == data.resolve(), config.DATA_DIR  # never the real library
    fixture = json.loads((HERE / "fixture.json").read_text())
    for p in fixture["profiles"]:
        (config.PROFILES_DIR / p["id"]).mkdir(parents=True, exist_ok=True)
        tone(config.PROFILES_DIR / p["id"] / "preview.wav", 1.0)  # without it, startup would render a preview
        db.put_profile(dict(p))
    for g in fixture["generations"]:
        gdir = config.GENERATIONS_DIR / g["id"]
        gdir.mkdir(parents=True, exist_ok=True)
        for c in g["chunks"]:
            if c.get("file"):
                tone(gdir / c["file"], 0.5, 200 + 20 * c["i"])
        if g.get("file"):
            tone(gdir / "final.wav", g["duration"])
        db.put_generation(dict(g))


def free_port() -> int:
    for port in PORTS:
        with socket.socket() as s:
            try:
                s.bind(("127.0.0.1", port))
                return port
            except OSError:
                continue
    raise SystemExit("No free port in 8790-8799.")


def wait_ready(url: str, proc: subprocess.Popen, timeout: float = 90) -> None:
    end = time.time() + timeout
    while time.time() < end:
        if proc.poll() is not None:
            raise SystemExit(f"The engine exited with code {proc.returncode} before it was ready.")
        try:
            with urllib.request.urlopen(url + "/api/health", timeout=1) as r:
                if json.loads(r.read()).get("ok"):
                    return
        except OSError:
            time.sleep(0.3)
    raise SystemExit("The engine did not answer /api/health in time.")


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="taketwo-contract-") as tmp:
        data = Path(tmp) / "library"
        data.mkdir()
        seed(data)
        port = free_port()
        url = f"http://127.0.0.1:{port}"
        env = {**os.environ, "TAKETWO_DATA": str(data), "VOICEFORGE_DATA": str(data),
               "HF_HUB_OFFLINE": "1", "HF_HOME": str(Path(tmp) / "hf")}  # belt and braces: nothing can render
        log = open(Path(tmp) / "engine.log", "w+b")
        proc = subprocess.Popen([sys.executable, "-B", "-m", "uvicorn", "app.main:app", "--host", "127.0.0.1", "--port", str(port)],
                                cwd=BACKEND, env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            wait_ready(url, proc)
            print(f"Python engine ready at {url} (library {data})", flush=True)
            return subprocess.run([sys.executable, "-B", "-m", "unittest", "discover", "-s", str(HERE), "-t", str(HERE)],
                                  env={**env, "TAKETWO_URL": url}).returncode
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
            if proc.returncode not in (0, -15):
                log.seek(0)
                print(log.read().decode(errors="replace")[-3000:], file=sys.stderr)
            log.close()


if __name__ == "__main__":
    raise SystemExit(main())
