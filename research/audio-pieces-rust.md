# How TakeTwo's smaller audio pieces run in Rust

Research for issue #4 (parent #1). Date: 2026-11-26. Targets: macOS on Apple Silicon and Windows x64 with NVIDIA. No Python at runtime. Native libraries (ONNX Runtime through `ort`, ffmpeg) are allowed.

Method: I read primary sources only (crate metadata from the crates.io API, upstream READMEs and source at the pinned tags, Hugging Face model cards and configs). I installed nothing, downloaded no weights and ran nothing. **Every "same results" or "needs re-tuning" judgement below is a hypothesis.** None of it has been measured or heard. Rows marked *fact* come from a cited source. Rows marked *inference* are my reading of those sources.

## Summary

| Piece | What VoiceForge does today (code) | Best Rust route | Mac arm64 / Win x64 | Licence | Re-tuning risk |
|---|---|---|---|---|---|
| Voice fingerprint | Resemble GE2E voice encoder (`ve.safetensors`) via mlx-audio 0.5.7 (`backend/app/ml.py:112-135`) | Port the small 3-layer LSTM to **`candle-nn`** (has `LSTM`) and load `ve.safetensors` directly. Port the mel front-end, trim and partial-window logic by hand | Yes / Yes (CPU is enough at 5.7 MB) | candle MIT OR Apache-2.0. Weights from ResembleAI/chatterbox, MIT | **Medium.** Stored fingerprints must be recomputed in Rust, and `min_speaker_similarity` (0.85, or derived in `delivery.py:79`) re-checked |
| Sidon restoration | ONNX Runtime, CPU provider, `fp16` predictor and vocoder (`studio.py:52-77`) | **`ort`** 2.0.0-rc.15 (wraps ONNX Runtime 1.31), CPU EP first, CoreML/CUDA optional | Yes / Yes (prebuilt binaries for both) | `ort` MIT OR Apache-2.0. Sidon-ONNX MIT | **Low** for the graph itself (same engine) |
| w2v-BERT features | `transformers.SeamlessM4TFeatureExtractor` (`studio.py:56,73`) | **Hand port** (about 100 lines): Kaldi fbank via **`kaldi-native-fbank`** (Rust) or own code on `realfft`, then CMVN (ddof=1) and x2 frame stacking. soniqo's Apache-2.0 C++ `seamless_fbank` is a ready reference | Yes / Yes (pure Rust) | MIT | **Low-medium**: must be checked numerically against the Python extractor before trusting |
| Loudness (LUFS) | `pyloudnorm` 0.2.0 `Meter(sr).integrated_loudness` (`audio.py:56-71`) | **`ebur128`** 0.1.11 (`Mode::I`) | Yes / Yes (pure Rust) | MIT | **Low**: possibly a few hundredths of a dB (different K-weighting filter design). The -20 / -18 LUFS targets and `target_lufs` should hold |
| Resampling | `scipy.signal.resample_poly` (`audio.py:44-48`); ffmpeg `-ar` on decode | **`rubato`** 5.1.0 `Fft` resampler with `process_all()` | Yes / Yes | MIT OR Apache-2.0 | **Low-medium**: different anti-alias filter from scipy's Kaiser(5.0) FIR. Only matters near Nyquist |
| Noise reduction | `noisereduce` 3.0.3, `stationary=True, prop_decrease=0.75`, non-studio path only (`reference.py:162-167`) | **Hand port** of stationary spectral gating on `realfft` (about 80 lines). **No Rust port exists.** `nnnoiseless` (RNNoise) is a different algorithm | Yes / Yes | noisereduce is MIT, so a port is fine | **Medium** if ported loosely. **High** if swapped for RNNoise. Feeds the SNR < 25 gate and Whisper |
| De-esser, high-pass, mic EQ, compressor, expander | scipy `butter(4)` + `sosfiltfilt` (zero-phase), `firwin2(1025)` + `fftconvolve` (`audio.py:51-53`, `studio.py:145-180`, `delivery.py:121-150`) | **`sci-rs`** for `butter` and `sosfiltfilt` (SciPy-mirroring, odd padding). **Hand port** `firwin2` (no crate found). Plain loops for compressor and expander. FFT convolution on `realfft` | Yes / Yes | MIT OR Apache-2.0 | **Low** if zero-phase is kept. A plain one-pass biquad (`biquad` crate) is *not* equivalent |
| Pitch tracking | `librosa.yin(fmin=60, fmax=420, frame_length=1024, hop=320, sr=16000)` (`delivery.py:26`). There is **no pyin** | **Hand port of librosa's `yin`** (centred frames, constant padding, trough threshold 0.1, parabolic interpolation). `pyin` crate (pyin-rs, a librosa port) is a reference. `pitch-detection` has a YIN detector but a different per-window API | Yes / Yes | MIT | **Low-medium**: feeds `pitch_st`, `pitch_hz` and expressiveness. Stored `pitch_hz` in profiles should be recomputed |
| Decode of uploads | `ffmpeg` subprocess to mono float WAV (`audio.py:14-20`). The UI accepts `audio/*,video/*` and records with `MediaRecorder` as `.webm` | **Keep a bundled ffmpeg binary** (call it as today, or via `ffmpeg-sidecar`). Optional pure-Rust fast path: **`symphonia`** for WAV/MP3/FLAC/AAC/Vorbis/ALAC | Yes / Yes | ffmpeg LGPL-2.1+ (GPL if GPL parts are built in). symphonia MPL-2.0 | **None** with ffmpeg (same decoder). **Gap**: symphonia 0.6.1 has **no Opus**, so browser recordings (webm/Opus) need ffmpeg or libopus |
| WAV read/write | `soundfile` (`audio.py:23-34`, writes PCM_16) | **`hound`** 3.5.1 | Yes / Yes | Apache-2.0 | None (bit-exact PCM) |
| MP3 export | `ffmpeg -b:a 192k` (`audio.py:37-41`) | **Same bundled ffmpeg** (with libmp3lame). Alternative: `mp3lame-encoder` (static LAME, LGPL-3.0) | Yes / Yes | LGPL | None for ffmpeg. Static LAME linking puts LGPL relinking duties on the app |

