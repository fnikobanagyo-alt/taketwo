# TakeTwo in Rust

The Rust port of TakeTwo (formerly VoiceForge): one app for Mac (Metal) and Windows (NVIDIA CUDA).
The plan is `rust/PLAN.md`; what each step built and measured is `rust/PROGRESS.md`.

Three programs in one cargo workspace:

- `taketwo-engine`: the HTTP API (axum, 127.0.0.1), one GPU worker, the React UI served from a folder.
- `taketwo`: the Tauri 2 shell; it starts the engine as a sidecar and loads its address.
- `taketwo-mcp`: the MCP server, the same 12 tools as `mcp/taketwo_mcp.py`, registered as `taketwo` and `voiceforge`.

## Build

Run every command from `rust/`, with `env.sh` sourced first:

```sh
source scripts/env.sh                  # cmake from rust/.tools/bin on PATH, TAKETWO_MODELS=rust/.models,
                                       # TAKETWO_DATA = your folder or a new temporary one (isolated mode)
cargo build --workspace                # debug, stub-free, no GPU needed
cargo build --release -p taketwo-engine --features metal   # Mac with the real models
cargo test --workspace                 # stub and golden tests; no model, no GPU
```

- Tools live in `rust/.tools` (git-ignored). whisper.cpp needs cmake on its first build; `env.sh` provides it.
- ffmpeg and ffprobe must be on PATH (or set `TAKETWO_FFMPEG`). They decode uploads and write MP3.
- Models are read from `TAKETWO_MODELS` (dev: `rust/.models`, git-ignored). It holds the Qwen3-TTS 1.7B Base folder, `ggml-large-v3-turbo.bin` and `voice-encoder/ve.safetensors`. Nothing is downloaded.
- Studio restoration (Sidon, decision #4) is in. It needs `sidon/sidon-predictor.onnx` and `sidon/sidon-vocoder.onnx` (soniqo/Sidon-ONNX, fp16) in the models folder; `env.sh` links them from the Hugging Face cache. It runs on the CPU (ONNX Runtime) when a voice is made or re-analysed with studio on. Without the two files a studio request runs the plain clean-up (high-pass, noise reduction when needed, loudness), and the voice's first tip names the missing files. A narration does not need them.
- Features: `metal` (Mac), `cuda` (Windows), `flash-attn` (Windows, reported separately). `stub` is for tests only.

## Run in isolated mode

Every program refuses to start without `TAKETWO_DATA` unless it is given `--live`. These steps never use `--live`.

```sh
export TAKETWO_DATA="$PWD/target/my-library"; mkdir -p "$TAKETWO_DATA"
source scripts/env.sh
target/release/taketwo-engine serve --port 8790 --ui ../frontend/dist
```

- In isolated mode the library, lock file and address file (`engine.json`) all live under `TAKETWO_DATA`.
- The default ports are 8790-8799. Ports 8765-8785 are refused, because they belong to the running app.
- The one-time copy of the VoiceForge library is off.
- `TAKETWO_MAX_TAKES=<n>` caps the synthesis calls of one engine run. The engine prints one `take N of M` line per call on stderr.

## Gates

The verdict of each gate is its command's exit code (`WT` is the worktree root). Full table: `rust/PLAN.md`.

| Gate | Command |
|---|---|
| G1 tests | `cd "$WT"/rust && source scripts/env.sh >/dev/null && cargo test --workspace` |
| G2 API contract | `cd "$WT" && rust/scripts/contract.sh` (prints `Ran 33 tests` and `OK`) |
| G3 shell | `cd "$WT"/rust && cargo build -p taketwo && scripts/smoke_shell.sh` |
| G4 MCP | `cd "$WT"/rust && cargo build -p taketwo-mcp && cargo test -p taketwo-mcp` |
| G5 real synthesis smoke | `cd "$WT" && SMOKE_STUDIO=0 rust/scripts/synth_smoke.sh && cat rust/out/smoke/report.json` |
| G6 Windows build | `cd "$WT" && rust/scripts/g6.sh` (exit 75: still running) |
| G7 blind A/B harness | `cd "$WT" && python3 rust/ab/ab.py plan && python3 rust/ab/ab.py selftest && ! python3 rust/ab/ab.py render` |
| G8 isolation | `cd "$WT" && rust/scripts/g8.sh` |

How G5 works:

- It renders real audio with the release `metal` engine, at most 10 takes per attempt, and at most two attempts and 20 takes in all (`rust/out/smoke/attempts.json`).
- It exits 75 when the running app on 8766 is busy, and sends that app only GET requests.
- Its WAV stays in the git-ignored `rust/out/smoke/` for a listen.
- It makes no claim about the sound.
- Two modes:
  - Run A, `SMOKE_STUDIO=0 rust/scripts/synth_smoke.sh`: one reference clip of the running app's first ready English voice, studio off (the plain clean-up), into `rust/out/smoke/`.
  - Run B, `SMOKE_OUT=rust/out/smoke-studio SMOKE_CLIP=<wav> rust/scripts/synth_smoke.sh`: a given clip (a synthetic `say` voice of about 44 s), studio on (the default), into its own folder and ledger; it reads 8766 only for `/api/status`.
- `report.json` says `studio_requested` and whether studio ran (`studio`, `room_echo_after_db`).
- A short single-window upload with studio on (the default) ends in "Could not find a clean stretch of speech" in both the Python app and TakeTwo, which is why run A switches studio off.

## Not in TakeTwo 1.0

- **Not in the plan:**
  - code signing, installers and app bundles
  - the update flow
  - Voice Design and the Speech to Text page
  - the model catalogue, other engine families and custom engines
  - re-tuning the checks on Rust takes
- **Routes:** the routes of these features answer 501 `{"detail": "Not in TakeTwo 1.0 yet."}`.
- **The blind A/B test** (`ab.py render`) waits for Franz's OK.

## Windows

GitHub Actions builds Windows (`.github/workflows/windows-cuda.yml`, jobs `build (cuda)` and `build (cuda,flash-attn)`).
GitHub's Windows runners have no NVIDIA GPU, so the build proves compile and link only.
The Windows build has never run on an NVIDIA PC. Its sound is untested until someone runs it on one.
