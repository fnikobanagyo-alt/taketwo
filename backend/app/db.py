"""Tiny SQLite store: profiles and generations as JSON documents."""
from __future__ import annotations

import json
import math
import re
import sqlite3
import threading
import time

from .config import DB_PATH, DEFAULT_SETTINGS, SETTINGS_PATH

_lock = threading.Lock()


def _conn():
    c = sqlite3.connect(DB_PATH, check_same_thread=False)
    c.row_factory = sqlite3.Row
    return c


_db = _conn()
_db.executescript(
    """
    CREATE TABLE IF NOT EXISTS profiles (id TEXT PRIMARY KEY, created REAL, doc TEXT);
    CREATE TABLE IF NOT EXISTS generations (id TEXT PRIMARY KEY, created REAL, profile_id TEXT, doc TEXT);
    """
)


def _put(table: str, doc: dict, **cols) -> dict:
    doc.setdefault("created", time.time())
    doc["updated"] = time.time()
    names = ["id", "created", *cols.keys(), "doc"]
    vals = [doc["id"], doc["created"], *cols.values(), json.dumps(doc)]
    with _lock:
        _db.execute(
            f"INSERT OR REPLACE INTO {table} ({','.join(names)}) VALUES ({','.join('?' * len(names))})", vals
        )
        _db.commit()
    return doc


def _get(table: str, id_: str) -> dict | None:
    with _lock:
        r = _db.execute(f"SELECT doc FROM {table} WHERE id=?", (id_,)).fetchone()
    return json.loads(r["doc"]) if r else None


def _list(table: str, where: str = "", args: tuple = ()) -> list[dict]:
    with _lock:
        rows = _db.execute(f"SELECT doc FROM {table} {where} ORDER BY created DESC", args).fetchall()
    return [json.loads(r["doc"]) for r in rows]


def _delete(table: str, id_: str) -> None:
    with _lock:
        _db.execute(f"DELETE FROM {table} WHERE id=?", (id_,))
        _db.commit()


put_profile = lambda d: _put("profiles", d)
get_profile = lambda i: _get("profiles", i)
list_profiles = lambda: _list("profiles")
delete_profile = lambda i: _delete("profiles", i)

put_generation = lambda d: _put("generations", d, profile_id=d.get("profile_id"))
get_generation = lambda i: _get("generations", i)
list_generations = lambda: _list("generations")
delete_generation = lambda i: _delete("generations", i)

# Fields the user edits from the UI. A running narration must never write these back
# with the values it loaded when it started (that would undo an edit made while it renders).
GEN_META_KEYS = ("title", "favorite")


def put_generation_live(d: dict) -> bool:
    """Worker save: read-modify-write in one locked step.

    Returns False, writing nothing, when the narration was deleted meanwhile, so a stale
    worker can never recreate a deleted row. Title and favourite edits made while it
    rendered are kept."""
    with _lock:
        r = _db.execute("SELECT doc FROM generations WHERE id=?", (d["id"],)).fetchone()
        if r is None:
            return False
        cur = json.loads(r["doc"])
        for k in GEN_META_KEYS:
            if k in cur:
                d[k] = cur[k]
        d["updated"] = time.time()
        _db.execute("UPDATE generations SET doc=? WHERE id=?", (json.dumps(d), d["id"]))
        _db.commit()
    return True


def patch_generation_meta(gid: str, fields: dict) -> dict | None:
    """Apply UI metadata edits (title, favourite) to the stored narration, atomically."""
    with _lock:
        r = _db.execute("SELECT doc FROM generations WHERE id=?", (gid,)).fetchone()
        if r is None:
            return None
        doc = json.loads(r["doc"])
        for k in GEN_META_KEYS:
            if k in fields:
                doc[k] = fields[k]
        doc["updated"] = time.time()
        _db.execute("UPDATE generations SET doc=? WHERE id=?", (json.dumps(doc), gid))
        _db.commit()
    return doc


def _custom_engines_ok(v) -> bool:
    return isinstance(v, list) and all(isinstance(c, dict) and all(isinstance(c.get(k), str) for k in ("id", "repo")) for c in v)


def get_settings() -> dict:
    """Defaults plus saved settings. A damaged file must never stop the app from starting: it is kept
    aside as settings.json.broken and the defaults are used."""
    s = dict(DEFAULT_SETTINGS)
    if not SETTINGS_PATH.exists():
        return s
    try:
        saved = json.loads(SETTINGS_PATH.read_text())
        if not isinstance(saved, dict):
            raise ValueError("settings must be an object")
    except ValueError:
        SETTINGS_PATH.replace(SETTINGS_PATH.with_name("settings.json.broken"))
        return s
    if not _custom_engines_ok(saved.get("custom_engines", [])):
        saved.pop("custom_engines")
    s.update(saved)
    return s


# Ranges for numeric settings. Out-of-range values made the pipeline index missing
# results (zero attempts) or cut pauses to nothing, so they are rejected at the API.
SETTING_RULES = {
    "max_chunk_chars": (80, 600, int),
    "max_attempts": (1, 8, int),
    "compare_takes": (1, 3, int),
    "max_wer": (0.01, 0.5, float),
    "min_speaker_similarity": (0.5, 0.99, float),
    "max_internal_silence": (0.2, 3.0, float),
    "max_pitch_offset_st": (0.5, 12.0, float),
    "sentence_pause_ms": (0, 2000, int),
    "inner_sentence_pause_ms": (0, 2000, int),
    "paragraph_pause_ms": (0, 4000, int),
    "target_lufs": (-30.0, -10.0, float),
    "seed": (0, 2**31 - 1, int),
}


def _checked(patch: dict) -> dict:
    out = {}
    for k, v in patch.items():
        if k == "check_updates" and not isinstance(v, bool):
            raise ValueError("check_updates must be true or false")
        if k == "update_dismissed" and not (v == "" or (isinstance(v, str) and re.fullmatch(r"\d{1,4}\.\d{1,4}\.\d{1,4}", v))):
            raise ValueError("update_dismissed must be a version like 2.1.0")
        if k == "custom_engines" and not _custom_engines_ok(v):
            raise ValueError("custom_engines must be a list of models")
        if k in SETTING_RULES:
            lo, hi, typ = SETTING_RULES[k]
            if isinstance(v, bool):
                raise ValueError(f"{k} must be a number")
            try:
                num = typ(v)
            except (TypeError, ValueError):
                raise ValueError(f"{k} must be a number") from None
            if typ is float and not math.isfinite(num):
                raise ValueError(f"{k} must be a number")
            if not lo <= num <= hi:
                raise ValueError(f"{k} must be between {lo} and {hi}")
            v = num
        out[k] = v
    return out


def save_settings(patch: dict) -> dict:
    s = get_settings()
    s.update({k: v for k, v in _checked(patch).items() if k in DEFAULT_SETTINGS})
    tmp = SETTINGS_PATH.with_name("settings.json.tmp")
    tmp.write_text(json.dumps(s, indent=1))
    tmp.replace(SETTINGS_PATH)  # never leave a half-written file
    return s
