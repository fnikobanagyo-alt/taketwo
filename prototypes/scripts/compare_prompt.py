"""PROTOTYPE. Compare what Rust extracts from a reference clip with what MLX extracts.
1) qwen-tts ... --save-prompt rust_prompt.json voice-clone -a ref.wav -t "<transcript>"
2) backend/.venv/bin/python compare_prompt.py rust_prompt.json ref.wav   (run from voiceforge/backend)"""
import base64, json, struct, sys
import numpy as np
sys.path.insert(0, ".")
from app import engines
from mlx_audio.utils import load_audio
blob = base64.b64decode(json.load(open(sys.argv[1]))["prompt_data"])
n = struct.unpack("<Q", blob[:8])[0]; hdr = json.loads(blob[8:8 + n]); body = blob[8 + n:]
def t(name, dt):
    s, e = hdr[name]["data_offsets"]; return np.frombuffer(body[s:e], dtype=dt).reshape(hdr[name]["shape"])
rc, rx = t("ref_code", np.int64), t("ref_spk_embedding", np.float32).astype(np.float64)
m = engines.load("qwen3-tts-1.7b"); audio = load_audio(sys.argv[2], sample_rate=24000)
x = np.array(m.extract_speaker_embedding(audio)).ravel().astype(np.float64)
codes = np.array(m.speech_tokenizer.encode(audio[None, None, :]))[0].T
k = min(len(rc), len(codes))
print(f"fingerprint cosine {rx @ x / np.linalg.norm(rx) / np.linalg.norm(x):.4f}; codes rust {rc.shape} mlx {codes.shape}, agree {np.mean(rc[:k] == codes[:k]):.3f}")
