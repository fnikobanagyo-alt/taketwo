# Which Rust route runs Qwen3-TTS voice cloning on Mac and Windows?

Research for [issue #2](https://github.com/fnikobanagyo-alt/voiceforge/issues/2), which blocks prototype ticket #7. Researched on 2026-11-26 by reading source code, READMEs, crates.io and the Hugging Face API. Nothing was built or run, and no weights were downloaded. Every link to source is pinned to the commit that was read.

## Recommendation

**Use Candle, starting from the `qwen_tts` crate ([danielclough/qwen3-tts-rs](https://github.com/danielclough/qwen3-tts-rs)). Vendor it into our workspace as a fork instead of depending on it from crates.io. Keep TrevorS/qwen3-tts-rs as the second reference.**

Why this option:

- It is one of only two Rust codebases that cover all of the following:
  - in-context (ICL) cloning from a reference clip plus its transcript on the **1.7B Base** model
  - `metal` and `cuda` features
  - the official `Qwen/Qwen3-TTS-12Hz-1.7B-Base` safetensors, with no conversion ([README L24-L36](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/README.md#L24-L36)).
- It is the only one that tries a Windows CUDA build in CI. Its CI matrix includes a `windows-latest` CUDA build and macOS Metal tests ([release.yml L41-L67, L119-L136](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/.github/workflows/release.yml#L41-L136)).
- Its decode path does not treat codec id 0 as padding (see [the reference-boundary section](#the-reference-boundary-problem)).
- It is dual-licensed MIT OR Apache-2.0 on [crates.io](https://crates.io/crates/qwen_tts).

Why fork rather than depend:

- The crate had a single release day (0.1.0 and 0.1.1, both on 2026-01-29) and no commits after that ([crates.io API](https://crates.io/api/v1/crates/qwen_tts), [repo](https://github.com/danielclough/qwen3-tts-rs)).
- It pins Candle 0.9 ([qwen_tts/Cargo.toml L13-L16](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/qwen_tts/Cargo.toml#L13-L16)). Candle is now at 0.11.1 ([crates.io](https://crates.io/crates/candle-core)).
- In effect, this is the "write our own on Candle" option with most of the model already written.

**Risks, most serious first.** No candidate has shown 1.7B Base ICL working on both Metal and Windows CUDA.

1. **The Windows CUDA build is unproven.** The only release run of `qwen_tts` failed on exactly the job `build (windows-latest, x86_64-pc-windows-msvc, cuda,cudnn,flash-attn)`. Every other job passed, including Windows CPU and Linux CUDA ([run 21471622810](https://github.com/danielclough/qwen3-tts-rs/actions/runs/21471622810)). The logs have expired (HTTP 410), so the cause is unknown. Building without `flash-attn` on Windows is the first thing to try.
2. **Metal correctness is unproven for Candle Qwen3-TTS.**
   - TrevorS/qwen3-tts-rs, a sibling Candle 0.9 implementation, has an open bug: on an M3, generation is "stuck in an infinite loop producing silence tokens" while CPU is correct ([issue #12](https://github.com/TrevorS/qwen3-tts-rs/issues/12), opened 2026-02-23, no replies).
   - The macOS CI job of `qwen_tts` passed. Its integration tests pick Metal when they can ([integration_tests.rs L45-L49](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/qwen_tts/tests/integration_tests.rs#L45-L49)). But its ICL test uses the **0.6B** Base ([L600-L603](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/qwen_tts/tests/integration_tests.rs#L600-L603)), and I could not confirm that the hosted runner actually used Metal.
3. **No Candle Metal speed has been published.**
   - The only Apple Silicon numbers for a Rust Qwen3-TTS come from second-state's MLX backend on an M4. 1.7B CustomVoice runs at RTF about 3.0, meaning 3 s of work per 1 s of audio, and 0.6B ICL cloning at 2.38 ([README L301-L350](https://github.com/second-state/qwen3_tts_rs/blob/efe3285ece133c452fe888d6eb52801761d0c501/README.md#L301-L350)).
   - Candle Metal could be slower or faster. Mac speed is a go/no-go gate for #7.
4. **The licence file is inconsistent.** The crate declares `MIT OR Apache-2.0`, but the repository's LICENSE file is MIT with "Copyright (c) 2025 Microsoft" ([LICENSE](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/LICENSE)). It looks like a copied template. Ask the author, or keep both notices when vendoring.
5. **We own maintenance.** With a fork we carry Candle upgrades and Metal/CUDA kernel issues ourselves.

**Fallback if Candle Metal fails the #7 gate:** the second-state crate `qwen3-tts-rs` 0.2.2. It runs MLX through `mlx-c` on Mac and libtorch through `tch` elsewhere ([Cargo.toml L54-L60](https://github.com/second-state/qwen3_tts_rs/blob/efe3285ece133c452fe888d6eb52801761d0c501/Cargo.toml#L54-L60)). Its costs:

- It uses two backends.
- It needs a libtorch native library on Windows.
- It publishes no Windows build: the v0.2.2 assets cover Linux and macOS only ([release v0.2.2](https://github.com/second-state/qwen3_tts_rs/releases/tag/v0.2.2)).

**Ruled out:**

- `any-tts`: no Qwen3 cloning.
- `wavekat-tts`: no 1.7B Base, and no GPU on Mac.
- darkautism/qwen3-tts: Linux/ARM CPU only.
- `speakers-qwen3-tts`: a republished fork of TrevorS that its own user calls experimental.

Details for each are below.

## What #7 should test

Run these in order and stop at the first hard failure.

1. **Windows CUDA build.** `cargo build --release --features cuda,cudnn` on Windows plus an NVIDIA GPU, first without `flash-attn` and then with it. Record the CUDA and cuDNN versions. This reproduces the failed CI job above.
2. **Metal sanity.** On the M4 Pro, generate 1.7B Base ICL with a fixed seed and greedy decoding on CPU and on Metal.
   - The Metal output must not be silent or looping (TrevorS #12).
   - The Metal codec ids should match CPU for at least the first N frames.
3. **Parity with the reference implementation.** On the same reference clip, transcript and text with greedy decoding, compare the reference codes and generated codes against PyTorch `qwen-tts` ([QwenLM/Qwen3-TTS](https://github.com/QwenLM/Qwen3-TTS), the ground truth). The 12 Hz tokenizer is not bit-exact across backends, so report where the codes first differ.
4. **Reference boundary.** Assert that decoded samples equal `total_frames × 1920` and that the cut equals `ref_frames × 1920`. Do this:
   - on a run longer than 512 frames, which exercises `qwen_tts`'s chunked decode path (`CHUNK_THRESHOLD = 512`, [generate.rs L71-L75](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/qwen_tts/src/model/generate.rs#L71-L75); a 12 s reference plus about 30 s of speech crosses it)
   - on a reference whose codes contain id 0.
   Better still, replace the proportional cut with the exact one, as VoiceForge's `backend/app/qwen_compat.py` does.
5. **Speed and memory.** Measure Mac Metal against today's mlx-audio 0.5.7 on the same M4 Pro, using RTF and peak RSS. Do the same on the Windows GPU.
6. **Listening.** Franz listens to paired outputs. Measurements alone are not proof of audio quality.

## Comparison

"1.7B ICL" means the 1.7B Base model with a reference clip and its exact transcript. That is the mode VoiceForge uses.

| Option | 1.7B ICL clone | Metal (Mac) | CUDA / Windows | Weights | Speed reported | Last activity | Licence |
|---|---|---|---|---|---|---|---|
| `qwen_tts` (Candle), danielclough | Yes | `metal` feature; macOS CI passed | `cuda` feature; Windows CUDA CI build **failed** | Official `Qwen/...-Base` safetensors | None | 0.1.1 on 2026-01-29; last push the same day | MIT OR Apache-2.0 (the LICENSE file says MIT, © Microsoft) |
| TrevorS/qwen3-tts-rs (Candle) | Yes | `metal` feature; **open bug: silence loop on M3** | `cuda` + `flash-attn`; CI is Linux CPU only | Official safetensors | DGX Spark CUDA: 1.7B Base RTF 0.65 | Last push 2026-02-06; not on crates.io under its own name | MIT in Cargo.toml; no LICENSE file |
| `speakers-qwen3-tts` 0.3.0 | Fork of TrevorS; its user calls ICL "experimental" | As TrevorS | As TrevorS | As TrevorS | None | Published 2026-03-13 | MIT |
| `any-tts` 0.2.0 (Candle) | **No**: cloning returns an error | `metal` feature | `cuda` feature | Official CustomVoice and VoiceDesign | None for Qwen | 2026-10-09 | MIT OR Apache-2.0 |
| `wavekat-tts` 0.0.8 (ONNX Runtime) | **0.6B only** | CoreML provider crashes on 1.7B graphs, so Mac runs on CPU | CUDA and TensorRT providers | Converted ONNX by `wavekat` on Hugging Face; no 1.7B Base | None for clone | 2026-11-15 (active) | Apache-2.0 |
| `qwen3_tts` / `qwen3-tts-rs` (second-state; tch + MLX) | Yes (benchmarked on 0.6B) | **MLX** backend through mlx-c | libtorch (`tch`) with CUDA; **no Windows build shipped** | Official safetensors | M4 MLX: 1.7B RTF about 3.0; 0.6B ICL 2.38 | `qwen3-tts-rs` 0.2.2 on 2026-03-29; the ticket's `qwen3_tts` is **yanked** | Apache-2.0 |
| darkautism/qwen3-tts (Candle GGUF + ONNX vocoder) | ICL by voice JSON; model size not verified | No; CPU, aimed at Linux/ARM SBCs | ONNX vocoder can use CUDA on Linux; no Windows | Its own GGUF/ONNX conversions | About 5.5 tok/s on RK3588 | 2026-03-06 | MIT |
| Our own on Candle | Whatever we write | Candle `metal` | Candle `cuda` | Official safetensors | — | — | Ours |

## Details by option

### `qwen_tts` (Candle), danielclough/qwen3-tts-rs

- **Cloning.** Supports x-vector mode and ICL from reference audio plus `--ref-text`. The README example uses `Qwen/Qwen3-TTS-12Hz-1.7B-Base` ([README L24-L36, L100-L135](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/README.md#L24-L135)).
- **Reusable voice prompts.** They can be saved as safetensors (`--save-prompt` / `--load-prompt`), which suits VoiceForge's saved voices (same README).
- **Platforms.**
  - Features `metal`, `accelerate`, `cuda`, `cudnn`, `flash-attn` and `nccl` exist.
  - The README gives a Windows NVIDIA install line: `cargo install qwen_tts_cli --features cuda,flash-attn,cudnn,audio-loading` ([README L27-L31](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/README.md#L27-L31)).
  - GPU support is chosen at compile time with no runtime detection ([TESTS.md](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/TESTS.md)). So we need separate Mac and Windows builds, which we need anyway.
- **CI evidence.** Run [21471622810](https://github.com/danielclough/qwen3-tts-rs/actions/runs/21471622810) on tag v0.1.1:
  - Passed: test-linux, test-macos, test-windows, and the builds for macOS Metal, Windows CPU (x86_64 and aarch64) and Linux CUDA.
  - **Failed:** Windows CUDA (`cuda,cudnn,flash-attn`).
  - Skipped: release and publish. That is why no GitHub release exists.
- **Weights.** The official Hugging Face repo `Qwen/Qwen3-TTS-12Hz-1.7B-Base`, with `model.safetensors` and `speech_tokenizer/model.safetensors`, Apache-2.0, last modified 2026-01-23 ([HF API](https://huggingface.co/api/models/Qwen/Qwen3-TTS-12Hz-1.7B-Base)).
- **Speed.** Not reported. A `timing` feature exists.
- **Long text.** Default `max_tokens` is 2048, about 2 min 40 s at 12.5 Hz. The 12 Hz model "should generate audio up to around 10 minutes before it becomes decoherent" ([README L227-L246](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/README.md#L227-L246)).
- **Maintenance.** 28 stars, one closed issue, no push since 2026-01-29 ([GitHub API](https://api.github.com/repos/danielclough/qwen3-tts-rs)). There have been 43k crates.io downloads.

### TrevorS/qwen3-tts-rs (Candle), and `speakers-qwen3-tts`

- **Cloning and platforms.**
  - Supports x-vector or ICL cloning on Base models.
  - Has CUDA (bf16, Flash Attention 2) and Metal features.
  - Uses the official weights, including `Qwen/Qwen3-TTS-12Hz-1.7B-Base` (3.9 GB) ([README](https://github.com/TrevorS/qwen3-tts-rs/blob/711ceee07cad92673f86de8997bdf54c30caa49f/README.md)).
  - The changelog says "ICL voice cloning now works correctly with proper reference audio".
- **Speed.** Measured on an NVIDIA DGX Spark (GB10): 1.7B Base CUDA BF16 runs at RTF 0.65, 19.4 tok/s; CPU F32 at RTF 5.4–6.5 ([README L64-L70](https://github.com/TrevorS/qwen3-tts-rs/blob/711ceee07cad92673f86de8997bdf54c30caa49f/README.md#L64-L70)). There are no Metal numbers.
- **Open issues.** Metal is broken on M3 ([#12](https://github.com/TrevorS/qwen3-tts-rs/issues/12)); there are T4 and 0.6B audio issues ([#15](https://github.com/TrevorS/qwen3-tts-rs/issues/15)). CI runs on `ubuntu-latest` only and excludes `cuda`/`metal` ([ci.yml L12-L18](https://github.com/TrevorS/qwen3-tts-rs/blob/711ceee07cad92673f86de8997bdf54c30caa49f/.github/workflows/ci.yml#L12-L18)).
- **Licence.** `license = "MIT"` in [Cargo.toml L6](https://github.com/TrevorS/qwen3-tts-rs/blob/711ceee07cad92673f86de8997bdf54c30caa49f/Cargo.toml#L6), but GitHub finds no LICENSE file (licence API returns 404). Last push 2026-02-06, 152 stars.
- **`speakers-qwen3-tts` 0.3.0 (MIT, 2026-03-13)** points at OneNoted/speake-rs ([crates.io](https://crates.io/crates/speakers-qwen3-tts)).
  - Its version and feature list (`metal`, `cuda`, `flash-attn`, `hub`, `cli`) match TrevorS's `qwen3-tts` 0.3.0.
  - speake-rs depends on `OneNoted/qwen3-tts-rs`, which is a GitHub fork of TrevorS ([speake-rs Cargo.toml L22](https://github.com/OneNoted/speake-rs/blob/a7f2ee86647946b8449172ee25fa97a7409516c0/Cargo.toml#L22); fork parent per GitHub API, fork last pushed 2026-02-18).
  - speake-rs is a Linux daemon. Its README says "ICL voice cloning is mostly untested in this project right now and should be treated as experimental" ([README L30](https://github.com/OneNoted/speake-rs/blob/a7f2ee86647946b8449172ee25fa97a7409516c0/README.md#L30)).
  - *I read TrevorS HEAD, not the OneNoted fork's diff.*

### `any-tts` 0.2.0 (Candle)

- **Qwen3 cloning is not implemented.** A request with reference audio returns: "Qwen3-TTS: voice cloning from reference audio is not yet implemented. Use a named speaker via .with_voice() instead." ([src/models/qwen3_tts/model.rs L284-L289](https://github.com/TM9657/any-tts/blob/39154d2174185665c7b7e8e47eec7063e5a09b7e/src/models/qwen3_tts/model.rs#L284-L289)).
- **Models.** The default is `Qwen/Qwen3-TTS-12Hz-1.7B-CustomVoice` ([README L47, L416](https://github.com/TM9657/any-tts/blob/39154d2174185665c7b7e8e47eec7063e5a09b7e/README.md#L47)).
- **Platforms and licence.** Has `metal`, `cuda` and `accelerate` features ([Cargo.toml L73-L78](https://github.com/TM9657/any-tts/blob/39154d2174185665c7b7e8e47eec7063e5a09b7e/Cargo.toml#L73-L78)). MIT OR Apache-2.0, actively released (0.2.0 on 2026-10-09).
- **Verdict.** Ruled out for cloning today. It could become an option later.

### `wavekat-tts` 0.0.8 (ONNX Runtime)

- **Cloning.** Clone support (`Qwen3TtsClone`) covers the **0.6B Base only** ([README L25, model-weights table](https://github.com/wavekat/wavekat-tts/blob/fc24370bc590da299d00fd12c05a5eff0e059c09/README.md#L25)). The design doc says the 1.7B Base "can be added later by swapping model files" ([docs/08 L23](https://github.com/wavekat/wavekat-tts/blob/fc24370bc590da299d00fd12c05a5eff0e059c09/docs/08-qwen3-tts-0.6b-voice-clone.md#L23)).
- **Weights.** Converted by wavekat. Hugging Face lists only `wavekat/Qwen3-TTS-0.6B-Base-ONNX` and `wavekat/Qwen3-TTS-1.7B-VoiceDesign-ONNX`; there is no 1.7B Base ([HF API](https://huggingface.co/api/models?author=wavekat)). The 0.6B Base ONNX card is Apache-2.0.
- **Mac.**
  - The `coreml` feature exists, but the project's own MLX design doc says ORT's CoreML provider "crashes (SIGSEGV) when compiling 1.7B-parameter transformer graphs" and that CoreML's static shapes conflict with the KV cache ([docs/05 L17](https://github.com/wavekat/wavekat-tts/blob/fc24370bc590da299d00fd12c05a5eff0e059c09/docs/05-mlx-backend.md#L17)).
  - The MLX backend is "Planned — not being implemented now" (same doc, L3).
  - So on a Mac the language model runs on CPU.
- **Windows.** The CUDA and TensorRT providers exist (`ort` 2.0.0-rc.12).
- **Maintenance.** Active (0.0.8 on 2026-11-15), Apache-2.0, 3 stars, "Early development".
- **Verdict.** It fails the 1.7B requirement and has no Mac GPU path. Its ONNX export tools ([tools/qwen3-tts-onnx](https://github.com/wavekat/wavekat-tts/tree/fc24370bc590da299d00fd12c05a5eff0e059c09/tools/qwen3-tts-onnx)) could convert the 1.7B Base ourselves. That is untested, and we would host converted weights.

### `qwen3_tts` → `qwen3-tts-rs` (second-state, tch/libtorch and MLX)

- **Crate status.** The `qwen3_tts` crate named in the ticket has both versions (0.1.0, 0.1.1) **yanked** ([crates.io versions](https://crates.io/api/v1/crates/qwen3_tts/versions)). The live crate is `qwen3-tts-rs` 0.2.2 (2026-03-29). `juntao/qwen3_tts_rs` redirects to second-state/qwen3_tts_rs (238 stars, last push 2026-05-19).
- **Backends.** The default is libtorch through `tch` 0.20; there is also an `mlx` feature for Apple Silicon through an `mlx-c` submodule ([Cargo.toml L54-L60](https://github.com/second-state/qwen3_tts_rs/blob/efe3285ece133c452fe888d6eb52801761d0c501/Cargo.toml#L54-L60)).
- **Cloning and weights.** A `voice_clone` binary does ICL when `ref_text` is given. Weights are official safetensors from `Qwen/...` (install.sh L164-L165, [src/weights.rs](https://github.com/second-state/qwen3_tts_rs/blob/efe3285ece133c452fe888d6eb52801761d0c501/src/weights.rs)).
- **Windows.** The README mentions Windows nowhere and gives libtorch download steps for Linux only. The release assets cover Linux and macOS only. Windows would need us to wire up libtorch with CUDA ourselves.
- **Speed (M4, MLX).** 0.6B ICL clone RTF 2.38; 1.7B CustomVoice RTF about 2.9–3.4 ([README L301-L350](https://github.com/second-state/qwen3_tts_rs/blob/efe3285ece133c452fe888d6eb52801761d0c501/README.md#L301-L350)).
- **Licence.** Apache-2.0 per Cargo.toml; GitHub detects no licence file.

### darkautism/qwen3-tts (Candle GGUF talker + ONNX vocoder)

- **Scope.** A distributed pipeline for Linux SBCs (RK3588). Talker and predictor run as Candle GGUF on CPU, the vocoder as ONNX FP32 on CPU, with an optional RKNN NPU and an ONNX CUDA option on Linux ([README](https://github.com/darkautism/qwen3-tts/blob/ad5b95d7bd828020ed940f6af8c19146cb54fce1/README.md)).
- **Cloning.** ICL through a voice JSON of `ref_text` plus `codec_tokens`.
- **Platforms and weights.** No Metal and no Windows. It uses a Candle fork for aarch64 SDOT and its own converted GGUF/ONNX weights. MIT, last push 2026-03-06.
- **Not verified:** which model size it targets, and how it trims the reference. Ruled out on platform alone.

### Writing our own on Candle

- Both Candle ports above show the architecture fits Candle 0.9.
- Starting from scratch would repeat that work. A vendored `qwen_tts` gives us the same control, including Candle upgrades and the exact reference boundary, with the model already written.
- The official PyTorch package `qwen-tts` 0.1.1 (Apache-2.0, [pyproject.toml L7, L19](https://github.com/QwenLM/Qwen3-TTS/blob/022e286b98fbec7e1e916cb940cdf532cd9f488e/pyproject.toml#L7-L19)) stays the behaviour ground truth for parity tests.

## The reference-boundary problem

VoiceForge's `backend/app/qwen_compat.py` works around two things in mlx-audio 0.5.7:

- **(a)** The decoder counts non-zero codes to find the audio length, so codec id 0 is treated as padding.
- **(b)** ICL trims the reference proportionally (`ref_len / total_len × wav_len`), which goes wrong once (a) has shortened the waveform.

**What the official code does:**

- **(a) does not occur there.** Batches are padded with **-1** ([qwen3_tts_tokenizer.py L329](https://github.com/QwenLM/Qwen3-TTS/blob/022e286b98fbec7e1e916cb940cdf532cd9f488e/qwen_tts/inference/qwen3_tts_tokenizer.py#L329)). Length is `(audio_codes[..., 0] > -1).sum(1) * decode_upsample_rate`, so code 0 counts as audio ([modeling_qwen3_tts_tokenizer_v2.py L1012-L1017](https://github.com/QwenLM/Qwen3-TTS/blob/022e286b98fbec7e1e916cb940cdf532cd9f488e/qwen_tts/core/tokenizer_12hz/modeling_qwen3_tts_tokenizer_v2.py#L1012-L1017)). Bug (a) is specific to mlx-audio.
- **(b) is in the official code too:** `cut = int(ref_len / max(total_len, 1) * wav.shape[0])` ([qwen3_tts_model.py L624-L630](https://github.com/QwenLM/Qwen3-TTS/blob/022e286b98fbec7e1e916cb940cdf532cd9f488e/qwen_tts/inference/qwen3_tts_model.py#L624-L630)). It is harmless whenever the decoded length is exactly `total_frames × 1920`, apart from float rounding of about one sample.

**What the Rust options do:**

| Option | Code 0 treated as padding? | Cut | Decoded length exactly frames × 1920? |
|---|---|---|---|
| `qwen_tts` | No. Length comes from the EOS step (`effective_lengths`), not code values ([generate.rs L77-L89](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/qwen_tts/src/model/generate.rs#L77-L89)) | Proportional ([L588-L596](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/qwen_tts/src/model/generate.rs#L588-L596)) | Likely, from reading: chunked decode drops only left-context samples per chunk ([decoder/v2.rs L250-L286](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/qwen_tts/src/audio/decoder/v2.rs#L250-L286)). **Untested; assert in #7.** |
| TrevorS / `speakers-qwen3-tts` | No such counting found | Proportional, integer form `ref_len * audio.len() / total_frames` ([lib.rs L1030-L1040](https://github.com/TrevorS/qwen3-tts-rs/blob/711ceee07cad92673f86de8997bdf54c30caa49f/src/lib.rs#L1030-L1040)) | Not checked |
| `wavekat-tts` | No such counting found | Proportional ([clone_model.rs L763-L764](https://github.com/wavekat/wavekat-tts/blob/fc24370bc590da299d00fd12c05a5eff0e059c09/crates/wavekat-tts/src/backends/qwen3_tts/clone_model.rs#L763-L764)) | Vocoder exported as `T × upsample` ([export_vocoder.py L52](https://github.com/wavekat/wavekat-tts/blob/fc24370bc590da299d00fd12c05a5eff0e059c09/tools/qwen3-tts-onnx/export_vocoder.py#L52)); not checked at runtime |
| second-state `qwen3-tts-rs` | No such counting found | Proportional ([inference.rs L1767-L1768](https://github.com/second-state/qwen3_tts_rs/blob/efe3285ece133c452fe888d6eb52801761d0c501/src/inference.rs#L1767-L1768)) | Not checked |
| `any-tts` | n/a (no cloning) | n/a | n/a |
| darkautism | Not read | Not read | Not read |

"No such counting found" means a search for `> 0`, `!= 0`, `gt(0)` and `ne(0)` length counting in each crate's Qwen code found nothing (searched 2026-11-26).

**Verdict.** None of the Rust options I read is known to have bug (a). For `qwen_tts`, the tokenizer `decode` returns the decoder output with no length trim based on code values ([tokenizer/v2/mod.rs L207-L211](https://github.com/danielclough/qwen3-tts-rs/blob/4fc1a5da82f1bb8a69fd6e615662913dac54b37e/qwen_tts/src/audio/tokenizer/v2/mod.rs#L207-L211)). Every cloning port I read (`qwen_tts`, TrevorS, `wavekat-tts`, second-state) copies the official proportional cut (b). darkautism was not read. That is safe only if their decoders return exactly `frames × 1920`, which nobody has verified at runtime. In our fork, replace the cut with `ref_frames × 1920` and assert the decoded length. This is the same rule `qwen_compat.py` enforces.

## Not verified

- Whether any candidate actually runs 1.7B Base ICL on Metal, and at what speed. No one has published it.
- Why `qwen_tts`'s Windows CUDA build failed (CI logs expired).
- Whether the `qwen_tts` macOS CI job used the Metal device or fell back to CPU.
- The OneNoted fork's changes on top of TrevorS.
- darkautism's model size and reference trimming.
- Whether second-state's libtorch path builds on Windows with CUDA.
- Audio quality of any option. Nothing was run or listened to.
