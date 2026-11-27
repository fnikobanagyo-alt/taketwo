# TakeTwo Rust port: progress log

The loop appends one entry per session at the bottom: date, step id, what was built, the commands run with their real results, what is not done, questions for the lead. Never edit earlier entries.

## Step status

| Step | Status |
|---|---|
| S01 Workspace, safety rails, tools | done |
| S02 Windows CUDA link proof | done (G6 early evidence) |
| S03 Text logic | todo |
| S04 Library store, seeder, one-time copy | todo |
| S05 Captions, timings, subtitles, review rules | todo |
| S06 Engine: read side, guards, UI, contract runner | todo |
| S07 Engine: write side, worker, stub voice, lifecycle | todo |
| S08 MCP server | todo |
| S09 Tauri shell | todo |
| S10 Audio DSP with Python parity vectors | todo |
| S11 Delivery, ending guard, check decisions | todo |
| S12 Qwen voice (real model, Metal) | todo |
| S13 Whisper with word timings | todo |
| S14 Speaker encoder | todo |
| S15 Render pipeline A | todo |
| S16 Render pipeline B | todo |
| S17 Voice creation | todo |
| S18 Studio restoration (Sidon) | todo |
| S19 Real synthesis smoke | todo |
| S20 Blind A/B harness | todo |
| S21 Final gate run | todo |

## Gate status

| Gate | Status | Evidence |
|---|---|---|
| G1 | pass (start) | S01 entry below: `cargo test --workspace` exit 0, 143 run, 0 failed |
| G2 | not run | |
| G3 | not run | |
| G4 | not run | |
| G5 | not run | |
| G6 | early evidence (judge again on the final commit) | S02 entry below: run 45921171833 on `e28e7ba`, `build (cuda)` success, `g6.sh` exit 0 |
| G7 | not run | |
| G8 | pass so far (re-run at the end) | Lead decision 2 below: `g8.sh` exit 0; data/ 0 differences; App Support/VoiceForge differences attributed to Franz's running app (browser cache and storage files) |

## Baseline for G8 (recorded 2026-11-27, before any Rust was written)

- `main` and `origin/main`: `4448aaa9b241f2e52cfaa6c06fa98b89653be82e`
- `rust/taketwo` started at `1e4f195`; it forked from main at `0092114`. `git diff --stat rust/taketwo main -- backend/app mcp` was empty.
- `rust/taketwo` was not on GitHub yet.
- Franz's app on 8766: `{"ok":true,"name":"TakeTwo","formerly":"VoiceForge","version":"2.0.0"}`, idle.
- Metadata-only manifests (path, size, mtime; no file contents were read), kept git-ignored because they hold private file names:
  - `rust/.baseline/data.manifest`: 5811 files under the Python app's `data/`, sha256 `0ec7d4a51676ec29835515d01569768ea6b4d4b3441ff38f2c7243d819ca7c25`
  - `rust/.baseline/appdata-voiceforge.manifest`: 178 files under `~/Library/Application Support/VoiceForge`, sha256 `75a4f529570275a7661da9b0bfe80781052ce0676a4320ce1aeb823a7fd4637d`
- Franz's app is live and writes its own database, so G8 asks that no baseline voice or audio file changed or vanished, not that the folders are identical.

## Log

### 2026-11-27, lead, planning
- Read the worktree, the Python app, `contract/README.md`, `prototypes/README.md` and map issue #1 with its tickets.
- Wrote `rust/PLAN.md` (21 steps, 8 gate commands) and this file. No Rust written.
- Found: no cmake, no Homebrew, no `cargo tauri`; no ggml Whisper model on disk (only MLX weights); swap already high (8.9 of 10 GB); 40 GB disk free; ports 8796 and 8797 busy.
- Open for Franz, only if S13 fails: approve one download of `ggml-large-v3-turbo.bin` (1.6 GB).

