"""Record Python parity vectors for the Rust speaker encoder (step S14, `taketwo_voice::encoder`).

    backend/.venv/bin/python -B rust/golden/record_encoder.py

Check first that Franz's app is idle (GET 127.0.0.1:8766/api/status: "running":null, "queued":0) and that
no Rust model test runs. One short process; the model is the 5.7 MB voice encoder only.

Clips (made with macOS `say`, never a voice of Franz's; made only when a wav is missing, because `say`
output may change between macOS versions, and every vector below depends on the wav bytes):
  a.wav  ~4 s at 24 kHz, Samantha (the resample runs)
  b.wav  1.1 s at 24 kHz, Samantha (pipeline.py embeds anything over 1.0 s; shorter than one partial window)
  c.wav  3 s at 16 kHz, Samantha, 0.5 s of silence before and after (the trim runs, no resample)
  d.wav  ~4 s at 24 kHz, Daniel (a second voice)
Writes rust/golden/encoder.json and, under rust/golden/encoder/, per clip the resampled samples
(<clip>.resampled.f32), the mel (<clip>.mel.f32, frames x 40), plus the resample FIR (fir_24000_16000.f64)
and the mel filterbank (mel_filters.f32, 40 x 201). Raw little-endian arrays.

The stages are the code of ml.speaker_embedding -> VoiceEncoder.embeds_from_wavs. The trim is inline in
embeds_from_wavs, so it is copied here to get its bounds; the recorder then checks that its own stages,
fed to embeds_from_mels, give the embedding of the real ml.speaker_embedding.
Safety: the data folder is a fresh temporary folder (checked before ml is imported), Hugging Face is offline,
the weights come from the local cache only (local_files_only=True) and a missing file fails, never downloads.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

os.environ["HF_HUB_OFFLINE"] = "1"  # before huggingface_hub is imported anywhere
os.environ["TRANSFORMERS_OFFLINE"] = "1"

HERE = Path(__file__).resolve().parent
WT = HERE.parent.parent
BACKEND = WT / "backend"
OUT = HERE / "encoder.json"
DIR = HERE / "encoder"

CLIPS = [
    # name, voice, sample rate, text, seconds of speech kept, seconds of silence before and after
    ("a", "Samantha", 24000, "The quick brown fox jumps over the lazy dog by the quiet river.", 4.0, 0.0),
    ("b", "Samantha", 24000, "Hello there, friend.", 1.1, 0.0),
    ("c", "Samantha", 16000, "Good morning to everyone here.", 2.0, 0.5),
    ("d", "Daniel", 24000, "A gentle breeze moved slowly across the open green fields.", 4.0, 0.0),
]


def make_clip(np, sf, name, voice, sr, text, keep, pad):
    wav = DIR / f"{name}.wav"
    if wav.exists():
        return
    with tempfile.TemporaryDirectory() as td:
        raw = Path(td) / "say.wav"
        subprocess.run(["say", "-v", voice, "-o", str(raw), "--file-format=WAVE", f"--data-format=LEI16@{sr}", text], check=True)
        a, got_sr = sf.read(str(raw), dtype="float32")
    assert got_sr == sr, (got_sr, sr)
    if a.ndim > 1:
        a = a.mean(axis=1)
    n = int(round(keep * sr))
    a = a[:n] if len(a) >= n else np.pad(a, (0, n - len(a)))
    if pad:
        z = np.zeros(int(round(pad * sr)), dtype=np.float32)
        a = np.concatenate([z, a, z])
    sf.write(str(wav), a, sr, subtype="PCM_16")


def trim(np, math, wav_np, trim_top_db=20):
    """embeds_from_wavs lines 410-450, returning the bounds and the smallest distance to the threshold."""
    frame_length = 2048
    hop_length = 512
    n_frames = 1 + (len(wav_np) - frame_length) // hop_length
    if n_frames <= 0:
        return 0, len(wav_np), None
    rms = np.array([math.sqrt(np.mean(wav_np[i * hop_length: i * hop_length + frame_length] ** 2)) for i in range(n_frames)])
    rms_db = 20 * np.log10(np.maximum(rms, 1e-10))
    threshold = np.max(rms_db) - trim_top_db
    non_silent = np.where(rms_db >= threshold)[0]
    if len(non_silent) == 0:
        return 0, len(wav_np), None
    start = int(non_silent[0]) * hop_length
    end = min((int(non_silent[-1]) + 1) * hop_length + frame_length, len(wav_np))
    return start, end, float(np.min(np.abs(rms_db - threshold)))


def main() -> None:
    tmp = Path(tempfile.mkdtemp(prefix="taketwo-golden."))
    try:
        os.environ["TAKETWO_DATA"] = os.environ["VOICEFORGE_DATA"] = str(tmp)
        sys.path.insert(0, str(BACKEND))
        from app import config

        assert config.DATA_DIR.resolve().is_relative_to(tmp.resolve()), config.DATA_DIR  # never the real library
        import math

        import mlx.core as mx
        import numpy as np
        import soundfile as sf
        from huggingface_hub import hf_hub_download
        from mlx_audio.resample import _polyphase_filter
        from mlx_audio.tts.models.chatterbox.voice_encoder.melspec import melspectrogram
        from mlx_audio.tts.models.chatterbox.voice_encoder.voice_encoder import VoiceEncoder, get_frame_step, get_num_wins
        from mlx_audio.utils import mel_filters, resample_audio

        from app import ml

        # ml._voice_encoder falls back to a download; load the cached file ourselves and hand it the model.
        weights = hf_hub_download(config.VOICE_ENCODER_REPO, "ve.safetensors", local_files_only=True)
        ve = VoiceEncoder()
        ve.load_weights(list(ve.sanitize(mx.load(weights)).items()))
        mx.eval(ve.parameters())
        ml._spk = ve
        hp = ve.hp

        DIR.mkdir(exist_ok=True)
        for c in CLIPS:
            make_clip(np, sf, *c)

        _, _, fir = _polyphase_filter(24000, 16000)
        fir.astype("<f8").tofile(DIR / "fir_24000_16000.f64")
        filters = np.array(mel_filters(sample_rate=hp.sample_rate, n_fft=hp.n_fft, n_mels=hp.num_mels, f_min=hp.fmin,
                                       f_max=hp.fmax, norm="slaney", mel_scale="slaney"), dtype=np.float32)
        filters.astype("<f4").tofile(DIR / "mel_filters.f32")

        frame_step = get_frame_step(0.5, 1.3, hp)
        clips, emb = [], {}
        for name, voice, sr, _text, _keep, _pad in CLIPS:
            a, file_sr = sf.read(str(DIR / f"{name}.wav"), dtype="float32")
            assert file_sr == sr
            e = ml.speaker_embedding(a, sr)
            assert e.dtype == np.float32 and e.shape == (256,)
            emb[name] = e

            r = np.array(resample_audio(mx.array(a), sr, hp.sample_rate), dtype=np.float32)
            start, end, margin = trim(np, math, r)
            t = r[start:end]
            mel = np.array(mx.transpose(melspectrogram(mx.array(t), hp)), dtype=np.float32)  # (frames, 40)
            n_wins, target_n = get_num_wins(mel.shape[0], frame_step, 0.8, hp)

            # Self-check: these stages, fed to the model, give ml.speaker_embedding's vector.
            mine = np.array(ve.embeds_from_mels([mx.array(mel)], as_spk=True, rate=1.3)).reshape(-1)
            mine = (mine / (np.linalg.norm(mine) + 1e-9)).astype(np.float32)
            assert np.max(np.abs(mine - e)) < 1e-6, (name, np.max(np.abs(mine - e)))

            r.astype("<f4").tofile(DIR / f"{name}.resampled.f32")
            mel.astype("<f4").tofile(DIR / f"{name}.mel.f32")
            clips.append({
                "name": name, "wav": f"encoder/{name}.wav", "voice": voice, "sr": sr, "n_in": int(len(a)),
                "resampled_len": int(len(r)), "trim_start": start, "trim_end": end,
                "trim_margin_db": margin, "mel_frames": int(mel.shape[0]),
                "frame_step": frame_step, "n_wins": n_wins, "target_n": target_n,
                "partial_starts": [i * frame_step for i in range(n_wins)],
                "embedding": [float(v) for v in e],
            })
            print(f"{name}: {len(a)} samples at {sr} -> {len(r)}, trim {start}..{end} (margin {margin} dB), "
                  f"{mel.shape[0]} mel frames, {n_wins} partial(s)")

        names = [c[0] for c in CLIPS]
        pairs = [{"a": x, "b": y, "cosine": ml.cosine(emb[x], emb[y])} for i, x in enumerate(names) for y in names[i + 1:]]
        for p in pairs:
            print(f"cosine {p['a']} {p['b']}: {p['cosine']}")
        doc = {
            "source": "ml.speaker_embedding / ml.cosine (mlx_audio chatterbox VoiceEncoder), rust/golden/record_encoder.py",
            "versions": {"numpy": np.__version__, "mlx": mx.__version__, "scipy": __import__("scipy").__version__,
                         "mlx_audio": __import__("importlib.metadata", fromlist=["x"]).version("mlx-audio")},
            "fir": "encoder/fir_24000_16000.f64", "mel_filters": "encoder/mel_filters.f32",
            "clips": clips, "pairs": pairs,
        }
        OUT.write_text(json.dumps(doc, indent=1) + "\n")
        print(f"wrote {OUT.relative_to(WT)} and {len(list(DIR.iterdir()))} files in {DIR.relative_to(WT)}")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