**Pieces with no good ready-made Rust option:** noise reduction (no `noisereduce` port), the w2v-BERT/SeamlessM4T feature extractor (no exact crate, but the parts exist), `firwin2`, librosa-exact `yin`, and Opus decoding in pure Rust (symphonia 0.6.1 ships no Opus decoder). Each is a small hand port except Opus, which should stay with ffmpeg.

---

## 1. Voice fingerprint (Chatterbox / Resemble voice encoder)

**What the code loads** (*fact*). `ml.py`'s module docstring mentions a "Qwen3 ECAPA encoder", but the code loads Resemble's voice encoder. `_voice_encoder()` imports `mlx_audio.tts.models.chatterbox.voice_encoder.VoiceEncoder` and loads `ve.safetensors` from `VOICE_ENCODER_REPO = "ResembleAI/chatterbox"` (`backend/app/ml.py:112-128`, `backend/app/config.py:37`). `speaker_embedding()` calls `embeds_from_wavs([...], sr, as_spk=True)` and L2-normalises the result (`ml.py:131-135`). The fingerprint is compared with `cosine` against `min_speaker_similarity` (`pipeline.py:227-243`, default 0.85 in `config.py:49`, per-voice value from `delivery.py:79`).

**Model shape** (*fact*, mlx-audio v0.5.7 source):
- Config: 40 mels, 16 kHz, n_fft 400, hop 160, win 400, fmin 0, fmax 8000, `mel_power 2.0`, `mel_type "amp"` (no log), 160-frame partials, final ReLU, embed size 256 ([config.py@v0.5.7](https://github.com/Blaizzy/mlx-audio/blob/v0.5.7/mlx_audio/tts/models/chatterbox/voice_encoder/config.py)).
- Network: 3-layer stacked LSTM (40 to 256), then a linear projection, ReLU and L2 norm. PyTorch LSTM keys are renamed and the `ih`/`hh` biases summed on load ([voice_encoder.py@v0.5.7](https://github.com/Blaizzy/mlx-audio/blob/v0.5.7/mlx_audio/tts/models/chatterbox/voice_encoder/voice_encoder.py)).
- Front-end: Hann-window STFT with `center=True, pad_mode="reflect"` ([dsp.py@v0.5.7 `stft`](https://github.com/Blaizzy/mlx-audio/blob/v0.5.7/mlx_audio/dsp.py)), magnitude squared, then a Slaney-norm, Slaney-scale mel filterbank ([melspec.py@v0.5.7](https://github.com/Blaizzy/mlx-audio/blob/v0.5.7/mlx_audio/tts/models/chatterbox/voice_encoder/melspec.py)).
- `embeds_from_wavs` resamples to 16 kHz with mlx-audio's own Kaiser-"best" polyphase resampler ([utils.py@v0.5.7 `resample_audio`](https://github.com/Blaizzy/mlx-audio/blob/v0.5.7/mlx_audio/utils.py)). It then trims silence with its **own RMS gate** (2048/512 frames, top_db 20; not librosa's `trim`), and uses `rate=1.3`, which gives a frame step of round(16000/1.3/160) = 77 frames, `min_coverage 0.8`. The mean of the partial embeddings is re-normalised (voice_encoder.py@v0.5.7, same file).

**Rust route** (*inference*). The network is tiny, and `candle-nn` ships an `LSTM` with `lstm()` constructor and `LSTMConfig` ([candle-nn/src/rnn.rs](https://github.com/huggingface/candle/blob/main/candle-nn/src/rnn.rs)); candle-nn 0.11.1 is MIT OR Apache-2.0 ([crates.io](https://crates.io/crates/candle-nn)). Candle is expected to read safetensors natively (not checked in its docs this session), so `ve.safetensors` (MIT, [ResembleAI/chatterbox card](https://huggingface.co/ResembleAI/chatterbox)) can be loaded without conversion. CPU is enough for a 5.7 MB model on both platforms. The alternative is a one-time export to ONNX run through `ort`. I found **no published standalone voice-encoder ONNX**. `onnx-community/chatterbox-ONNX` (MIT) ships `speech_encoder.onnx`, `conditional_decoder.onnx`, `embed_tokens.onnx` and `language_model*.onnx` ([file list](https://huggingface.co/onnx-community/chatterbox-ONNX/tree/main)). I did not inspect whether `speech_encoder.onnx` exposes the GE2E embedding.

**Re-tuning** (*hypothesis*). The risky part is the front-end, not the LSTM. The port must copy mlx-audio's trim gate, reflect padding, Slaney mel and the resampler. It must not copy upstream PyTorch Chatterbox, which differs in places (for example the trim). Even a careful port will shift cosines slightly, so **recompute every stored voice fingerprint in Rust**. Do not mix Python-made and Rust-made embeddings. Then re-check the 0.85 / `0.70 + 0.2 * similarity` thresholds on a few known-good and known-drifted takes.

## 2. Sidon restoration and the w2v-BERT feature extractor

**What the code does** (*fact*). It loads `soniqo/Sidon-ONNX` `fp16/sidon-predictor.onnx` and `fp16/sidon-vocoder.onnx` with `providers=["CPUExecutionProvider"]`. It builds features with `SeamlessM4TFeatureExtractor.from_pretrained("facebook/w2v-bert-2.0")`, runs the predictor (`input_features`), then the vocoder (`features`). Audio goes in at 16 kHz and comes out at 48 kHz, in 20 s chunks with 1 s crossfades (`studio.py:16-110`).

**Model facts** (*fact*, [Sidon-ONNX card](https://huggingface.co/soniqo/Sidon-ONNX) and [config.json](https://huggingface.co/soniqo/Sidon-ONNX/blob/main/config.json)):
- Licence MIT. "All components are MIT-licensed".
- Inputs are `input_features [1, T, 160]` from the w2v-BERT 2.0 SeamlessM4T extractor at 16 kHz, with a dynamic time axis.
- fp16 is "near-lossless (waveform cos 0.999)" against fp32.
- The card names [soniqo/speech-core](https://github.com/soniqo/speech-core) (Apache-2.0, C++17, ONNX backend) as the consuming engine that handles front-end and chunking.

**Runtime: `ort`** (*fact*). `ort` 2.0.0-rc.15 is MIT OR Apache-2.0 and wraps ONNX Runtime 1.31 ([crates.io](https://crates.io/crates/ort), [README](https://github.com/pykeio/ort)).
- Prebuilt binaries exist for windows-x64 and macos-arm64. Windows prebuilts "require a recent version of Windows 11 and Visual Studio 2022 (≥ 17.14)", and macOS needs ≥ 13.4 ([docs/core/platform.tsx](https://github.com/pykeio/ort/blob/main/docs/core/platform.tsx)).
- The CUDA EP has Windows x64 binaries, needs CUDA ≥ 13.2 and cuDNN ≥ 9.23 on `PATH`, and covers sm_75 to sm_120 (RTX 20xx to 50xx). The CoreML EP has macOS arm64 binaries. DirectML is Windows-only ([docs/core/ep.tsx](https://github.com/pykeio/ort/blob/main/docs/core/ep.tsx), [execution-providers.mdx](https://github.com/pykeio/ort/blob/main/docs/content/perf/execution-providers.mdx)).
- `load-dynamic` lets the app ship and locate the ONNX Runtime dylib/DLL at runtime ([linking.mdx](https://github.com/pykeio/ort/blob/main/docs/content/setup/linking.mdx)).

*Inference:* use the CPU EP as the parity baseline on both platforms, because that is what VoiceForge runs today and both use the same ONNX Runtime engine (1.30 in Python versus 1.31 in `ort`, per `backend/requirements.txt`). Treat CoreML and CUDA as speed options that each need a listen. The "Windows 11 only" prebuilt note matters for anyone still on Windows 10. Building ONNX Runtime yourself is the escape hatch.

**Feature extractor recipe** (*fact*):
- Config: `feature_size 80`, `num_mel_bins 80`, `stride 2`, `sampling_rate 16000`, `padding_value 1` ([preprocessor_config.json](https://huggingface.co/facebook/w2v-bert-2.0/blob/main/preprocessor_config.json)).
- Per the [transformers source at v5.17.0, the pinned version](https://github.com/huggingface/transformers/blob/v5.17.0/src/transformers/models/seamless_m4t/feature_extraction_seamless_m4t.py): scale by 2^15, 400-sample Povey window, hop 160, FFT 512, power spectrum, `center=False`, pre-emphasis 0.97, DC removal.
- Then an 80-bin Kaldi-scale mel bank (20 Hz to 8 kHz, `triangularize_in_mel_space=True`, no norm), natural log with float32-eps floor.
- Then per-mel-bin mean/variance normalisation with **ddof=1**, drop the odd last frame, and reshape [T, 80] to [T/2, 160].
- soniqo's C++ `seamless_log_mel` re-implements exactly this pipeline and documents each step ([seamless_fbank.h](https://github.com/soniqo/speech-core/blob/main/include/speech_core/audio/seamless_fbank.h), Apache-2.0).

**Rust route** (*inference*). No crate does the whole extractor. The `kaldi-native-fbank` crate (0.1.0, MIT) is a "Rust port of kaldi-native-fbank" with FBANK options; its README turns `dither` off explicitly ([README](https://github.com/RustedBytes/kaldi-native-fbank)). With dither 0 and default Kaldi options (Povey window, pre-emphasis 0.97, DC removal, snip edges, low_freq 20) it should match steps 1-4. CMVN and stacking are about 20 lines on top. Writing the fbank directly on `realfft` (MIT, [crates.io](https://crates.io/crates/realfft)) with speech-core's C++ as a line-by-line reference is equally small and removes a young dependency.

**Re-tuning** (*hypothesis*). Low, provided a golden-file test compares Rust features against the Python extractor on a few clips (max absolute difference near 1e-4). Note that VoiceForge pads 0.5 s of zeros before extraction (`studio.py:73`). CMVN statistics include that padding, so the port must pad the same way.

## 3. Loudness (EBU R128 / LUFS)

*Fact:*
- VoiceForge uses `pyln.Meter(sr).integrated_loudness` (pyloudnorm 0.2.0), returns -inf under 0.5 s, then scales to the target and soft-limits (`audio.py:56-71`).
- pyloudnorm's default K-weighting is two RBJ-style biquads: a +4 dB high shelf at 1500 Hz, Q 1/√2, and a high-pass at 38 Hz, Q 0.5. Blocks are 400 ms with 75 % overlap, gates are -70 LUFS absolute and -10 relative ([meter.py@v0.2.0](https://github.com/csteinmetz1/pyloudnorm/blob/v0.2.0/pyloudnorm/meter.py)).
- `ebur128` 0.1.11 (MIT) is a Rust port of libebur128 that "produces the same results as the C library". It passes all EBU Tech 3341/3342 tests and "supports all samplerates by recalculation of the filter coefficients" ([README](https://github.com/sdroege/ebur128), [crates.io](https://crates.io/crates/ebur128)). Pure Rust, so both platforms.

*Hypothesis:* the two use different filter derivations, so integrated values may differ by hundredths of a dB on speech. That is below anything the -20 LUFS reference target, the -18 LUFS design target (`tools.py:269`) or `target_lufs` would notice. Check with a few clips rather than re-tune.

## 4. Resampling

*Fact:*
- VoiceForge resamples with `scipy.signal.resample_poly(up, down)` after a gcd reduction (`audio.py:44-48`). Decoding resamples inside ffmpeg (`-ar`, `audio.py:16`). The voice encoder uses mlx-audio's own Kaiser-best resampler (section 1).
- `rubato` 5.1.0 (MIT OR Apache-2.0) offers a synchronous FFT resampler for fixed ratios and says "use `Fft` with `process_all()`" for offline batch jobs. It also offers async sinc and polynomial resamplers ([README](https://github.com/HEnquist/rubato), [crates.io](https://crates.io/crates/rubato)). Pure Rust.

*Hypothesis:* the anti-alias filter differs from scipy's default Kaiser(5.0) FIR, so samples will not match bit for bit. Anything that measures energy near the top band (the de-esser's 4.5-9.5 kHz band, `lead_burst`'s 4-10 kHz band in `audio.py:385`) could move slightly. Resample-then-measure paths should get a quick A/B. If exact scipy behaviour is wanted, `resample_poly` is a short polyphase FIR (firwin with Kaiser β=5, then upfirdn) that can be hand-ported.

## 5. Noise reduction

*Fact:*
- VoiceForge calls `nr.reduce_noise(y=a, sr=sr, stationary=True, prop_decrease=0.75)`, only when studio mode is off and (`denoise` or SNR < 25) (`reference.py:162-167`).
- noisereduce 3.0.3 (MIT) defaults: `n_fft=1024`, `n_std_thresh_stationary=1.5`, `freq_mask_smooth_hz=500`, `time_mask_smooth_ms=50`, `chunk_size=600000`, `padding=30000` ([noisereduce.py@v3.0.3](https://github.com/timsainb/noisereduce/blob/v3.0.3/noisereduce/noisereduce.py)).
- When no noise clip is given, the noise statistics come from the signal itself, clipped to its first `chunk_size` samples (`clip_noise_stationary=True`). The gate takes `scipy.signal.stft(..., nperseg=win_length, noverlap=win_length - hop_length, nfft=n_fft, padded=False)` in dB, computes a per-frequency mean + 1.5·std threshold, makes a binary mask mixed by `prop_decrease`, smooths it with `fftconvolve` and applies it before the iSTFT ([stationary.py@v3.0.3](https://github.com/timsainb/noisereduce/blob/v3.0.3/noisereduce/spectralgate/stationary.py)).
- `win_length` defaults to `n_fft` (1024) and `hop_length` to `win_length // 4` (256, i.e. 75 % overlap). The mask-smoothing kernel is built by `_smoothing_filter` from the 500 Hz / 50 ms settings. Audio is processed in 600000-sample chunks with 30000 samples of padding ([base.py@v3.0.3](https://github.com/timsainb/noisereduce/blob/v3.0.3/noisereduce/spectralgate/base.py)).
- The only maintained Rust denoiser found is `nnnoiseless` 0.5.2 (BSD-3-Clause), "a rust port of the RNNoise C library … based on a recurrent neural network", which works on 480-sample frames ([README](https://github.com/jneem/nnnoiseless), [src/lib.rs](https://github.com/jneem/nnnoiseless/blob/master/src/lib.rs)).

**No good Rust option.** *Inference:* port the stationary gate on `realfft` (about 80 lines, MIT source, so porting is fine). The STFT must copy the call above (scipy's default Hann window and zero boundary extension, 1024/256 framing), the smoothing kernel and the chunking for close parity. RNNoise is a different algorithm and would change the cleaned reference, so it should not be a drop-in. *Hypothesis:* a faithful port needs only a listen. Swapping algorithms would need re-tuning of the reference cleaning path, and of everything downstream (Whisper transcript, utterance SNR, fingerprints).

## 6. De-essing, high-pass, mic EQ, compressor and expander

*Fact:*
- `highpass` is `butter(4, hz, "highpass", output="sos")` + `sosfiltfilt`, zero-phase (`audio.py:51-53`).
- `de_ess` uses a 4th-order Butterworth band-pass (4.5-9.5 kHz) through `sosfiltfilt`, 4 ms moving-RMS envelopes, ratio rules and a loudness gate (`studio.py:145-167`).
- `master_chain` is high-pass 75 Hz, de-ess, 2:1 RMS compression above -24 dBFS, then the expander (`studio.py:170-180`).
- `mic_match` builds a 1025-tap linear-phase FIR with `firwin2` from 1/3-octave gains and applies it with `fftconvolve(mode="same")` (`delivery.py:121-150`). `ltas` uses `np.fft.rfft` on 2048-sample Hann frames (`delivery.py:104-118`).
- `sci-rs` (MIT OR Apache-2.0) "mirror[s] SciPy". It provides Butterworth design to SOS and `sosfilt`/`sosfiltfilt` ([README](https://github.com/qsib-cbie/sci-rs)). Its `sosfiltfilt_dyn` uses odd extension and `sosfilt_zi` initial conditions like SciPy ([sosfiltfilt.rs](https://github.com/qsib-cbie/sci-rs/blob/main/sci-rs/src/signal/filter/sosfiltfilt.rs)).
- The repo tree has `firwin.rs` and `kaiser.rs` but **no `firwin2`** ([tree](https://github.com/qsib-cbie/sci-rs/tree/main/sci-rs/src/signal/filter/design)). The last crates.io release is 0.4.1 from 2024-11, older than `main` (last commit 2026-05) ([crates.io](https://crates.io/crates/sci-rs)).
- `biquad` 0.6.0 (MIT OR Apache-2.0) is a one-pass IIR biquad crate ([crates.io](https://crates.io/crates/biquad)).

*Inference:* use `sci-rs` (or a vendored copy of its ~200 lines) for `butter` and `sosfiltfilt`. Hand-port `firwin2` (frequency sampling: interpolate the gains onto a dense grid, inverse FFT, window; roughly 40 lines) and do FFT convolution with `realfft`. The compressor and expander are plain array maths. Do **not** substitute a single-pass biquad, because zero-phase filtering is part of how these sound. *Hypothesis:* with zero-phase kept, outputs match to float precision and need only a listen, not re-tuning.

## 7. Pitch tracking

*Fact:*
- VoiceForge uses `librosa.yin` (not pyin) at 16 kHz, `fmin=60, fmax=420, frame_length=1024, hop_length=320` (`delivery.py:18-43`). It feeds `pitch_st`, `pitch_range`, `pitch_hz` and expressiveness, used in take picking (`pipeline.py:315,474`) and stored in voice profiles (`delivery.py:174`).
- librosa 0.11.0's `yin` defaults are `trough_threshold=0.1`, `center=True`, `pad_mode="constant"`, plus parabolic interpolation of the period ([pitch.py@0.11.0](https://github.com/librosa/librosa/blob/0.11.0/librosa/core/pitch.py)).
- `pyin` 1.2.0 (MIT) is a pYIN implementation "based on librosa", written on `ndarray` ([README](https://github.com/Sytronik/pyin-rs), [crates.io](https://crates.io/crates/pyin)).
- `pitch-detection` 0.3.0 (MIT/Apache-2.0, last release 2022-06) has autocorrelation, McLeod and YIN detectors behind a per-window `get_pitch(...)` API ([repo](https://github.com/alesgenova/pitch-detection), [crates.io](https://crates.io/crates/pitch-detection)).

*Inference:* neither crate reproduces `librosa.yin`'s framing and always-returns-a-value behaviour. Hand-port it (about 60 lines: centre padding, difference function via FFT, cumulative-mean normalisation, first trough under 0.1 or the global minimum, parabolic refinement), using pyin-rs's YIN internals as a reference. *Hypothesis:* after a golden-file check the results should match closely. Recompute stored `pitch_hz` values with the Rust version rather than mixing.

## 8. Decoding uploads, WAV I/O and MP3 export

*Fact:*
- Uploads: the UI accepts `audio/*,video/*` (`frontend/src/pages/Voices.tsx:210`, `Transcribe.tsx:41`). In-app recordings come from `MediaRecorder` and are saved as `.webm` with the recorder's mime type (`Voices.tsx:288-296`). *(Inference:* Chromium-based shells such as Electron normally record WebM with Opus; not checked against Chromium docs here.)
- VoiceForge decodes everything with an ffmpeg subprocess to mono `pcm_f32le` at the working rate (`audio.py:14-20`). It writes 16-bit PCM WAV with soundfile (`audio.py:33-34`) and exports MP3 with `ffmpeg -b:a 192k` (`audio.py:37-41`).
- `symphonia` 0.6.1 (MPL-2.0, pure Rust) decodes WAV (Excellent), MP3 (Excellent), FLAC (Excellent), Vorbis (Excellent), AAC-LC (Great) and ALAC (Great). It demuxes ISO/MP4 (Great) and MKV/WebM (Good). Opus and HE-AAC are marked "-" ("in work or not started") ([README at v0.6.1](https://github.com/pdeljanov/Symphonia/blob/v0.6.1/README.md)). The 0.6.1 release has **no `opus` feature**, and `symphonia-codec-opus` does not exist on crates.io. The default features are `adpcm, flac, mkv, ogg, pcm, vorbis, wav` (+ meta, simd); `mp3`, `aac`, `alac` and `isomp4` must be enabled explicitly ([crates.io API, symphonia 0.6.1](https://crates.io/crates/symphonia/0.6.1)). It is a decoder, not an encoder.
- Opus options: `symphonia-adapter-libopus` 0.3.0 (MIT OR Apache-2.0) plugs libopus into Symphonia ([crates.io](https://crates.io/crates/symphonia-adapter-libopus)). `opus` 0.4.0 provides libopus bindings ([crates.io](https://crates.io/crates/opus)). `opus-rs` 0.1.42 (BSD-3-Clause) claims a pure-Rust Opus ([crates.io](https://crates.io/crates/opus-rs)); its maturity is untested.
- `hound` 3.5.1 (Apache-2.0) reads and writes WAV ([crates.io](https://crates.io/crates/hound)).
- MP3 encoding: `mp3lame-encoder` 0.3.0 wraps `mp3lame-sys` 0.2.0. Both are LGPL-3.0, and the -sys crate is a "static only build with bundled version" of LAME ([mp3lame-sys README](https://github.com/DoumanAsh/mp3lame-sys), [crates.io](https://crates.io/crates/mp3lame-encoder)). `shine-rs` 0.1.6 (LGPL-2.0) is a pure-Rust port of the shine encoder ([crates.io](https://crates.io/crates/shine-rs)).
- ffmpeg: "licensed under the … LGPL version 2.1 or later". Optional GPL parts make the whole build GPL ([ffmpeg.org/legal.html](https://ffmpeg.org/legal.html)). `ffmpeg-sidecar` 2.7.0 (MIT) wraps a standalone ffmpeg binary as an iterator and can auto-download one ([README](https://github.com/nathanbabcock/ffmpeg-sidecar)). Linking options are `ffmpeg-next` 9.1.0 (WTFPL) and `rsmpeg` 0.18.0 (MIT) ([crates.io](https://crates.io/crates/ffmpeg-next), [crates.io](https://crates.io/crates/rsmpeg)).

*Inference, recommended split:*
- **Decode:** ship an LGPL ffmpeg binary next to the app and call it as a separate process, exactly as today. This keeps every upload format, including webm/Opus and video files, gives identical decoded audio (no re-tuning), and keeps LGPL obligations simple (separate executable, not linked). Ship your own build rather than relying on `ffmpeg-sidecar`'s runtime auto-download, which conflicts with an offline-first app. Symphonia is a worthwhile optional fast path for WAV/MP3/FLAC/M4A, but cannot replace ffmpeg while browser recordings are Opus.
- **WAV:** `hound`.
- **MP3 export:** the same ffmpeg binary. Check that the shipped build includes an MP3 encoder (normally libmp3lame, LGPL); this was not verified here. Static LAME via `mp3lame-encoder` works but makes the Apache-2.0 app carry LGPL static-linking (relinkability) duties.

## 9. Other `audio.py` helpers

Frame levels, speech thresholds, silence reports, edge trimming, gap shortening, fades, the tanh soft limiter, clipping ratio, SNR estimate and pause concatenation (`audio.py:74-400`) are plain NumPy array maths. They need no crate, only a direct port on `Vec<f32>` (or `ndarray`), checked with golden files like everything else. *(Inference from reading the code.)*

---

## What still has to be verified (nothing here was run)

- Every parity claim above. A golden-file test per piece (Python output vs Rust output on the same clips) is the cheap way to settle it, plus a listening check of Sidon, noise reduction and mastering output before anything is called equal.
- Whether `onnx-community/chatterbox-ONNX`'s `speech_encoder.onnx` contains the GE2E voice encoder (outputs not inspected).
- `kaldi-native-fbank`'s default options (only the README was read, not the option structs).
- The exact pad length `sci-rs` uses inside `pad(Pad::Odd, …, ntaps)` against SciPy's `3 * ntaps` (the call site was read, not the `pad` helper).
- `opus-rs` quality and completeness.
- ONNX Runtime's own licence: the GitHub API returned "Not Found" for `microsoft/onnxruntime` during this session, so it is taken from the `ort` crate's description only. ONNX Runtime is widely known to be MIT, but that was not confirmed here.
- ffmpeg's default MP3 encoder in a given build depends on whether libmp3lame is compiled in. The shipped build must be checked.
