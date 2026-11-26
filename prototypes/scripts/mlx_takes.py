"""Today's engine, raw: VoiceForge's own synthesize() call (no retakes, checks or mastering), one take per line."""
import json, os, resource, sys, tempfile, time
from pathlib import Path
os.environ["TAKETWO_DATA"] = tempfile.mkdtemp(prefix="taketwo-proto-")  # never the real library
P = Path(sys.argv[1]); sys.path.insert(0, sys.argv[2])
import soundfile as sf
from app import engines
ref = json.load(open(P / "ref.json")); lines = json.load(open(P / "lines.json"))
out = []
for i, text in enumerate(lines):
    t0 = time.time()
    a, sr = engines.synthesize("qwen3-tts-1.7b", text, P / "ref.wav", ref["text"], "en", {}, 1234, len(text.split()) / 2.6)
    secs = time.time() - t0
    sf.write(P / f"mlx_{i}.wav", a, sr)
    out.append({"line": i, "engine": "mlx", "seconds": round(secs, 2), "audio_s": round(len(a) / sr, 2), "sr": sr})
    print(out[-1], flush=True)
peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1e9  # bytes on macOS
json.dump({"takes": out, "peak_gb": round(peak, 2)}, open(P / "mlx_report.json", "w"), indent=1)
print("peak memory GB", round(peak, 2))
