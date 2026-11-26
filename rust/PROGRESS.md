# TakeTwo Rust port: progress log

The loop appends one entry per session at the bottom: date, step id, what was built, the commands run with their real results, what is not done, questions for the lead. Never edit earlier entries.

## Step status

| Step | Status |
|---|---|
| S01 Workspace, safety rails, tools | done |
| S02 Windows CUDA link proof | next |
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
| G6 | not run | |
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
