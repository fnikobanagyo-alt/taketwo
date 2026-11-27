"""Convert the local MLX Whisper large-v3-turbo weights to a whisper.cpp ggml file (step S13).

    HF_HUB_OFFLINE=1 backend/.venv/bin/python -B prototypes/scripts/convert_whisper_ggml.py

Reads the local Hugging Face snapshots mlx-community/whisper-large-v3-turbo (config.json,
weights.safetensors) and openai/whisper-large-v3-turbo (vocab.json, preprocessor_config.json).
Writes only rust/.models/ggml-large-v3-turbo.bin. No network.

Follows rust/third_party/whisper.cpp/models/convert-pt-to-ggml.py for the header, the vocabulary,
the tensor names, which tensors stay f32, and the f16 rule. Differences from that script, all forced
by the MLX layout: names mlp1/mlp2 -> mlp.0/mlp.2; conv weights are channels-last (out, k, in) in
MLX and go back to PyTorch's (out, in, k) (axes 1 and 2 swapped); encoder.positional_embedding is
not stored by MLX (mlx-audio computes it) and is computed here with the same sinusoids formula in
float32; the alignment_heads tensor is not a weight (mlx-audio ignores it) and is skipped; the local
vocab.json is sorted by key, so tokens are written in id order (whisper.cpp assigns ids by position).
Mel filters: the filterbank mlx-audio really uses (mlx_audio.utils.mel_filters, slaney), compared
with transformers' WhisperFeatureExtractor built from the local preprocessor_config.json.
"""
from __future__ import annotations

import json
import os
import shutil
import struct
import sys
from pathlib import Path

import numpy as np

assert os.environ.get("HF_HUB_OFFLINE") == "1", "run with HF_HUB_OFFLINE=1"

WT = Path(__file__).resolve().parents[2]
OUT = WT / "rust/.models/ggml-large-v3-turbo.bin"
REF = WT / "rust/third_party/whisper.cpp/models/convert-pt-to-ggml.py"


def snapshot(repo: str) -> Path:
    from huggingface_hub import snapshot_download

    return Path(snapshot_download(repo, local_files_only=True))


def bytes_to_unicode():  # copied from convert-pt-to-ggml.py
    bs = list(range(ord("!"), ord("~") + 1)) + list(range(ord("¡"), ord("¬") + 1)) + list(range(ord("®"), ord("ÿ") + 1))
    cs = bs[:]
    n = 0
    for b in range(2**8):
        if b not in bs:
            bs.append(b)
            cs.append(2**8 + n)
            n += 1
    cs = [chr(n) for n in cs]
    return dict(zip(bs, cs))


def vocab_tokens(vocab_json: Path) -> dict[bytes, int]:
    """As convert-pt-to-ggml.py's hf_transformers path; whisper.cpp assigns ids by file position."""
    byte_decoder = {v: k for k, v in bytes_to_unicode().items()}
    raw = json.loads(vocab_json.read_text(encoding="utf8"))
    if "<|endoftext|>" in raw:
        del raw["<|endoftext|>"]
    tokens = {bytes([byte_decoder[c] for c in token]): int(idx) for token, idx in raw.items()}
    assert len(tokens) == len(raw), "two vocab entries decode to the same bytes"
    # This vocab.json is sorted by key, not id. The reference writes dict order, which would give
    # wrong ids here; write in id order (what its tiktoken path produces) and check ids are 0..n-1.
    in_file_order = list(tokens.values()) == sorted(tokens.values())
    print(f"vocab.json in id order: {in_file_order}; writing in id order")
    tokens = dict(sorted(tokens.items(), key=lambda kv: kv[1]))
    assert list(tokens.values()) == list(range(len(tokens))), "vocab ids are not 0..n-1"
    return tokens