### 2026-11-27, lead, iteration 1 decision
- State checked by command: branch `rust/taketwo` at `76ce9e0`, only planning commits; `rust/` holds `PLAN.md`, `PROGRESS.md` and the git-ignored `.baseline/`. No Rust workspace yet. `main` and `origin/main` still `4448aaa`; `git diff --stat rust/taketwo main -- backend/app mcp` empty; 8766 health answers `"name":"TakeTwo"`; cargo 1.100.0, uv present, cmake absent; 41 GB disk free.
- No gate has evidence yet. Not done, not blocked on Franz.
- Next step: **S01 Workspace, safety rails, tools**. Every other step needs it, and it starts G1 and gives the G8 script.
- Builder must hand back: `cd rust && cargo test --workspace` passing, `rust/scripts/g8.sh` exiting 0, one commit, a log entry here with the real output.

### 2026-11-27, builder (Opus), S01 Workspace, safety rails, tools
Built:
- Cargo workspace `rust/Cargo.toml` (resolver 3, one `rust/target`). Members: `vendor/qwen_tts`, `vendor/qwen_tts_cli`, `crates/taketwo-core`, `crates/taketwo-voice` (empty lib), `crates/taketwo-engine` and `crates/taketwo-mcp` (mains that answer `--version`, otherwise exit 2). No GPU feature is on by default.
- The fork was moved with `git mv` to `rust/vendor/`: `qwen_tts`, `qwen_tts_cli`, `LICENSE`, `cudnn_version_shim.c`, plus `README.md`, `TESTS.md`, the png, `.gitignore` and the fork's own `.github/release.yml`.
  - These extras moved because `qwen_tts/Cargo.toml` has `readme = "../README.md"` and the fork's release.yml builds the shim from the fork root. `prototypes/qwen3-tts-rs-fork/` is now gone.
  - The fork's `Cargo.toml` was removed. Its `Cargo.lock` became `rust/Cargo.lock`, so the listened-to versions are kept: a diff shows only the 4 taketwo crates added and no version moved.
  - No vendored source was changed.
- `prototypes/README.md` paths now point at `rust/`. `.github/workflows/windows-cuda-build.yml` changed only in `working-directory: rust`, the artifact path `rust/build.log` and `rust/**` in the paths filter.
- `taketwo-core::paths` holds the isolation table as code. Every function takes its base folders as parameters.
  - `Mode::resolve`: `TAKETWO_DATA` means isolated, `--live` means live, neither is refused, both together are refused, and empty counts as unset.
  - `library_dir`, `lock_file` and `address_files` all sit under the data folder when isolated. When live they are under `<app data>/TakeTwo/library`, `<app data>/TakeTwo/engine.lock` and `engine.json` under TakeTwo and VoiceForge.
  - `default_ports`: isolated uses 8790-8795, 8798 and 8799; live uses 8766-8785.
  - `check_port` refuses 8765-8785 outside live mode.
  - `publish_address` writes only in live mode and only for ports 8766-8785, ignoring write errors, as `_publish_address` does. The JSON matches Python's `json.dumps`: `{"url": "http://h:p", "pid": n, "started": <float repr>}`.
  - `EngineLock` is an OS file lock (`File::try_lock`) with the pid written inside for people to read. The OS frees it when the process dies, so a stale lock is taken over.
  - `parse_max_takes`: unset or empty means no cap. 0, negative or non-numeric values are errors.
  - `live_app_data_root()` (`dirs::data_local_dir`) is the only function that looks at the real home, and no test calls it.
- Tests:
  - 14 unit tests cover each rule: port boundaries 8764/8765/8766/8785/8786, an address file never written when isolated even when handed live paths, the exact JSON, and the second lock failing then freeing on drop.
  - `tests/stale_lock.rs` runs a real child process that takes the lock and exits while holding it, then checks the parent takes the lock over.
  - It lives in its own test binary. The first version spawned the child from the unit-test binary, and the lock test failed once in 5 runs (the lock was still held after drop).
  - Hypothesis, not proven: a fork from one test briefly shares another test's lock descriptor. Since the move, 25 repeated runs and every full run have passed.
  - S07 should keep this in mind, because the engine will spawn ffmpeg while it holds the lock.
  - The tests read the pid only after the lock is released, because Windows blocks reads of an exclusively locked file.
- cmake 4.4.5 is installed in `rust/.tools/venv` (uv, PyPI `cmake`), with only `rust/.tools/bin/cmake` symlinked so that `python3` is not shadowed.
- `rust/scripts/env.sh` (bash or zsh, no `set -e`/`exit`/`cd`) does three things:
  - Puts `rust/.tools/bin` on PATH.
  - Exports `TAKETWO_MODELS=rust/.models`, with a symlink `qwen3-tts-1.7b-base` to the converted model for read-only use.
  - Exports a fresh `mktemp -d` `TAKETWO_DATA`.
  It never sets `--live`.
