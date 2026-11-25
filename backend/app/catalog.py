"""Keeping up with new free models: discover MLX TTS repos on Hugging Face and
upgrade the mlx-audio engine library (which is where new architectures land)."""
from __future__ import annotations

import json
import re
import subprocess
import sys
import urllib.request
from importlib import metadata
from pathlib import Path

# The version this release was tested and listened to with. Widen only after testing a newer one by ear.
MLX_AUDIO_RANGE, _TOO_NEW = "mlx-audio>=0.5.6,<0.6", (0, 6)
HF_API = "https://huggingface.co/api/models"


def _get(url: str):
    req = urllib.request.Request(url, headers={"User-Agent": "voiceforge"})
    with urllib.request.urlopen(req, timeout=15) as r:
        return json.loads(r.read())


def supported_architectures() -> list[str]:
    import mlx_audio.tts.models as m

    return sorted(p.name for p in Path(m.__file__).parent.iterdir() if p.is_dir() and not p.name.startswith("_"))


def library_status() -> dict:
    installed = metadata.version("mlx-audio")
    latest = None
    try:
        latest = _get("https://pypi.org/pypi/mlx-audio/json")["info"]["version"]
    except Exception:
        pass
    tested = bool(latest) and tuple(int(x) for x in re.findall(r"\d+", latest)[:2]) < _TOO_NEW
    return {"installed": installed, "latest": latest, "update_available": bool(latest and latest != installed and tested)}


def upgrade_library() -> str:
    r = subprocess.run(
        [sys.executable, "-m", "pip", "install", "-U", MLX_AUDIO_RANGE], capture_output=True, text=True, timeout=900
    )
    if r.returncode != 0:
        raise RuntimeError(r.stderr[-800:])
    return metadata.version("mlx-audio")


CLONE_HINTS = ("tts", "voice", "speech", "chatterbox", "voxcpm", "fish", "higgs", "csm", "tada", "echo", "index", "spark", "zonos", "dia", "outetts", "vibevoice", "moss")


def discover(known_repos: set[str], sort: str = "lastModified", limit: int = 80) -> list[dict]:
    items = _get(f"{HF_API}?author=mlx-community&pipeline_tag=text-to-speech&sort={sort}&direction=-1&limit={limit}")
    archs = supported_architectures()
    out = []
    for m in items:
        rid = m["id"]
        low = rid.lower()
        if not any(h in low for h in CLONE_HINTS) or "stt" in low or "asr" in low:
            continue
        norm = low.split("/")[-1].replace("-", "_").replace(".", "_")
        arch = next((a for a in archs if a.replace("_", "") in norm.replace("_", "")), None)
        out.append(
            {
                "repo": rid, "downloads": m.get("downloads", 0), "likes": m.get("likes", 0),
                "updated": m.get("lastModified") or m.get("createdAt"), "architecture": arch,
                "supported": arch is not None, "added": rid in known_repos,
            }
        )
    return out
