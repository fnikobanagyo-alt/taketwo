"""Paths and global defaults."""
import os
from pathlib import Path

# The xet transfer backend stalls on flaky connections (0 bytes for minutes); plain HTTP resumes.
os.environ.setdefault("HF_HUB_DISABLE_XET", "1")
# Fail fast on stalled Hugging Face connections instead of hanging forever.
os.environ.setdefault("HF_HUB_DOWNLOAD_TIMEOUT", "60")
os.environ.setdefault("HF_HUB_ETAG_TIMEOUT", "15")
os.environ.setdefault("HF_HUB_DISABLE_TELEMETRY", "1")

ROOT = Path(__file__).resolve().parents[2]
APP_VERSION = "2.0.0"  # bump with each GitHub Release tag (v2.0.0); the update check compares against it
def _env(name: str, default: str | Path):  # TAKETWO_* wins; VOICEFORGE_* still works (former name)
    return os.environ.get(f"TAKETWO_{name}") or os.environ.get(f"VOICEFORGE_{name}") or default


DATA_DIR = Path(_env("DATA", ROOT / "data"))
PROFILES_DIR = DATA_DIR / "profiles"
GENERATIONS_DIR = DATA_DIR / "generations"
DESIGNS_DIR = DATA_DIR / "designs"
TRANSCRIPTS_DIR = DATA_DIR / "transcripts"
DB_PATH = DATA_DIR / "voiceforge.db"
SETTINGS_PATH = DATA_DIR / "settings.json"
FRONTEND_DIST = ROOT / "frontend" / "dist"

for d in (DATA_DIR, PROFILES_DIR, GENERATIONS_DIR, DESIGNS_DIR, TRANSCRIPTS_DIR):
    d.mkdir(parents=True, exist_ok=True)

# Bump when voice analysis improves, so older voices can be offered an upgrade.
PIPELINE_VERSION = 4  # 4: reference clips cut inside real pauses, light repetition penalty

# Reference uploads
MAX_REFERENCE_SECONDS = 300.0  # 5 minutes
MIN_REFERENCE_SECONDS = 3.0

# Internal working sample rate for analysis / QA
WORK_SR = 24000

STT_REPO = "mlx-community/whisper-large-v3-turbo"
STT_PROCESSOR_REPO = "openai/whisper-large-v3-turbo"
VOICE_ENCODER_REPO = "ResembleAI/chatterbox"  # only ve.safetensors (5.7 MB) is fetched

# Release the voice and speech-recognition models after this many idle seconds (0 = keep them loaded).
IDLE_UNLOAD_SECONDS = float(_env("IDLE_UNLOAD_S", "300"))

DEFAULT_SETTINGS = {
    "default_engine": "qwen3-tts-1.7b",
    "max_chunk_chars": 220,
    "max_attempts": 4,
    "compare_takes": 1,  # 2 = record two clean takes per part and keep the closer voice match
    "experimental_qwen_ending_guard": False,  # spoken continuation failed the user's listening comparison
    "max_wer": 0.12,
    "min_speaker_similarity": 0.85,  # take-vs-fingerprint, relative to the sample's own consistency
    "max_internal_silence": 0.9,
    "max_pitch_offset_st": 2.0,  # a take this far from the voice's (or a neighbour part's) median pitch is retaken
    "sentence_pause_ms": 300,
    "inner_sentence_pause_ms": 0,  # 0 = off. Widens rushed full stops inside a part; on once a length is chosen by ear
    "paragraph_pause_ms": 700,
    "target_lufs": -16.0,
    "seed": 1234,
    "pronunciations": [],  # [{"from": "Fimbulwinter", "to": "Fimbul-winter"}]
    "strip_citations": True,
    "custom_engines": [],  # user-added HF repos
    "check_updates": True,  # ask GitHub once at launch whether a newer release exists
    "update_dismissed": "",  # version whose notice the user hid
}