def sinusoids(length: int, channels: int, max_timescale: float = 10000) -> np.ndarray:
    """OpenAI whisper/model.py sinusoids (torch float32), here in numpy float32; mlx-audio uses the same formula."""
    log_timescale_increment = np.log(max_timescale) / (channels // 2 - 1)
    inv_timescales = np.exp(-log_timescale_increment * np.arange(channels // 2, dtype=np.float32)).astype(np.float32)
    scaled_time = np.arange(length, dtype=np.float32)[:, None] * inv_timescales[None, :]
    return np.concatenate([np.sin(scaled_time), np.cos(scaled_time)], axis=1).astype(np.float32)


def mel_filters(n_mels: int, preproc: Path) -> np.ndarray:
    import mlx.core as mx
    from mlx_audio.utils import mel_filters as mlx_mel
    from transformers import WhisperFeatureExtractor

    mlx_f = np.array(mlx_mel(16000, 400, n_mels, norm="slaney", mel_scale=None).astype(mx.float32))
    fe = WhisperFeatureExtractor.from_pretrained(str(preproc.parent), local_files_only=True)
    hf_f = np.asarray(fe.mel_filters, dtype=np.float64).T  # transformers stores (n_freq, n_mels)
    assert mlx_f.shape == hf_f.shape == (n_mels, 201), (mlx_f.shape, hf_f.shape)
    diff = float(np.max(np.abs(mlx_f.astype(np.float64) - hf_f)))
    print(f"mel filters: mlx-audio {mlx_f.shape} vs transformers WhisperFeatureExtractor: largest abs difference {diff:.3e}")
    print(f"mel filters: largest value {float(mlx_f.max()):.6e}; writing mlx-audio's")
    return mlx_f.astype(np.float32)


def ggml_name(mlx: str) -> str:
    return mlx.replace(".mlp1.", ".mlp.0.").replace(".mlp2.", ".mlp.2.")


def expected_names(n_audio_layer: int, n_text_layer: int) -> set[str]:
    """The tensor names whisper.cpp's loader asks for (src/whisper.cpp, whisper_model_load)."""
    attn = ["attn.query.weight", "attn.query.bias", "attn.key.weight", "attn.value.weight", "attn.value.bias",
            "attn.out.weight", "attn.out.bias", "attn_ln.weight", "attn_ln.bias"]
    mlp = ["mlp.0.weight", "mlp.0.bias", "mlp.2.weight", "mlp.2.bias", "mlp_ln.weight", "mlp_ln.bias"]
    cross = ["cross_attn." + a[len("attn."):] if a.startswith("attn.") else "cross_" + a for a in attn]
    names = {"encoder.positional_embedding", "encoder.conv1.weight", "encoder.conv1.bias", "encoder.conv2.weight",
             "encoder.conv2.bias", "encoder.ln_post.weight", "encoder.ln_post.bias", "decoder.positional_embedding",
             "decoder.token_embedding.weight", "decoder.ln.weight", "decoder.ln.bias"}
    for i in range(n_audio_layer):
        names |= {f"encoder.blocks.{i}.{n}" for n in attn + mlp}
    for i in range(n_text_layer):
        names |= {f"decoder.blocks.{i}.{n}" for n in attn + mlp + cross}
    return names


def main() -> None:
    assert REF.exists(), REF
    mlx_dir = snapshot("mlx-community/whisper-large-v3-turbo")
    tok_dir = snapshot("openai/whisper-large-v3-turbo")
    hparams = json.loads((mlx_dir / "config.json").read_text())
    print("hparams:", hparams)

    free_gb = shutil.disk_usage(WT).free / 2**30
    print(f"free disk before: {free_gb:.1f} GiB")
    assert free_gb >= 12, "need at least 12 GB free"

    from safetensors import safe_open

    f = safe_open(str(mlx_dir / "weights.safetensors"), "np")
    mlx_names = list(f.keys())
    want = expected_names(hparams["n_audio_layer"], hparams["n_text_layer"])
    skipped = [n for n in mlx_names if n == "alignment_heads"]
    for n in skipped:
        print(f"skipped, not a weight: {n} {f.get_tensor(n).tolist()}")
    weights = [n for n in mlx_names if n not in skipped]
    unmatched = [n for n in weights if ggml_name(n) not in want]
    print("MLX tensors without a whisper.cpp name:", unmatched or "none")
    assert not unmatched, unmatched
    synthesized = sorted(want - {ggml_name(n) for n in weights})
    print("whisper.cpp tensors not in the MLX file (computed here):", synthesized)
    assert synthesized == ["encoder.positional_embedding"], synthesized

    filters = mel_filters(hparams["n_mels"], tok_dir / "preprocessor_config.json")
    tokens = vocab_tokens(tok_dir / "vocab.json")
    print(f"vocab: {len(tokens)} tokens written (n_vocab {hparams['n_vocab']}; whisper.cpp adds the special tokens)")

    use_f16 = True
    OUT.parent.mkdir(parents=True, exist_ok=True)
    tmp = OUT.with_suffix(".part")
    dtypes: dict[str, int] = {}
    with tmp.open("wb") as fout:
        fout.write(struct.pack("i", 0x67676D6C))
        for k in ["n_vocab", "n_audio_ctx", "n_audio_state", "n_audio_head", "n_audio_layer", "n_text_ctx",
                  "n_text_state", "n_text_head", "n_text_layer", "n_mels"]:
            fout.write(struct.pack("i", hparams[k]))
        fout.write(struct.pack("i", use_f16))
        fout.write(struct.pack("i", filters.shape[0]))
        fout.write(struct.pack("i", filters.shape[1]))
        fout.write(filters.astype("<f4").tobytes())  # row-major, same order as the reference's double loop
        fout.write(struct.pack("i", len(tokens)))
        for key in tokens:
            fout.write(struct.pack("i", len(key)))
            fout.write(key)

        def tensor(name: str, data: np.ndarray) -> None:
            data = data.squeeze()
            if name in ["encoder.conv1.bias", "encoder.conv2.bias"]:
                data = data.reshape(data.shape[0], 1)
            n_dims = len(data.shape)
            ftype = 1
            if n_dims < 2 or name in ("encoder.conv1.bias", "encoder.conv2.bias",
                                       "encoder.positional_embedding", "decoder.positional_embedding"):
                data = data.astype(np.float32)  # f16 -> f32 is exact
                ftype = 0
            else:
                assert data.dtype == np.float16, (name, data.dtype)  # keep the stored precision
            str_ = name.encode("utf-8")
            fout.write(struct.pack("iii", n_dims, len(str_), ftype))
            for i in range(n_dims):
                fout.write(struct.pack("i", data.shape[n_dims - 1 - i]))
            fout.write(str_)
            np.ascontiguousarray(data).tofile(fout)
            dtypes[ftype] = dtypes.get(ftype, 0) + 1

        pe = sinusoids(hparams["n_audio_ctx"], hparams["n_audio_state"])
        pe16 = pe.astype(np.float16).astype(np.float32)
        print(f"encoder.positional_embedding {pe.shape}: f32; largest change if rounded to f16 as mlx-audio "
              f"runs it: {float(np.max(np.abs(pe - pe16))):.3e}")
        tensor("encoder.positional_embedding", pe)
        for n in weights:
            data = f.get_tensor(n)
            if n in ("encoder.conv1.weight", "encoder.conv2.weight"):
                before = data.shape
                data = data.transpose(0, 2, 1)  # MLX (out, k, in) -> PyTorch (out, in, k)
                print(f"{n}: MLX {before} -> PyTorch {data.shape} (axes 1 and 2 swapped)")
            tensor(ggml_name(n), data)
    tmp.replace(OUT)
    print(f"tensors written: {sum(dtypes.values())} (f32: {dtypes.get(0, 0)}, f16: {dtypes.get(1, 0)})")
    print(f"wrote {OUT.relative_to(WT)}: {OUT.stat().st_size} bytes")
    print(f"free disk after: {shutil.disk_usage(WT).free / 2**30:.1f} GiB")


if __name__ == "__main__":
    sys.exit(main())