- `rust/scripts/g8.sh` (read-only apart from `git fetch origin main`) works in two parts.
  - It fails on any of three conditions:
    - `4dd65c3` is on `main` or `origin/main`. Any `merge-base` exit other than 1 also fails.
    - 8766 health lacks `"name":"TakeTwo"`.
  - It then prints how main moved since `4448aaa` and the metadata differences against `rust/.baseline/*.manifest`, in the same `path|size|mtime`, C-sorted format.
    - The format was calibrated: data/ re-listed against its baseline gave 0 differing lines.
    - The default output is counts only, per top-level folder and audio/other. `--names` prints the private paths; never paste that output.

Commands and real results:
```
$ cd rust && cargo test --workspace   (real output, filtered to Running/test result lines; 143 tests run, 1 ignored = the child helper)
     Running unittests src/lib.rs (…
test result: ok. 116 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.69s
     Running tests/integration_tests.rs (…
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src/main.rs (…
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src/lib.rs (…
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/stale_lock.rs (…
test result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src/main.rs (…
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src/main.rs (…
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src/lib.rs (…
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests qwen_tts
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests taketwo_core
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests taketwo_voice
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
exit 0

$ rust/scripts/g8.sh                           # exit 0 (verbatim)
ok: Rust port is not on main (4448aaa)
ok: Rust port is not on origin/main (4448aaa)
ok: 8766 health: {"ok":true,"name":"TakeTwo","formerly":"VoiceForge","version":"2.0.0","loaded_engine":null}

--- for the lead to attribute (does not fail the gate) ---
main since baseline 4448aaa: 0 new commit(s)
origin/main since baseline 4448aaa: 0 new commit(s)
data/: 0 added, 0 removed, 0 changed (size or mtime)
App Support/VoiceForge: 7 added, 0 removed, 22 changed (size or mtime)
  added Cache/ other: 5
  added Code Cache/ other: 1
  added GraphiteDawnCache/ other: 1
  changed (top-level file) other: 1
  changed Cache/ other: 8
  changed Code Cache/ other: 1
  changed DawnGraphiteCache/ other: 1
  changed DawnWebGPUCache/ other: 1
  changed GPUCache/ other: 1
  changed GraphiteDawnCache/ other: 4
  changed Local Storage/ other: 3
  changed Session Storage/ other: 2

G8 PASS

$ source rust/scripts/env.sh && cmake --version && test -d "$TAKETWO_DATA" && test -d rust/vendor/qwen_tts && test ! -e prototypes/qwen3-tts-rs-fork/qwen_tts   # exit 0
cmake version 4.4.5

$ rust/target/debug/taketwo-engine --version  -> taketwo-engine 0.1.0 ; taketwo-mcp --version -> taketwo-mcp 0.1.0
```
No engine was started, no port was bound, no model was run, nothing was pushed.

The G8 fail path was also checked with a throwaway copy, since deleted. It used `FIRST_COMMIT=4448aaa` and health on unused port 8798. It printed `FAIL ... is on main`, `FAIL ... is on origin/main`, `FAIL: 8766 health did not name TakeTwo: no answer` and `G8 FAIL`, and exited 1.

Not done / notes:
- The vendored crates print 3 `unused dependency` warnings (`candle-transformers`, `float8`, `tracing-appender`). They were left as they are; this step does not change the vendored crate.
- `env.sh` makes a new temporary folder each time it is sourced and does not clean it up. About 4 empty `$TMPDIR/taketwo-data.*` folders from this session are left; a safety check refused their removal.
- `scripts/check.py` was not run. It runs the Python and React checks, and this step changed neither. Its `npm run build` could also write caches into the symlinked `node_modules` of the main checkout.
- The lock is untested on Windows. While it is held, the "another engine" error shows an empty pid there.
- Hindsight was unreachable, so no `retain` was made.
- cmake is 4.x. If whisper.cpp's `cmake_minimum_required` is too old for cmake 4 (S02/S13), set `CMAKE_POLICY_VERSION_MINIMUM=3.5` for that build.

