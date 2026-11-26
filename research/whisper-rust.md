# Whisper with word timings from Rust (Mac Metal, Windows CUDA)

Answers issue #3: which Rust route runs Whisper large-v3-turbo with word-level timestamps on Apple Silicon (Metal) and Windows + NVIDIA (CUDA), with no Python at runtime?

Researched 2026-11-26 from primary sources only: repos, source files, crate registries and model repos. Nothing was installed, built, downloaded or run, so every speed and accuracy statement below comes from the sources, not from measurement. Sources are pinned to the commits read that day:

- whisper.cpp [`79b1191`](https://github.com/ggml-org/whisper.cpp/tree/79b11911d9d38c4be2fc7dc8b4efeff3324c090f)
- Candle [`9c1fcff`](https://github.com/huggingface/candle/tree/9c1fcffaf956fcfd629fdddc9fc25adefb95c052)
- sherpa-onnx [`696ce07`](https://github.com/k2-fsa/sherpa-onnx/tree/696ce07efd1cd67cfd7b7fed57f2e34d56568942)
- ctranslate2-rs [`f80ff84`](https://github.com/jkawamoto/ctranslate2-rs/tree/f80ff848aafa1276588d24cb2f8b4fc5a6a452d3)
- floneum [`7a9ead2`](https://github.com/floneum/floneum/tree/7a9ead2493b31ef670a35ed0507de1188006329e)
- mlx-audio [`93cfb9f`](https://github.com/Blaizzy/mlx-audio/tree/93cfb9ff82c10e610ce65570b1c2efc1f0f9411d)

## Recommendation

**Use whisper.cpp with DTW token timestamps, through our own thin Rust binding (bindgen over `whisper.h`) pinned to a whisper.cpp commit at or after PR #4170. Port the word-grouping step from mlx-audio's `timing.py` into Rust.**

- **Why whisper.cpp.** It is the only maintained option that has all five of these at once: Metal on Mac, CUDA on Windows, a ready large-v3-turbo GGML file, an alignment-head preset for turbo, and active upkeep. Its DTW follows the same cross-attention method VoiceForge uses today in mlx-audio. Sources: [whisper.cpp README](https://github.com/ggml-org/whisper.cpp/blob/79b11911d9d38c4be2fc7dc8b4efeff3324c090f/README.md), [`whisper.h`](https://github.com/ggml-org/whisper.cpp/blob/79b11911d9d38c4be2fc7dc8b4efeff3324c090f/include/whisper.h) and [`timing.py`](https://github.com/Blaizzy/mlx-audio/blob/93cfb9ff82c10e610ce65570b1c2efc1f0f9411d/mlx_audio/stt/models/whisper/timing.py).
- **Why not `whisper-rs` as shipped.** The crate is the obvious binding, but its GitHub repo is archived and development moved to Codeberg. Its latest release, 0.16.0 from 2026-03-12, bundles whisper.cpp v1.8.3, which is 2,058 commits behind v1.9.6. It predates the fix that gives DTW a correct token *start* (PR #4170, merged 2026-11-25 and not yet in any tagged release). Details are in the whisper-rs section below.
- **Fallback.** `whisper-rs` with a patched `whisper-rs-sys` submodule, or a git dependency, is an acceptable stopgap. Its safe API already exposes the DTW settings and the raw token struct.
- **Second choice: ct2rs (CTranslate2).** It already returns word timings built by faster-whisper-style DTW, and runs on Windows CUDA. It has **no GPU on Mac** (CPU with Accelerate only), so it cannot serve the Mac target well.
- **Not recommended: Candle.** Its Whisper example has no word timings. The DTW PR was closed unmerged, and the attention weights are thrown away inside the model. We would have to fork the model code and write DTW ourselves.
- **Not recommended: sherpa-onnx.** It has DTW token timestamps, but needs a turbo ONNX file re-exported with attention outputs, and none is published for turbo.
- **Not recommended: rwhisper.** It has DTW word timings and Metal and DX12 through wgpu, but no CUDA, and the released crate has been idle since March 2025.
- **Before this is final.** Hold this recommendation until a Mac and a Windows spike compare its word timings against today's mlx-audio output on the existing reference clips. See "What could not be verified".

## What VoiceForge needs from word timings (current code)

- **How words are requested.** `ml.transcribe(..., words=True)` calls mlx-audio Whisper (`mlx-community/whisper-large-v3-turbo`) with `word_timestamps=True`. It keeps `{word, start, end, p}` for each word ([`backend/app/ml.py`](../backend/app/ml.py), [`backend/app/config.py`](../backend/app/config.py) `STT_REPO`).
- **Reference clip cutting.** `reference._utterances` splits clips at gaps `w.start - prev.end >= 0.35 s`, or `>= 0.12 s` after a sentence end ([`backend/app/reference.py`](../backend/app/reference.py)). **The gaps between words are therefore load-bearing.**
- **Subtitles.** `tools.py` groups words into captions and maps script words onto heard word times ([`backend/app/tools.py`](../backend/app/tools.py)).
- **Where the gaps come from.** mlx-audio builds word times the way OpenAI's `timing.py` does. Raw DTW word times are back to back: `start_times = jump_times[word_boundaries[:-1]]` and `end_times = jump_times[word_boundaries[1:]]`. Gaps only appear after post-processing:
  - `merge_punctuations` folds punctuation tokens, along with the silence they absorbed, into the neighbouring word's text but not its times.
  - Words at sentence and segment boundaries longer than twice the median word length (median capped at 0.7 s) are clamped.

  Source: [`timing.py` `find_alignment` / `add_word_timestamps`](https://github.com/Blaizzy/mlx-audio/blob/93cfb9ff82c10e610ce65570b1c2efc1f0f9411d/mlx_audio/stt/models/whisper/timing.py). This is a reading of the code; it has not been tested.
- **What this means for Rust.** Any route that returns only *token* times must port this grouping and clamping, or the 0.35 s pause rule will behave differently. This applies to whisper.cpp and sherpa-onnx. ct2rs and rwhisper do their own word grouping, which may differ from mlx-audio's.

## Comparison

| | whisper.cpp (own binding, or whisper-rs) | ct2rs (CTranslate2) | Candle | sherpa-onnx | rwhisper (floneum/kalosm) |
|---|---|---|---|---|---|
| Word timings | DTW token times (`t_dtw_start`, `t_dtw`); separate older heuristic `t0/t1` | Words from DTW `align()` | None (segment timestamps only) | DTW token times; needs attention-export model | DTW word timings |
| Mac GPU | Metal, plus optional Core ML encoder | None (Accelerate CPU) | Metal | CPU or Core ML (not checked) | Metal via wgpu |
| Windows NVIDIA | CUDA build (`GGML_CUDA=1`) | CUDA | CUDA (Windows not checked) | CUDA provider (not checked) | DX12 via wgpu; no CUDA |
| turbo weights | `ggml-large-v3-turbo.bin` 1,625 MB, q8_0 874 MB, q5_0 574 MB | Community CT2 conversions | `openai/whisper-large-v3-turbo` safetensors | 2024 export without attention outputs | `Demonthos/...turbo` GGUF |
| Licence | MIT (binding: own; whisper-rs is Unlicense) | MIT (wraps CTranslate2) | Apache-2.0 / MIT | Apache-2.0 | MIT / Apache-2.0 |
| Maintenance | Very active; v1.9.6 on 2026-11-23 | Active; 0.11.3 on 2026-11-23 | Active; 0.11.1 on 2026-10-31 | Active; 2.0.3 on 2026-11-22 | Repo active; crate 0.4.1 from 2025-03 |

## whisper.cpp (and `whisper-rs`)

### Word timings: two different methods

1. **Heuristic `t0/t1`** (`token_timestamps`, `thold_pt`, `thold_ptsum`). It uses timestamp-token probabilities and signal energy. The README's "Word-level timestamp (experimental)" mode `-ml 1` is built on this: `whisper_wrap_segment` splits segments using `token.t0`/`segment.t1`, not DTW ([README](https://github.com/ggml-org/whisper.cpp/blob/79b11911d9d38c4be2fc7dc8b4efeff3324c090f/README.md#word-level-timestamp-experimental), [`whisper.cpp` `whisper_exp_compute_token_level_timestamps` / `whisper_wrap_segment`](https://github.com/ggml-org/whisper.cpp/blob/79b11911d9d38c4be2fc7dc8b4efeff3324c090f/src/whisper.cpp)). This is **not** what VoiceForge uses today, so do not use `-ml 1` output.
2. **DTW over the cross-attention alignment heads** (`dtw_token_timestamps`, marked `[EXPERIMENTAL]` in [`whisper.h`](https://github.com/ggml-org/whisper.cpp/blob/79b11911d9d38c4be2fc7dc8b4efeff3324c090f/include/whisper.h)). This is the same idea as OpenAI and mlx-audio. Points to know:
   - **Turbo preset.** `WHISPER_AHEADS_LARGE_V3_TURBO` exists, added in [PR #2481](https://github.com/ggml-org/whisper.cpp/pull/2481) in October 2024. Its heads `{2,4},{2,11},{3,3},{3,6},{3,11},{3,14}` match the `alignment_heads` in OpenAI's [turbo `generation_config.json`](https://huggingface.co/openai/whisper-large-v3-turbo/blob/main/generation_config.json).
   - **`t_dtw` is the token's end, not its start.** [PR #4170](https://github.com/ggml-org/whisper.cpp/pull/4170), merged 2026-11-25, found this. It added `t_dtw_start` and the `whisper_full_get_token_dtw_t0/t1` accessors, which also map times back correctly when VAD is on. On `jfk.wav` with the **tiny** model, the PR reports a median difference from faster-whisper of **10 ms** with the corrected reading, against 340 ms with the old reading. **No figure exists for turbo.**
   - **Release status.** PR #4170 landed two days after v1.9.6 (2026-11-23, from [releases](https://github.com/ggml-org/whisper.cpp/releases/tag/v1.9.6)), so it is on `master` only for now.
   - **Flash attention must be off.** `whisper_init_with_params_no_state` disables DTW when `flash_attn` is true, and `flash_attn` defaults to `true` ([`src/whisper.cpp`](https://github.com/ggml-org/whisper.cpp/blob/79b11911d9d38c4be2fc7dc8b4efeff3324c090f/src/whisper.cpp)). The PR also notes that DTW needs `-nfa`. Turning flash attention off costs speed, but the size of that cost was not measured.
   - **Open DTW issues.** [#2148](https://github.com/ggml-org/whisper.cpp/issues/2148) ("-dtw doesn't work as intended") is still open, and [#4046](https://github.com/ggml-org/whisper.cpp/issues/4046) (VAD timeline) is open as well. #4170 says it addresses #4046.
3. **What is still ours to build.** DTW gives *token* times. Grouping tokens into words, merging punctuation and clamping long words (see above) must be written in Rust.

### Platforms

- **Mac.** "Apple Silicon first-class citizen", and inference "runs fully on the GPU via Metal". An optional Core ML or ANE encoder is available ([README](https://github.com/ggml-org/whisper.cpp/blob/79b11911d9d38c4be2fc7dc8b4efeff3324c090f/README.md)).
- **Windows.** MSVC and MinGW builds are supported. CUDA builds use `cmake -B build -DGGML_CUDA=1` with "cuBLAS and custom CUDA kernels" (README).
- **CI coverage.** The public Windows workflow ([`build-windows.yml`](https://github.com/ggml-org/whisper.cpp/blob/79b11911d9d38c4be2fc7dc8b4efeff3324c090f/.github/workflows/build-windows.yml)) builds CPU and OpenBLAS variants. No CUDA step showed up in that file or in `release.yml`. Whether Windows CUDA is covered elsewhere (for example `build-self-hosted.yml`) was **not checked**.

### Models

- **Files.** [`ggerganov/whisper.cpp`](https://huggingface.co/ggerganov/whisper.cpp) (MIT) has `ggml-large-v3-turbo.bin` (1,625 MB), `-q8_0` (874 MB) and `-q5_0` (574 MB), plus a Core ML encoder zip (1,173 MB). Sizes come from the HF API.
- **Original weights.** Upstream is [`openai/whisper-large-v3-turbo`](https://huggingface.co/openai/whisper-large-v3-turbo) (MIT): 32 encoder layers and 4 decoder layers per its `config.json`, the same shape as today's `mlx-community/whisper-large-v3-turbo`.

### Licence and maintenance

- whisper.cpp is MIT. It has 55.5k stars and was pushed 2026-11-26.

### `whisper-rs` specifically

- **Where it lives.** The GitHub repo [`tazz4843/whisper-rs`](https://github.com/tazz4843/whisper-rs) is **archived**. crates.io now points to [codeberg.org/tazz4843/whisper-rs](https://codeberg.org/tazz4843/whisper-rs). The licence is Unlicense.
- **Releases.** 0.16.0 was released 2026-03-12, and the last commit was 2026-03-14. Its `sys/whisper.cpp` submodule is `2eeeba5` = **v1.8.3** (2026-01-15).
- **Features.** It has `metal`, `cuda`, `coreml`, `vulkan` and others (from the [crates.io](https://crates.io/crates/whisper-rs) version features).
- **DTW support.** `WhisperContextParameters` exposes `flash_attn` (documented "Can't be used with DTW") and `dtw_parameters`, including a `DtwModelPreset::LargeV3Turbo`. `WhisperToken::token_data()` returns the raw `whisper_token_data`, so `t_dtw` is readable. `t_dtw_start` will not be there until the submodule is updated ([`src/whisper_ctx.rs`](https://codeberg.org/tazz4843/whisper-rs/src/branch/master/src/whisper_ctx.rs), [`src/whisper_state/token.rs`](https://codeberg.org/tazz4843/whisper-rs/src/branch/master/src/whisper_state/token.rs)).
- **Windows build.** CUDA builds with MSVC need Visual Studio C++, CMake, LLVM/clang (`LIBCLANG_PATH`) and the CUDA toolkit. The doc says this "has been reported" to work ([`BUILDING.md`](https://codeberg.org/tazz4843/whisper-rs/src/branch/master/BUILDING.md)).

## ct2rs (CTranslate2 bindings)

- **Word timings.** `Whisper::generate_segments` returns `Segment { start, end, words: Option<Vec<Word>> }`. It runs "DTW alignments on the encoder output cross-attention weights" through CTranslate2's `align()`, which is the faster-whisper approach. This was added 2026-07-31 ([`ct2rs/src/whisper.rs`](https://github.com/jkawamoto/ctranslate2-rs/blob/f80ff848aafa1276588d24cb2f8b4fc5a6a452d3/ct2rs/src/whisper.rs)). Whether its word grouping and clamping match mlx-audio's was not checked.
- **Platforms.** Recommended features are `cuda`, `cudnn` and `cuda-dynamic-loading` on Windows, and **`accelerate` and `ruy` on Apple Silicon (CPU only)** ([README](https://github.com/jkawamoto/ctranslate2-rs/blob/f80ff848aafa1276588d24cb2f8b4fc5a6a452d3/README.md)). There is no Metal path.
- **Models.** CTranslate2 format. Community conversions exist, such as `dropbox-dash/faster-whisper-large-v3-turbo` and `deepdml/faster-whisper-large-v3-turbo-ct2` (HF search); otherwise convert at build time.
- **Licence.** MIT, 0.11.3 released 2026-11-23.

## Candle

- **Timestamps.** The [Whisper example](https://github.com/huggingface/candle/blob/9c1fcffaf956fcfd629fdddc9fc25adefb95c052/candle-examples/examples/whisper/main.rs) supports `LargeV3Turbo` (`openai/whisper-large-v3-turbo`) and segment timestamps through timestamp-token rules. **Word timings are not supported.**
- **The DTW attempt.** [Issue #3013](https://github.com/huggingface/candle/issues/3013) ("Word Timestamp for whisper") is open. The DTW implementation in [PR #2728](https://github.com/huggingface/candle/pull/2728) was **closed without merging**.
- **Why it is hard.** `MultiHeadAttention::qkv_attention` computes the softmax weights and returns only `wv` ([`model.rs`](https://github.com/huggingface/candle/blob/9c1fcffaf956fcfd629fdddc9fc25adefb95c052/candle-transformers/src/models/whisper/model.rs)). Getting word timings means forking the model to return the cross-attention weights, then writing DTW and word grouping ourselves.
- **Quantized weights.** The example's default quantized repo, [`lmz/candle-whisper`](https://huggingface.co/lmz/candle-whisper), has only tiny models.
- **Platforms.** It has `metal` and `cuda` features ([`candle-examples/Cargo.toml`](https://github.com/huggingface/candle/blob/9c1fcffaf956fcfd629fdddc9fc25adefb95c052/candle-examples/Cargo.toml)). Windows CUDA status was not checked.
- **Licence.** Apache-2.0 / MIT, crate 0.11.1.

## sherpa-onnx

- **Word timings.** `OfflineWhisperModelConfig.enable_token_timestamps` uses "cross-attention weights and DTW". This "requires ONNX models exported with attention outputs" ([`offline-whisper-model-config.h`](https://github.com/k2-fsa/sherpa-onnx/blob/696ce07efd1cd67cfd7b7fed57f2e34d56568942/sherpa-onnx/csrc/offline-whisper-model-config.h)). It was added in [PR #2945](https://github.com/k2-fsa/sherpa-onnx/pull/2945) (2026-02-05).
- **Rust API.** The flag is exposed ([`rust-api-examples/examples/whisper.rs`](https://github.com/k2-fsa/sherpa-onnx/blob/696ce07efd1cd67cfd7b7fed57f2e34d56568942/rust-api-examples/examples/whisper.rs)).
- **Turbo model.** The published turbo model [`csukuangfj/sherpa-onnx-whisper-turbo`](https://huggingface.co/csukuangfj/sherpa-onnx-whisper-turbo) dates from 2024-10-02, before the attention export existed. It would have to be re-exported with [`export-onnx-with-attention.py`](https://github.com/k2-fsa/sherpa-onnx/blob/696ce07efd1cd67cfd7b7fed57f2e34d56568942/scripts/whisper/export-onnx-with-attention.py), which lists `turbo`. That uses Python at build time, not at runtime.
- **Not checked.** Which GPU provider it uses on Mac (Core ML or CPU) and on Windows (CUDA).
- **Licence.** Apache-2.0, crate 2.0.3.

## rwhisper (floneum / kalosm)

- **Word timings.** `.timestamped()` gives per-chunk word timings from `dtw_timestamps` over alignment heads. The turbo preset uses the same six heads ([`models/rwhisper/src`](https://github.com/floneum/floneum/tree/7a9ead2493b31ef670a35ed0507de1188006329e/models/rwhisper/src)). The released 0.4.1 source also contains DTW ([docs.rs source](https://docs.rs/crate/rwhisper/0.4.1/source/src/model.rs)).
- **Weights.** [`Demonthos/candle-quantized-whisper-large-v3-turbo`](https://huggingface.co/Demonthos/candle-quantized-whisper-large-v3-turbo) (`model.gguf`, 2024-11, no licence in its card metadata).
- **Platforms.** On `main` it runs on its own `fusor` framework over **wgpu** (Metal on Mac, DX12 on Windows), not CUDA ([`fusor-gpu/Cargo.toml`](https://github.com/floneum/floneum/blob/7a9ead2493b31ef670a35ed0507de1188006329e/fusor/fusor-gpu/Cargo.toml)). The released 0.4.1 on crates.io uses Candle.
- **Maintenance.** The crate has not had a release since 2025-03-06. MIT / Apache-2.0.

## What could not be verified

- **Accuracy on turbo.** No source reports word timing error for large-v3-turbo on any route. The only number found, from whisper.cpp PR #4170, is a 10 ms median on `jfk.wav` with the tiny model. Agreement with today's mlx-audio timings is unknown. A spike should compare word starts and ends, and the 0.35 s pause cuts, on the existing reference clips.
- **Speed.** No primary turbo benchmarks were found for any route on M-series Metal or Windows CUDA, including with flash attention off (required for DTW in whisper.cpp). whisper.cpp keeps a community thread, [#89 "Benchmark results"](https://github.com/ggml-org/whisper.cpp/issues/89), but it was not read for turbo numbers.
- **Windows CUDA in whisper.cpp CI.** Not found in `build-windows.yml` or `release.yml`; other workflows were not checked. A Windows CUDA build has not been tried.
- **Others.** The GPU providers for sherpa-onnx on Mac and Windows, and Candle's Windows CUDA status, were not checked.
- **Whether DTW times match mlx-audio.** Whether whisper.cpp's DTW (median filter width, normalisation, `dtw_n_top`) matches mlx-audio's `find_alignment` closely enough to keep the 0.35 s / 0.12 s pause thresholds was not checked.
