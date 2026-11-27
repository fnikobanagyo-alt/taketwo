# TakeTwo Rust port: plan

Lead: Fable 5.2. Builders: Opus, one step per session. Written 2026-11-27.
The goal is met only when every gate command below passes when run, not when the code looks right.

`WT` = the `rust-taketwo` git worktree, branch `rust/taketwo` (`git rev-parse --show-toplevel` inside it). Set `WT` in the shell before pasting a gate command. The absolute path is kept out of this file because the branch is pushed to a public repository.
`MAIN` = the main checkout, the first line of `git worktree list` (read-only).

## How a builder session runs

1. Read this file and `rust/PROGRESS.md`. Take the first step that is not done and whose dependencies are done.
2. Port from the Python in `WT/backend/app` and `WT/mcp` (identical to `MAIN` on 2026-11-27; `git diff --stat rust/taketwo main -- backend/app mcp` was empty). If that diff is no longer empty, stop and tell the lead.
3. Do the step. Run its "Done when" command. Run `cd rust && cargo test --workspace` too.
4. Commit on `rust/taketwo` with a clear message ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
5. Append to `rust/PROGRESS.md`: date, step id, what was built, the commands run and their real results, what is not done, anything the lead must decide.
6. A step that fails twice goes back to the lead with the evidence. Do not weaken a test, a limit or a gate to get to green.

## Safety rules (breaking one is a failure)