Questions for the lead:
1. Isolated mode gives the address file the path `<TAKETWO_DATA>/engine.json`, but by the rule `publish_address` never writes it. Should S07/S08 write it when isolated so the MCP "start an engine" test can find a dev engine, or should discovery stay on `TAKETWO_URL` and the dev-port scan?
2. These choices were not in the plan; please confirm or change them:
   - `TAKETWO_DATA` together with `--live` is refused.
   - `TAKETWO_MAX_TAKES=0` is an error.
   - The live lock file is at `<app data>/TakeTwo/engine.lock`.
3. Safety: the plan's stop-and-clean check `pgrep -fl 'taketwo-(engine|mcp)|target/.*/taketwo'` matches 5 processes this work did not start: `.../uv/environments-v2/taketwo-mcp-<hash>/bin/python .../voiceforge/mcp/taketwo_mcp.py`. They look like Franz's own Python MCP servers. A builder told "pgrep must be empty" must not kill them. Should the pattern be tightened (for example `rust/target/.*/taketwo`)?

### 2026-11-27, lead, iteration 2 decision
State checked by command: S01 is committed, the tree is clean, `main` and `origin/main` are still `4448aaa`, and 8766 health names TakeTwo. Gates G2 to G7 have no evidence yet. Not done, not blocked on Franz.

Commit hashes changed (history of 4 unpushed commits rewritten by the lead):
- Why: `rust/PLAN.md` held the absolute home-folder path of this Mac, and the branch is about to be pushed to a public repository. The path is now `$WT` / `$MAIN` in every commit. No commit message held it.
- Old to new: `4dd65c3` is `7e0a172`, `76ce9e0` is `3ec1042`, `00d7a88` is `12f460c`, `697ddd6` (S01) is `3442a21`. Earlier entries in this log still name the old hashes.
- `g8.sh` now uses `FIRST_COMMIT=7e0a172`; re-run after the change: exit 0, G8 PASS.
- The old commits are kept locally on `backup/rust-taketwo-pre-scrub`. Never push that branch.
- Rule for every later step: no absolute home path and no private file name in any committed file.

Answers to the S01 questions:
1. Address file when isolated: yes, write `<TAKETWO_DATA>/engine.json` (S07). It is inside the temporary folder, so it is safe. In isolated mode the MCP server (S08) looks at `TAKETWO_URL` first, then only that file. It never scans ports and never reads the live address files.
2. Confirmed: `TAKETWO_DATA` together with `--live` is refused; `TAKETWO_MAX_TAKES=0` is an error; the live lock file is `<app data>/TakeTwo/engine.lock`.
3. Agreed. `PLAN.md` now says `pgrep -fl 'rust-taketwo/rust/target/'`, and that the Python `mcp/taketwo_mcp.py` servers are Franz's and are never stopped. Stop only your own processes, by pid.

G8 attribution: data/ has 0 differences. The App Support/VoiceForge differences are browser cache, code cache, local and session storage, and one browser bookkeeping file at the top level. All of them are written by Franz's running app. None is a voice or audio file.

Facts found for the next step (GitHub, read-only):
- The prototype workflow triggers only on `prototype/qwen3-tts-rust`. On that branch the plain `cuda,audio-loading` job succeeded for `qwen_tts_cli` in about 13 minutes (runs 45869352830 and 45871279301).
- flash-attn failed at "Filename too long" while cloning cutlass. A long-paths fix is in commit `35c80d8` on the prototype branch, which is not on this branch; its run had not concluded when checked.
- `check.yml` runs on pushes to `main` and on pull requests only, so pushing this branch starts no Mac check.

Next step: **S02 Windows CUDA link proof (push)**. It is the only gate that waits on GitHub, and Whisper and ONNX Runtime have never been built on the Windows runner. A green S02 is early evidence for G6, not G6 itself: the gate is judged again on the final commit.
Additions to the plan text for S02:
- `--version` in both programs must call one cheap function from each of Candle (`qwen_tts`), the Whisper binding and `ort`, so the linker cannot drop them.
- Record the exact whisper.cpp commit and show that PR #4170 is in it.
- `g6.sh` matches the plain job by exact name.
- Carry the long-paths fix over from `35c80d8`.
- First push of this branch: check `git grep -nE "/Users/[A-Za-z]" HEAD` is empty before it.

