"""PROTOTYPE. Make the official-layout Qwen3-TTS 1.7B Base weights from the MLX copy VoiceForge already has.
Only the 38 speaker-encoder conv1d weights differ: MLX stores [out, kernel, in], PyTorch/Candle [out, in, kernel].
Usage: backend/.venv/bin/python convert_mlx_weights.py <mlx snapshot dir> <out dir> <official model.safetensors header json>"""
import json, sys, mlx.core as mx
src, out, hdr = sys.argv[1:]
official = json.load(open(hdr)); official.pop("__metadata__", None)
w = mx.load(src.rstrip("/") + "/model.safetensors")
for k, a in list(w.items()):
    if list(a.shape) != official[k]["shape"]:
        w[k] = mx.swapaxes(a, 1, 2)
        assert list(w[k].shape) == official[k]["shape"], k
mx.save_safetensors(out + "/model.safetensors", w, metadata={"format": "pt"})