- Change files only inside `WT`. Never touch `MAIN/data`, `~/Library/Application Support/VoiceForge`, or any saved voice, recording or narration.
- Never bind, start, stop or restart anything on ports 8765-8785 (Franz's running app). Dev engines use 8790-8795, 8798, 8799 (8796 and 8797 are busy). Stop every process you start.
- Never commit to or push `main`. Push only `rust/taketwo`, and only in the steps marked "push".
- No model downloads. No paid services. Tools and crates needed for the build are fine; install tools inside `WT/rust/.tools` (git-ignored).
- Before any real model run: `curl -s http://127.0.0.1:8766/api/status` must show `"running":null` and `"queued":0`, else wait or skip. One heavy model process at a time. At most 20 takes per experiment.
- A read-only `GET` of `http://127.0.0.1:8766/api/status` or `/api/health` is allowed in every step (Franz, 2026-11-27), to see that his app is idle or still answers. Other reads of 8766 only where a step names them (S19, S20). Never a write to it.
- Never write that audio is fixed, good or matching. Report measurements as measurements; only Franz's ear decides.
- Private text (Franz's narration scripts, file names from his library) never goes into a commit. It lives in git-ignored `rust/ab/private/`, `rust/out/`, `rust/.baseline/`.

### Isolation built into the programs (step S01, every later step relies on it)

| Rule | Why |
|---|---|
| `TAKETWO_DATA=<dir>` puts the engine, the MCP server and the shell in **isolated mode**: library, lock file and address file all live under that folder; default ports are 8790-8799; the one-time library copy is off. | A forgotten variable can't reach Franz's files. |
| Without `TAKETWO_DATA` the programs refuse to start unless given `--live`. The shell passes `--live` only when built with the cargo feature `live` (off by default). The loop never uses `--live`. | Running a dev build by hand can't go live. |
| The address file under the TakeTwo and VoiceForge app-data names is written only in live mode and only for a port in 8766-8785, as Python's `_publish_address` does. Its code takes the base folder as a parameter and is tested on a temp folder. In isolated mode the engine writes `<TAKETWO_DATA>/engine.json` instead (dev ports only) and removes it on a clean exit (lead decision 9). | Franz's MCP server reads the app-data files to find his app; an isolated MCP server finds the engine of its own library and no other. |
| The engine refuses ports 8765-8785 outside live mode. | Same. |
| The one-time library copy takes its source and destination as parameters, opens the source read-only, and is tested only on a synthetic source in a temp folder. | `MAIN/data` is never read or written by this work. |
| Every `taketwo-mcp` test pins `TAKETWO_URL`. The "start an engine" path is tested only in isolated mode with a dev port. | Unpinned discovery would find Franz's engine on 8766 and write to his library. |
| The shell never scans or attaches to 8766 in isolated mode; it starts its own sidecar on a dev port. | The Electron shell attaches to whatever answers. |
| `TAKETWO_MAX_TAKES=<n>` is a hard cap on synthesis calls for one engine run; over it, the narration stops with a clear message. | The 20-take limit. |

## Layout (decided, do not reopen)

```
rust/
  Cargo.toml               workspace; GPU features are opt-in so `cargo test --workspace` needs no GPU
  vendor/qwen_tts          the vendored Candle crate, moved here from prototypes/ (single copy)
  vendor/qwen_tts_cli      its CLI, kept for debugging
  crates/taketwo-core      pure logic and DSP: text, captions, review, checks, audio, library store
  crates/taketwo-voice     models behind traits: Voice (Qwen), Transcriber (whisper.cpp), SpeakerEncoder, Restorer (Sidon)
  crates/taketwo-engine    axum HTTP API, worker, pipeline; `taketwo-engine seed` is the contract seeder
  crates/taketwo-mcp       rmcp server, same 12 tools as mcp/taketwo_mcp.py
  crates/taketwo           Tauri 2 shell (engine as sidecar, WebviewUrl::External)
  scripts/                 gate runners
  golden/                  parity vectors recorded from the Python code (synthetic input only)
  ab/                      blind A/B harness
```

- Features: `metal` (Mac), `cuda` (Windows), `flash-attn` (reported separately), `stub` (stub Voice/Transcriber/SpeakerEncoder for tests; never in a release build). Test targets switch `stub` on through `[dev-dependencies]`, so the plain `cargo test --workspace` runs every stub test. `cargo test --workspace` must pass from a clean checkout: no `frontend/dist`, no prebuilt sidecar (so the shell finds its engine at run time, not through a Tauri `externalBin` entry). `taketwo-mcp` has a pass-through `cuda` feature so `--features cuda` is accepted for both programs.
- Models are read from `TAKETWO_MODELS` (dev: `rust/.models/`, git-ignored, holding symlinks to the local copies and anything converted locally). Qwen: `~/Library/Application Support/TakeTwo/models/qwen3-tts-1.7b-base`, read-only.
- The UI is served from a folder (`--ui <dir>`, the built `frontend/dist`), not embedded, so the Windows build needs no Node.
- Library documents are kept as `serde_json::Value` with key order preserved, so fields Rust does not know survive a round trip.
- Scope is 1.0: Qwen3 only. Voice Design, the Speech to Text page, the model catalogue, updates, other engine families and custom engines are not ported. Their routes answer 501 `{"detail": "Not in TakeTwo 1.0 yet."}`, except the few the UI calls at start (`/api/settings`, `/api/engines`, `/api/update`, `/api/integrations`, `/api/diagnostics`, `/api/catalog/library`), which return a minimal honest answer in today's shape.

## Things found on this Mac that shape the plan

- No `cmake`, no Homebrew, no `cargo tauri`. whisper.cpp needs cmake: install it into `rust/.tools` (for example `uv tool install --tool-dir`/a small venv, or the Kitware binary). `cargo build -p taketwo` is enough for G3; no bundle is made.
- No ggml Whisper model on disk. Only `mlx-community/whisper-large-v3-turbo` (MLX weights) and the OpenAI tokenizer files are local. Step S13 converts locally. If the conversion can't be made faithful, that is a question for Franz (a 1.6 GB download), never a silent download.
- Local and usable read-only: `ResembleAI/chatterbox` `ve.safetensors` (speaker encoder), `soniqo/Sidon-ONNX` fp16, the w2v-BERT preprocessor config.
- 24 GB memory, swap already high (8.9 of 10 GB used). The prototype peaked at 13.6 GB for Qwen alone. Load one model family at a time if needed and record peaks.
- 40 GB disk free. One shared `rust/target`. If free space drops under 10 GB, `cargo clean` the profile not in use.
- `workflow_dispatch` needs the workflow on the default branch, which we can't push. The Windows workflow triggers on `push` to `rust/taketwo`.
- `main` is at `4448aaa`; this branch forked at `0092114`. The four newer commits touch only CI and one test, not `backend/app` or `mcp`.

## Steps

Each is one builder session. "Needs" lists the steps that must be done first.

### S01 Workspace, safety rails, tools
Needs: nothing. Gates: G1 (start), G8 (script).
- Create the workspace above. `git mv prototypes/qwen3-tts-rs-fork/qwen_tts` and `qwen_tts_cli` to `rust/vendor/`, with the LICENSE and `cudnn_version_shim.c`; remove the fork's own workspace files; fix `prototypes/README.md` paths. Delete the old prototype workflow only in S02.
- `taketwo-core::paths`: the isolation table above, as code with unit tests on temp folders (isolated mode, refusal without `--live`, port refusal, address file only in live mode and range, lock file; lead decision 9 later added the isolated file under `TAKETWO_DATA`).
- Install cmake into `rust/.tools`; write `rust/scripts/env.sh` that exports `PATH`, `TAKETWO_MODELS`, and makes a fresh temp `TAKETWO_DATA`.
- `rust/scripts/g8.sh` (rewritten by lead decision 8 on Franz's instruction): proves that this work left the Python app alone. It fails only if (1) the first commit of this work (`7e0a172`) is on `main` or `origin/main` (after `git fetch origin main`) or on any branch or tag other than `rust/taketwo`, here or on GitHub; (2) a process of this work (command line or working folder inside the worktree) has a file open under `MAIN/data` or `~/Library/Application Support/VoiceForge`; (3) 8766 health does not answer `"name":"TakeTwo"`. It then prints, as information only, how `main` moved since the baseline and the metadata differences (path, size, mtime) against `rust/.baseline/*.manifest`. Franz uses his app while this work runs (new voices, narrations, the database, caches), and other sessions push to main: those differences are never a failure and need no attribution.
- Done when: `cd rust && cargo test --workspace` passes and `rust/scripts/g8.sh` passes.

### S02 Windows CUDA link proof (push)
Needs: S01. Gate: G6 (early).
- `taketwo-voice` declares its real dependencies with features: vendored `qwen_tts` (`cuda`, `flash-attn`), the whisper.cpp binding (pinned at or after PR #4170, CUDA under the same `cuda` feature), `ort`. `taketwo-engine` and `taketwo-mcp` are skeleton binaries that link them and answer `--version`.
- `.github/workflows/windows-cuda.yml`: on push to `rust/taketwo` (paths `rust/**` and the workflow, with `paths-ignore`-style exclusion of `rust/**/*.md` so a log entry starts no build), matrix `cuda` and `cuda,flash-attn` with `fail-fast: false`, reusing what the prototype workflow learned (CUDA 12.6 toolkit sub-packages, MSVC dev cmd, PowerShell, `CUDA_COMPUTE_CAP=86`), `cargo build --release -p taketwo-engine -p taketwo-mcp --features cuda`. Remove the prototype workflow.
- A second, cheap job in the same workflow: `cargo check --workspace --exclude taketwo` on Windows with no GPU feature, so later steps learn about Windows breaks in minutes.
- `rust/scripts/g6.sh`: finds the newest commit on `rust/taketwo` that changed anything other than `*.md`, finds the workflow run for that commit with `gh`, and exits 0 only if the plain `cuda` job concluded `success` (exit 75 while it is still running). It prints the flash-attn result without judging it.
- Push `rust/taketwo`. A run can take 150 minutes: poll with `g6.sh` or run `gh run watch` in the background, never in the foreground. Fix and push again until the plain `cuda` job is green. Record flash-attn separately.
- Fallback if whisper.cpp CUDA will not link on Windows: build Whisper for CPU on Windows under `cuda`, say so in PROGRESS.md and `notes`.
- Done when: `rust/scripts/g6.sh` exits 0.

### S03 Text logic
Needs: S01. Gate: G1.
- Port `textproc.py` (entities, `clean_text`, `unwrap_lines`, `split_sentences`, `_split_long`, `chunk_text`, `word_count`) and `ml.normalize_for_wer`, `ml.wer` (jiwer behaviour), `pipeline._word_coverage`, `_final_word_missing`, `_first_word_missing`.
- Tests read `contract/golden/clean_text.json` (18 cases) and `chunk_text.json` (8 cases) directly. For wer and word coverage, record vectors with `rust/golden/record.py` (runs the Python functions, `TAKETWO_DATA` temp, no model) into `rust/golden/text.json`.
- `normalize_for_wer` is the transformers Whisper `EnglishTextNormalizer` (numbers, spellings from `normalizer.json`, symbols and diacritics), not a small helper: port it in full. `rust/golden/record.py` gets it from the tokenizer files only (`WhisperTokenizer.from_pretrained(..., local_files_only=True).normalize`, offline) and sets `ml._normalizer`; it never calls `stt_model()`. See lead decision 3 in PROGRESS.md.
- Done when: `cargo test -p taketwo-core text` passes with every golden case and a passed count above zero.

### S04 Library store, seeder, one-time copy
Needs: S01. Gate: G1, G2 (seeder).
- Port `db.py`: same SQLite file name and schema (`rusqlite`, bundled), profiles and generations as JSON documents, `put_generation_live`, soft delete, purge after 7 days, `patch_generation_meta`, settings with `SETTING_RULES`, the startup repairs from `main.py` (queued/running to interrupted, and so on). Same folder layout as `config.py`.
- `taketwo-engine seed --fixture contract/fixture.json`: the same observable state as `contract/seed_python.py` (ids, texts, statuses, `created` order, a tone WAV at each chunk `file`, `final.wav` where the narration has `file`, a `preview.wav`, nothing for `ctpending01`). It refuses to run outside isolated mode.
- One-time copy as a function of (source, destination), tested on a synthetic VoiceForge-shaped folder: copies, never writes the source, does nothing when the destination already has a library.
- A test opens a database written by Python's `db.py` (made in a temp folder by `rust/golden/record.py`) and reads it back unchanged.
- Done when: `cargo test -p taketwo-core library` passes.

### S05 Captions, timings, subtitles, review rules
Needs: S03, S04. Gate: G1.
- Port `tools.py` captions, timings, `to_srt`, `to_vtt`, `to_txt`, `attachment_header`; `pipeline.py` `summarize`, `fix_targets`, `ISSUE_LABEL`, `review`, `outcome`, `set_accepted`, take bookkeeping (`TAKE_FIELDS`, `MAX_OLD_TAKES`, `_archive_take`, `_trim_takes`, `take_audio`).
- Tests: for each fixture narration, timings, captions, srt, vtt, txt, review and outcome equal `contract/golden/narrations.json` exactly.
- cargo takes one name filter before `--`; several go after it. Extra vectors come from `rust/golden/record.py` into `rust/golden/review.json` (see lead decision 5 in PROGRESS.md): the contract fixture has one part with heard words and no takes or accepted parts.
- Done when: `cargo test -p taketwo-core -- review captions` passes, with each test asserting the number of golden cases it read.

### S06 Engine: read side, guards, UI, contract runner
Needs: S05. Gate: G2 (part).
- axum on 127.0.0.1. FastAPI-compatible errors (`{"detail": ...}`). Host guard (403), Origin guard on writes (403), id and path checks (`..`, encoded `/`).
- Routes: health, pipeline-version, status, profiles list/get/analysis/audio/preview, generations list/search/get/timings/subtitles/audio (WAV, download name; `final.mp3` only if the file is there, making it is S16), part audio, take audio, jobs, settings get/put, a no-op cancel (`{"ok": true}`, there is no worker yet), the start-up stubs and the 501s listed under Layout, static UI from `--ui`. Every other write route checks its id and answers 501 until S07a/S07b.
- `rust/scripts/contract.sh`: build, make a temp library, seed, start the engine on a free dev port, run `TAKETWO_URL=http://127.0.0.1:<port> python3 -m unittest discover -s contract`, stop the engine, delete the library. It fails unless the run says `Ran 33 tests` and `OK` with no skips.
- Done when: `contract.sh` with the `-k` list in lead decision 6 (PROGRESS.md) prints `Ran 20 tests` and `OK`: Health 3, Profiles 1, Generations 3 (the two create tests are S07a), Search 3, TimingsAndSubtitles 3, Audio 3, SafetyGuards 4.

### S07a Engine: write routes that start no job
Needs: S06. Gate: G2 (full). No push. Split from the old S07 by lead decision 7: none of the 9 contract tests S06 left red needs a job.
- Routes answered in full: accept and its undo, delete, restore, patch title/favourite, reveal (the opener is injected; tests never open the file manager), and every refusal (400/404/409/422) of create, resume, regenerate, select-take and fix-remaining.
- Paths that would start a job (create success, resume, regenerate, select-take and fix-remaining after their checks) answer 501 until S07b. `PATCH` with a changed `speed` answers 501 until S16.
- Pure ports with golden cases: `pipeline.create_generation` (the document, not the save) and `pipeline.clean_replacement`.
- `record_http.py` and `http.json` grow the write cases.
- Done when: `rust/scripts/contract.sh` prints `Ran 33 tests` and `OK`.

### S07b Engine: worker, stub renderer, lifecycle (push)
Needs: S07a. Gates: G2 (kept), base for G3 and G4. Push at the end so the Windows `check` job sees the engine.
- One worker thread with a queue and a cancel flag (`worker.py`), job documents, `/api/status` and `/api/jobs/{id}` in today's shape, diagnostics, busy 409s, duplicate 409 with `duplicate_of`, cancel while queued and while running, the "interrupted" repair on restart.
- One seam in `taketwo-engine`: a `Renderer` trait (run narration, re-speak a part, fix remaining, select take). A deterministic stub (tones, a gate the test controls) is in the library for in-process tests, and `serve --stub-renderer` wires it in isolated mode only (lead decision 8: a switch at start, not a cargo feature, so `cargo test --workspace` runs the stub tests and S08/S09 can start a stub engine). Without the switch the paths that start a job keep answering 501, never tones. S15/S16 supply the real renderer. The model traits `Voice`, `Transcriber`, `SpeakerEncoder` move to S12, S13, S14, where their real shapes are known.
- Lifecycle: lock file (a second engine is refused), `--idle-exit <seconds>` (tested with a few seconds; MCP passes 900), exit when the parent that started it goes away, address file in live mode only (tested on a temp folder; `--live` is never run; S08 adds the isolated file under `TAKETWO_DATA`, lead decision 9).
- The 33-test contract can't see any of this, so it has its own Rust integration tests; the lead writes the acceptance commands in the decision that starts it.
- Done when: those tests pass, `contract.sh` still prints `Ran 33 tests` and `OK`, and the Windows `check` job is green on the pushed commit (`rust/scripts/g6.sh check`). Hand-back point if the session runs short: worker, stub and job shapes green and committed first; lifecycle and the push second.

### S08 MCP server
Needs: S07b. Gate: G4. No push. Binding detail: lead decision 9 in PROGRESS.md.
- Port `mcp/taketwo_mcp.py` with `rmcp` over stdio: `list_voices`, `generate_narration`, `get_status`, `get_timings`, `get_audio`, `get_subtitles`, `find_narration`, `respeak_part`, `fix_remaining`, `stop_narration`, `resume_narration`, `mark_sounds_fine`, with the same parameters, defaults, descriptions, replies and error texts. It answers to both names (`--name taketwo|voiceforge`).
- `taketwo-mcp` is a thin HTTP client: it no longer depends on `taketwo-voice` (a CUDA build of it would not start without the NVIDIA driver). Its `cuda` and `flash-attn` features stay as empty features so the G6 build command is unchanged.
- Finding the engine, isolated mode (the only mode that runs in this loop): `TAKETWO_URL`/`VOICEFORGE_URL` (refused when its port is 8765-8785), then `<TAKETWO_DATA>/engine.json`, then start `taketwo-engine` from the folder of its own program with `serve --idle-exit 900` and wait for that file. No port scan and no app-data file is ever read in isolated mode. Live mode (app-data address files under both names, then a scan of 8765-8785, then start) is written as functions that take the base folder and the port list and is unit-tested on stand-ins; `--live` on the command line stays refused, as in the engine.
- Tests (`crates/taketwo-mcp/tests`): an rmcp client starts the built program against a seeded `serve --stub-renderer` engine on a temp library and calls all 12 tools; the tool list is compared with the Python signatures; the cases from `backend/tests/test_mcp_tools.py` and the contract's `McpTools` class are ported; one test covers "no engine answers, so start one" in isolated mode and stops it afterwards. Every child gets a temp `HOME`, and `TAKETWO_URL`/`VOICEFORGE_URL` set or removed explicitly.
- MP3 is made in S16: until then `get_audio` is tested with `wav` and with the engine's 404 for `mp3`. G4 is judged again after S16.
- Done when: `cd rust && cargo build -p taketwo-mcp && cargo test -p taketwo-mcp` passes from a state with no prebuilt engine program.

### S09 Tauri shell (push)
Needs: S07b. Gate: G3. Binding detail: lead decision 10 in PROGRESS.md (live mode stays off, the engine lock stands in for single instance, the smoke's checks, the Windows `check` job excludes the shell and reports it separately).
- `crates/taketwo`: Tauri 2, one shell per library through the engine's lock file (no single-instance plugin yet), starts `taketwo-engine serve --exit-with-stdin` as a child found beside the program, waits for health, opens the window at the engine's address (`WebviewUrl::External`), stops the engine when the window closes. What `desktop/main.js` does today, minus updates.
- Build the UI in the worktree: `cd frontend && npm run build -- --configLoader runner` (uses the symlinked `node_modules`; the flag keeps vite from writing its temp config file there; `frontend/dist` is git-ignored).
- `rust/scripts/smoke_shell.sh`: isolated mode, temp library seeded with the fixture, a free dev port, hidden shells only, checks a to i of lead decision 10 (health, page and asset, parentage, page load, second-shell refusal, close and `kill -9` paths, refusals, leftovers, home-folder report). It removes a `frontend/dist` it built itself. A clean engine exit now removes `engine.lock` as well as `engine.json`.
- Done when: `cargo build -p taketwo && rust/scripts/smoke_shell.sh` passes.

### S10 Audio DSP with Python parity vectors
Needs: S01. Gate: G1.
- `rust/golden/record.py` records vectors from `backend/app/audio.py` on synthetic signals (tones, noise, bursts, gaps, clipped and faded shapes) into `rust/golden/audio.json`. No model, no real audio.
- Port `audio.py`: WAV load/save (`hound`), resample (`rubato`; tolerance against `resample_poly` stated in the test), high-pass (`sci-rs` Butterworth, forward-backward), loudness (`ebur128` against pyloudnorm), `normalize_lufs`, `soft_limit`, `frame_db`, `speech_threshold_db`, `silence_report`, `trim_edges`, `speech_start`, `ending_level`, `shorten_gaps`, `fade`, `clipping_ratio`, `noise_bursts`, `estimate_snr`, `concat_with_pauses`, `sound_regions`, `end_analysis`, `_terminal_end`, `lead_burst`, `edge_silence`; ffmpeg as a separate program for decode, MP3 and speed.
- Every tolerance is written next to its test with the reason. Decisions (which frames count as silence, where a cut lands) must match exactly; only filter and loudness numbers may differ, within the stated tolerance.
- Done when: `cargo test -p taketwo-core audio` passes.

### S11 Delivery, ending guard, check decisions
Needs: S03, S10. Gate: G1.
- Port `delivery.py` (`prosody` with a hand port of librosa `yin`, `expressiveness`, `resolve`, `pick_variant`, `ltas`, `mic_match` with `firwin2`), `ending_guard.py`, `studio.master_chain`, and from `pipeline.py`: `noise_limit`, `_internal_sentence_cut`, `evaluate` (every check with today's limits, `HARD_ISSUES`), `widen_sentence_pauses`, `_processed_edge_issues`, `clean_replacement`.
- Vectors: `rust/golden/record.py` runs `evaluate` and friends on synthetic audio with canned transcripts, similarities and settings (the model calls are replaced by fixed values) and stores inputs, issues and measurements in `rust/golden/checks.json`.
- Port the cases in `backend/tests/test_narration_quality.py`, `test_ending_guard.py`, `test_repeat_cutoff.py`, `test_regressions.py` and `test_audit_fixes.py` that test these functions. List any case left out in PROGRESS.md with the reason.
- `profiles/<id>/mic.json` is written by Python with plain `json.dumps` and likely holds `NaN` (`delivery.ltas` returns nan above 0.92 x Nyquist): read it with a NaN-tolerant parser, never plain serde_json (found in S04).
- Done when: `cargo test -p taketwo-core -- checks delivery ending_guard` passes, with each test asserting the number of golden cases it read.

### S12 Qwen voice (real model, Metal)
Needs: S01. Gate: G5 (part).
- `taketwo-voice::qwen`: load once, bf16 on Metal, `synthesize(text, ref wav, ref text, language, params, seed, expected seconds, cancel)` matching `engines.synthesize` for the `qwen3` family: `max_tokens = 12.5 * (expected * 1.8 + 3) + 8`, the synthesis report (tokens, limit, stop reason, sampler), cancel checked every frame, unload on idle (`IDLE_UNLOAD_SECONDS`).
- The three listening rules, each with a unit test that needs no model: the sub-talker always takes the caller's temperature, top-p and top-k (defaults 0.7 / 0.9 / 40; repetition penalty 1.05 on the first codebook only), never the crate defaults; the exact reference cut (`ref_len x 1920` samples, see `qwen_compat.py`), not the proportional one; the last reference frame is kept.
- One model test, ignored by default (`cargo test -p taketwo-voice --features metal -- --ignored qwen_smoke`): checks 8766 is idle, renders one short line from a reference made with macOS `say` in a temp folder, asserts the sample count is a multiple of 1920 before trimming, records seconds and peak memory. At most 3 takes. No claim about sound.
- Done when: the unit tests pass and the ignored smoke test has run once with its numbers in PROGRESS.md.

### S13 Whisper with word timings
Needs: S02 (binding chosen and linking). Gate: G5 (part).
- `prototypes/scripts/convert_whisper_ggml.py`: converts the local MLX Whisper large-v3-turbo weights plus the local OpenAI tokenizer files to a ggml file in `rust/.models/` (Python from `backend/.venv`, read-only use). No download.
- `taketwo-voice::whisper`: whisper.cpp through the thin binding, DTW token timings with the large-v3-turbo alignment preset, flash attention off; mlx-audio's word grouping ported; the same result shape as `ml.transcribe` (`text`, `segments`, `words` with start and end).
- Check (ignored test, one model at a time, 8766 idle): on three `say`-made clips, the Rust transcript's words equal Python's `ml.transcribe` words after `normalize_for_wer`, and word start/end differ by a stated tolerance; record the differences. This is map ticket #11's question, answered with numbers only.
- If the conversion is not faithful after one session: stop, write what was tried, and the lead asks Franz about downloading `ggml-large-v3-turbo.bin`.
- Done when: `cargo test -p taketwo-voice --features metal -- --ignored whisper_parity` has run and its numbers are in PROGRESS.md.

### S14 Speaker encoder
Needs: S12. Gate: G5 (part).
- Port the Chatterbox voice encoder (LSTM, from the local `ve.safetensors`) to Candle with its mel front end; `speaker_embedding`, `cosine` as in `ml.py`.
- Check (ignored test): on three `say` clips, cosine between the Rust and Python embeddings of the same clip is at least 0.999; record the values.
- Done when: `cargo test -p taketwo-voice --features metal -- --ignored encoder_parity` has run with numbers in PROGRESS.md.

### S15 Render pipeline A: takes and retakes
Needs: S07b, S11. Gate: G1.
- Port `profile_assets`, `_ref_pitch`, `_guarded_take`, `render_chunk` (seeds, retake rounds and budgets, the choice between takes, hard and soft issues, what is stored on the part), using the traits so tests run on the stub.
- Tests script the stub (take 1 cut off, take 2 clean; all takes flagged; cancel mid-part; ending guard on and off) and compare the part document with what Python produces for the same scripted inputs (`rust/golden/record.py` drives `render_chunk` with patched model calls into `rust/golden/render.json`).
- Done when: `cargo test -p taketwo-engine render_chunk` passes.

### S16 Render pipeline B: assembly, final scan, re-speak, takes, speed (push)
Needs: S15. Gate: G1, G2 (still green). Push at the end.
- Port `assemble` (pauses, mic match, master chain, loudness), `final_scan`, `run_generation`, `regenerate_chunk` with snapshot and restore, `select_take` with remix, `fix_remaining`, `apply_speed`, `retime`, `create_generation`, MP3 export, the WAV and subtitle files written beside the narration.
- Replace S07b's stub renderer with the real one. Port the cases from `backend/tests/test_review_loop.py` and the rest of `test_regressions.py`. Add stub-engine tests for what the contract can't reach: a successful create and render, re-speak, fix-remaining, speed change, select-take of a kept take, MP3.
- Done when: `cargo test --workspace` and `rust/scripts/contract.sh` both pass.

### S17 Voice creation
Needs: S13, S14, S16. Gate: G5 (part).
- Port `reference.py` (`analyze`, utterances, pause valleys, `snap_window`, window choice, `room_echo`, tips, `refresh_clips`) and the profile routes: create (multipart upload, ffmpeg decode, length limits), patch, delete, re-analyze, refresh, analysis, `make_preview`, `delivery.backfill`.
- Without Sidon (S18) the "studio" option answers with a clear "not available yet" and the plain path is used.
- Tests on the stub with `say`-made clips; vectors for the pure parts from `rust/golden/record.py`.
- Done when: `cargo test -p taketwo-engine profiles` passes.

### S18 Studio restoration (Sidon)
Needs: S17. Gate: G5 (the smoke reports whether studio ran); it completes decision #4.
- Port `studio.restore`: the w2v-BERT feature extractor by hand, Sidon predictor and vocoder through `ort` (fp16, local files), 20 s chunks with 1 s overlap; `noisereduce` by hand where `reference.py` uses it.
- Check (ignored test): Rust and Python outputs on one `say` clip with added noise; record length, loudness and spectral distance. No claim about sound.
- If it does not fit in one session, record what is left; G5 does not depend on it.
- Done when: `cargo test -p taketwo-voice -- --ignored sidon_parity` has run with numbers in PROGRESS.md.

### S19 Real synthesis smoke
Needs: S17 (S18 optional). Gate: G5.
- `rust/scripts/synth_smoke.sh`: check 8766 is idle (else exit 75, "busy, try later"); pick one voice and copy one reference clip through `GET http://127.0.0.1:8766/api/profiles` and `GET /api/profiles/<id>/audio/<file>` into a temp folder; start `taketwo-engine` (release, `metal`) in isolated mode on a dev port with `TAKETWO_MAX_TAKES=20` under `/usr/bin/time -l`; create the voice from that clip; create one narration of 2-3 fixed sentences; wait; fetch the WAV, SRT and timings into `rust/out/smoke/`; stop the engine; delete the temp library.
- It writes `rust/out/smoke/report.json`: takes used, seconds per phase, audio seconds, peak memory, the checks' result for each part (issues, outcome), which options ran (studio on or off). It prints "no quality claim; Franz listens".
- Done when: the script exits 0, `report.json` shows `takes <= 20`, the WAV and SRT exist, and the numbers are in PROGRESS.md.

### S20 Blind A/B harness (not rendered)
Needs: S07a (for the engine's API); can run any time after it. Gate: G7.
- `rust/ab/ab.py` (standard library):
  - `plan`: reads `GET http://127.0.0.1:8766/api/generations` (read-only), picks 10 finished narrations by a written rule (different voices where possible, short, medium and long, no duplicates of the same script), stores ids, titles and scripts in git-ignored `rust/ab/private/scripts.json`, and prints the count of parts and the takes needed (parts x 2 engines, plus the worst case with retakes).
  - `render --franz-ok`: refuses without the flag. With it: starts a Python engine and a Rust engine, each on its own temp library and dev port, one at a time; copies each needed voice through the API (profile document and its audio files) into both; renders the 10 scripts with each; never writes to Franz's library.
  - `page`: builds a blind listening page like `eval/rust-prototype` (random A/B order per script, the key kept in a separate private file).
  - `selftest`: runs `page` on tone files to prove the page builds.
- Do not run `render`.
- Done when: `python3 rust/ab/ab.py plan` prints 10 scripts and a take count, `python3 rust/ab/ab.py selftest` passes, and `python3 rust/ab/ab.py render` exits non-zero saying it waits for Franz.

### S21 Final gate run (push)
Needs: all. Gates: all.
- Push `rust/taketwo`; wait for the Windows workflow on the final commit.
- Run every gate command below in order and paste the real output tails into PROGRESS.md. That log commit changes only a `.md` file, so it starts no build and `g6.sh` still judges the last code commit. Write `rust/README.md` (how to build, run in isolated mode, run the gates).
- Done when: G1 to G8 pass on the same code commit.

## Gate commands

Each is one command whose exit code is the verdict. G5 renders audio and G6 waits on GitHub: run them only after their steps are done, not on every loop turn.

| Gate | Command | Passes when |
|---|---|---|
| G1 | `cd "$WT"/rust && cargo test --workspace` | exit 0 from a clean checkout; the golden and stub tests are in the run (not ignored) |
| G2 | `cd "$WT" && rust/scripts/contract.sh` | exit 0; prints `Ran 33 tests` and `OK`, no skips |
| G3 | `cd "$WT"/rust && cargo build -p taketwo && scripts/smoke_shell.sh` | exit 0; health names TakeTwo; UI served; no process or port left |
| G4 | `cd "$WT"/rust && cargo build -p taketwo-mcp && cargo test -p taketwo-mcp` | exit 0; all 12 tools called |
| G5 | `cd "$WT" && rust/scripts/synth_smoke.sh && cat rust/out/smoke/report.json` | exit 0; takes <= 20; WAV and SRT exist; timings and peak memory recorded. Exit 75 means Franz's app was busy: try later |
| G6 | `cd "$WT" && rust/scripts/g6.sh` | exit 0: the plain `cuda` job is `success` on the newest non-`.md` commit of `rust/taketwo`; flash-attn printed separately. Exit 75: still running |
| G7 | `cd "$WT" && python3 rust/ab/ab.py plan && python3 rust/ab/ab.py selftest && ! python3 rust/ab/ab.py render` | exit 0: 10 scripts and a take count printed; selftest OK; render refuses |
| G8 | `cd "$WT" && rust/scripts/g8.sh` | exit 0: this work is on no branch or tag but `rust/taketwo`; no process of this work has a file open in Franz's folders; 8766 health OK. The printed data differences are Franz's own app use: information, not a verdict |

Stop-and-clean check after any run: `lsof -nP -iTCP -sTCP:LISTEN | grep -E ':(879[0-589])'` shows nothing of ours, and `pgrep -fl 'rust-taketwo/rust/target/'` is empty. Stop only processes you started, by their pid. The Python `mcp/taketwo_mcp.py` servers and anything on 8765-8785 are Franz's: never stop them.

## Order and what can run side by side

Critical path: S01, S02, S03, S04, S05, S06, S07a (G2), S07b, then S10, S11, S15, S16, with S12, S13, S14 before S17, then S19, S21.
Independent after S07b: S08 (G4), S09 (G3). Independent after S07a: S20 (G7). Independent after S01: S10, S12.
Model steps (S12, S13, S14, S18, S19) never run at the same time as each other.

## Known risks and the fallback for each

| Risk | Fallback |
|---|---|
| MLX Whisper weights do not convert to a faithful ggml file | Ask Franz to approve one download; until then S13, S17, S19 wait and everything else continues |
| whisper.cpp CUDA does not link on the Windows runner | Whisper on CPU for Windows, reported |
| `flash-attn` fails on Windows (it did not build in the prototype's history) | Report it; the plain `cuda` job is the gate |
| Memory: Qwen 13.6 GB plus Whisper on 24 GB with swap high | Unload Qwen before Whisper loads if needed; record peaks; never run two model processes |
| Rust `regex` lacks look-around that Python's patterns use | `fancy-regex` for those patterns only |
| Filter and loudness numbers differ slightly from scipy and pyloudnorm | Stated tolerances on numbers; decisions must still match exactly |
| A contract test truly can't apply to Rust | Written up in `contract/README.md` and agreed by the lead first; never skipped quietly |

## Not in this plan

Code signing, installers and app bundles, the update flow, Voice Design, the Speech to Text page, the model catalogue, other engine families, a Windows sound test (needs a real NVIDIA PC), re-tuning the checks on Rust takes, the wiki receipt and HANDOFFS note (they live outside the worktree).