### 2026-11-27, builder (Opus), S02 Windows CUDA link proof
Built:
- `taketwo-voice` now links the three libraries:
  - Vendored `qwen_tts` by path, plus `candle-core = "0.9"` (the locked 0.9.2). Features: `cuda` turns on `qwen_tts/cuda`, `flash-attn` turns on `cuda` and `qwen_tts/flash-attn`, `metal` turns on `qwen_tts/metal`. No GPU feature is on by default.
  - whisper.cpp through a thin hand-written binding. `build.rs` builds the git submodule `rust/third_party/whisper.cpp` with the `cmake` crate: static libs; Release always; `GGML_NATIVE`, OpenMP, Metal and BLAS off; `GGML_CUDA=ON` and `CMAKE_CUDA_ARCHITECTURES=$CUDA_COMPUTE_CAP` (default 86) under `cuda`. On MSVC it uses the static CRT. Only `whisper_version` and `whisper_print_system_info` are declared for now; S13 adds the DTW API (`dtw_token_timestamps` is in `whisper.h` at this commit). Whisper stays on CPU on the Mac (no Metal) until S13 decides.
  - `ort = "=2.0.0-rc.15"` (ONNX Runtime 1.31, CPU provider), `default-features = false` with `std, download-binaries, copy-dylibs, tls-native, api-31`.
  - `link_report()` makes one real call into each: a 2-value CPU tensor from `qwen_tts::nn::mt_rng::MtBoxMullerRng`, plus Candle's cuda/metal availability; `whisper_version()` and `whisper_print_system_info()`; `ort::info()` (`OrtGetBuildInfoString`, a C API call). The whisper.cpp commit is stamped in at build time. One test checks that all three are named.
- `taketwo-engine` and `taketwo-mcp` print `taketwo-engine 0.1.0` / `taketwo-mcp 0.1.0`, then the report, and exit 0. Each has `cuda` and `flash-attn` features that forward to `taketwo-voice`.
- `Cargo.lock`: 17 packages added (`ort`, `ort-sys`, `cmake`, `ndarray`, `ureq` 3 and others). No existing version moved. The only removed lines are two `"ureq",` entries that became `"ureq 2.x"` because a second ureq now exists. No vendored source changed.
- `.github/workflows/windows-cuda.yml` replaces `windows-cuda-build.yml`.
  - Triggers: push to `rust/taketwo` with paths `rust/**`, `!rust/**/*.md`, `.gitmodules` and the workflow file, plus `workflow_dispatch`. `contents: read`.
  - Job `build (${{ matrix.features }})`: windows-2022, 150 minutes, fail-fast false, matrix `cuda` and `cuda,flash-attn`. Setup is the prototype's: Jimver 0.2.21, CUDA 12.6.0, network, same sub-packages, `CUDA_COMPUTE_CAP=86`, msvc-dev-cmd, pwsh. Added: the long-paths step from `35c80d8`, `submodules: true`, `CMAKE_GENERATOR=Ninja` and the ONNX Runtime step below. The build log is uploaded.
  - Job `check`: `cargo check --workspace`, then a no-GPU `cargo build --release` of both programs and both `--version` calls. It links and runs in about 9 minutes, so a link break shows long before the CUDA job ends.
- `rust/.cargo/config.toml`: `+crt-static` for `x86_64-pc-windows-msvc` only. `CMAKE_POLICY_VERSION_MINIMUM` was not needed: whisper.cpp built with cmake 4.4.5 on the Mac.
- `rust/scripts/g6.sh` (read-only):
  - It takes the newest commit on `rust/taketwo` that changed anything other than `**/*.md`.
  - It finds the run by workflow name `Windows CUDA build`, not by file. `gh run list --workflow windows-cuda.yml` returns 404 while the file is not on the default branch.
  - A run counts when its head is the target commit, or a descendant that differs from it only in `.md` files. GitHub runs a push on its tip commit, and that tip can be a later PROGRESS.md commit.
  - It judges only the job named exactly `build (cuda)`: success gives 0; queued, running or no run gives 75; anything else gives 1, as does a finished run with no such job. flash-attn and check are printed without judging. A `gh` failure exits non-zero, never 75.

