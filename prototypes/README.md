# PROTOTYPE: Qwen3-TTS voice cloning in Rust (map ticket #7)

Throwaway. Answers one question: does a Candle (Rust) Qwen3-TTS 1.7B clone sound as good as today's MLX engine?
Not for main. `qwen3-tts-rs-fork/` is danielclough/qwen3-tts-rs at 4fc1a5da (MIT OR Apache-2.0; its LICENSE file
says MIT, Copyright Microsoft, kept as is).

## Run (Apple Silicon)
```bash
cd prototypes/qwen3-tts-rs-fork
cargo build --release -p qwen_tts_cli --features metal,accelerate,audio-loading
target/release/qwen-tts --model-path "$HOME/Library/Application Support/TakeTwo/models/qwen3-tts-1.7b-base" \
  --device metal --dtype bf16 --seed 1234 --temperature 0.7 --top-p 0.9 --top-k 40 --repetition-penalty 1.05 \
  --max-tokens 300 --language english --text "Line to read." --output out.wav \
  voice-clone -a ref.wav -t "Transcript of ref.wav"
```
The model folder is made from VoiceForge's MLX copy by `scripts/convert_mlx_weights.py` (no download).
`scripts/mlx_takes.py` makes today's raw takes for comparison.

## Results, 2026-11-27, M4 Pro 24 GB (machine checks, not proof of sound)
| | MLX (today) | Rust Metal | Rust CPU |
|---|---|---|---|
| Words right (Whisper), 3 lines | 3/3 | 3/3 | line 1: "in" heard as "and" |
| Voice match to reference | 0.951 / 0.929 / 0.928 | 0.913 / 0.943 / 0.926 | 0.922 |
| Silence loops | none | none | none |
| Samples a multiple of 1920 | yes | **no** (trimmed) | no |
| 3 lines (12 s of speech), model loaded once | ~10 s | ~20 s (+8 s load) | ~31 s for 1 line incl. load |
| Peak memory | 4.8 GB RSS | 10-12.7 GB footprint | 10.9 GB |
| Binary size | n/a | 19.6 MB | |

Verdict pending: Franz listens to the blind A/B page (kept private in eval/, not in git).
Not tested: Windows CUDA build, code parity with PyTorch under greedy decoding, long scripts.