whisper.cpp commit and the PR #4170 evidence:
- Submodule `rust/third_party/whisper.cpp` = `868e55578ea5f69550c046da22aefe11dc6e04c0`. `gh pr view 4170 -R ggml-org/whisper.cpp` gives `"mergeCommit":{"oid":"868e55578ea5f69550c046da22aefe11dc6e04c0"}`, `"mergedAt":"2026-11-25T17:23:28Z"`, `"state":"MERGED"`. The pinned commit is the merge commit itself.
- Both runs print `whisper.cpp 1.9.6-dev (868e55578ea5)` in `--version`, the Mac and the Windows check job, so that commit is the one that got built. The version string alone would not prove it: v1.9.6 was tagged 2026-11-23, before the merge.
- whisper-rs was rejected on dates. crates.io `whisper-rs-sys` 0.15.0 was last updated 2026-03-12. whisper-rs git HEAD `129b982` (2026-03-14) pins whisper.cpp `2eeeba5`. Both are months before #4170.

ONNX Runtime strategy (fallback taken, not silent):
- Mac: `download-binaries`, pyke's static build, linked into the exe (ORT 1.31.0, commit `402979a`). No dylib.
- Windows: pyke's static Windows build is compiled `/MD`. The C++ in `esaxx-rs` (through `tokenizers`; its `build.rs` hard-codes `static_crt(true)`) and nvcc's kernels (candle's moe, flash-attn) are `/MT`. One exe cannot hold both.
- Windows now uses one static CRT (`+crt-static`) and links ONNX Runtime 1.31.1 as a DLL through its import library.
  - The DLL comes from Microsoft's NuGet package `Microsoft.ML.OnnxRuntime` 1.31.1, with its SHA-512 pinned from the nuget.org catalog. The `microsoft/onnxruntime` GitHub repository answered 404 here.
  - The workflow sets `ORT_LIB_PATH` and `ORT_PREFER_DYNAMIC_LINK=1`, and ort-sys copies `onnxruntime.dll` next to the exe.
  - This is not `load-dynamic`: the exe imports the DLL, so the link proof holds. Windows build type: `RelWithDebInfo`, commit `41d4d61`.

Runs (GitHub, `fnikobanagyo-alt/taketwo`; 3 of the 5 push rounds used):
| Run | Commit | build (cuda) | build (cuda,flash-attn) | check |
|---|---|---|---|---|
| 45913372450 | `818a45c` | failure, 37 min: LNK2038, whisper.cpp `/MD` against esaxx and moe `/MT` | failure, 72 min: same LNK2038, plus flash-attn's `/MT` objects. The long-paths fix worked: cutlass checked out and the crate compiled | success, 3 min (`cargo check` only) |
| 45917033507 | `4af22ad` (whisper.cpp `/MT`) | failure, 36 min: LNK2038/LNK2005, ORT's `/MD` objects (`msvcprt`) against `libcpmt` | still in progress when last read | success, 3 min |
| 45921171833 | `e28e7ba` (static CRT, ORT DLL) | **success, 35 min** (cargo `Finished` in 29m 43s) | in progress when this entry was written; not waited for | success, 9 min, both programs built and ran |

- On the runner, the CUDA build's `taketwo-engine.exe --version` exited -1073741515 (`STATUS_DLL_NOT_FOUND`, very likely the missing NVIDIA driver DLL). The step listed the two programs instead: `taketwo-engine.exe 246426112`, `taketwo-mcp.exe 246426112` bytes.
- The no-GPU Windows build in `check` ran both programs. Real output, from `taketwo-engine`; `taketwo-mcp` printed the same:
```
taketwo-engine 0.1.0
candle (qwen_tts): ok, cpu tensor [2]; cuda available: false; metal available: false
whisper.cpp 1.9.6-dev (868e55578ea5): WHISPER : VITISAI = 0 | COREML = 0 | OPENVINO = 0 | CPU : SSE3 = 1 | SSSE3 = 1 | AVX = 1 | AVX2 = 1 | F16C = 1 | FMA = 1 | BMI2 = 1 | REPACK = 1 |
onnx runtime: ORT Build Info: git-branch=HEAD, git-commit-id=41d4d61, fp8-kv-cache=1, build type=RelWithDebInfo
```

Acceptance commands (Mac, real results):
```
$ cd rust && source scripts/env.sh && cargo test --workspace     # exit 0
  116 + 14 + 1 (1 ignored = the child helper) + 1 (taketwo-voice link_report) + 12 doc-tests passed, 0 failed
$ cargo build -p taketwo-engine -p taketwo-mcp && target/debug/taketwo-engine --version && target/debug/taketwo-mcp --version   # exit 0
taketwo-engine 0.1.0
candle (qwen_tts): ok, cpu tensor [2]; cuda available: false; metal available: false
whisper.cpp 1.9.6-dev (868e55578ea5): WHISPER : VITISAI = 0 | COREML = 0 | OPENVINO = 0 | CPU : NEON = 1 | ARM_FMA = 1 | FP16_VA = 1 | DOTPROD = 1 | ACCELERATE = 1 | REPACK = 1 |
onnx runtime: ORT Build Info: git-branch=rel-1.31.0, git-commit-id=402979a, fp8-kv-cache=1, build type=Release
taketwo-mcp 0.1.0   (then the same three lines)
$ cargo tree -p taketwo-engine -p taketwo-mcp --features cuda -e features > /dev/null    # exit 0
$ test -f .github/workflows/windows-cuda.yml && test ! -e .github/workflows/windows-cuda-build.yml && git grep /Users/ count 0   # exit 0
$ rust/scripts/g6.sh      # exit 0
target: e28e7ba S02: one static C runtime on Windows, ONNX Runtime as a DLL there
run 45921171833 on e28e7ba: in_progress  https://github.com/fnikobanagyo-alt/taketwo/actions/runs/45921171833
info, not judged: build (cuda,flash-attn): in_progress  2026-11-27T01:11:54Z 0001-01-01T00:00:00Z
info, not judged: check: completed success 2026-11-27T01:11:54Z 2026-11-27T01:20:36Z
gate job: build (cuda): completed success 2026-11-27T01:11:54Z 2026-11-27T01:46:27Z
G6 PASS
$ rust/scripts/g8.sh      # exit 0, G8 PASS; main and origin/main still 4448aaa; 8766 health names TakeTwo
data/: 0 added, 0 removed, 2 changed (size or mtime)  [changed (top-level file) other: 1; changed generations/ other: 1]
App Support/VoiceForge: 8 added, 0 removed, 25 changed (size or mtime)
```
- This step never read or wrote data/, so the two data/ changes (neither of them audio) are for the lead to attribute, most likely to Franz's running app.
- No engine was started, no port was bound and no model was run. Only `rust/taketwo` was pushed, after `git grep /Users/` on HEAD, the commit messages and the branch's full diff history each found 0.

Not done / notes:
- flash-attn on Windows: unknown for the fixed commit, still running. Round 1 showed that the long-paths fix works and that flash-attn compiles; it failed only at the same CRT link.
- Whisper CUDA linked on Windows (`ggml-cuda` static plus `cudart`, `cublas`, `cublasLt`, `cuda`), so the CPU-Whisper fallback was not needed.
- The CUDA exes are 246 MB each, above the 100 MB app goal, most likely from the embedded CUDA kernels (Candle PTX plus ggml-cuda fatbin). Nothing was trimmed.
- The CUDA exe cannot start without the NVIDIA driver, not even for `--version`. Product question below.
- A Windows build now needs `ORT_LIB_PATH` and `ORT_PREFER_DYNAMIC_LINK=1` set as in the workflow; `Cargo.toml` cannot express that. A Windows install must ship `onnxruntime.dll` (17 MB) next to the exes.
- `scripts/check.py` was not run: no Python or React file changed.

Questions for the lead:
1. Should a CUDA build start and answer `--version` or `/api/health` on a PC without an NVIDIA driver (for example with a clear "needs an NVIDIA GPU" message)? That needs delay-loading `nvcuda.dll` or `cuda.lib`, or loading ggml-cuda dynamically.
2. Is NuGet acceptable as the source of the Windows ONNX Runtime DLL from now on, or should a later step build ONNX Runtime `/MT` from source?
3. Every later push step should keep the code commit as the tip, or rely on `g6.sh`'s ".md-only descendant" rule as built here.
