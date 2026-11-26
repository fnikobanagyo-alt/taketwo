# What Re:Create's Rust migration learned, and what transfers to TakeTwo

Research for issue #5 (parent #1). Date: 2026-11-26.

**Source.** Re:Create, a sibling local app owned by the same person. It was read only: nothing there was run, built or edited. Every path below is relative to the Re:Create project root, and every line number was read on 2026-11-26. Where a review quotes line numbers in Re:Create's own `app/src`, this note cites the review's line instead, because the code has changed since.

**What Re:Create is.** It is not a desktop app. It is one Rust binary: a `tiny_http` server on `127.0.0.1` that serves plain-JavaScript pages to the browser, plus a stdio MCP server in the same binary (`PLAN.md:209`, `CONTRACT-R.md:162`). A double-click `.command` script builds and starts it (`Start ReCreate.command`). The app was ported from Python 3.11 between 2026-11-20 and 2026-11-21 (`STATUS.md:25`).

The port changed no behaviour on purpose: "A difference is a bug, never an improvement" (`CONTRACT-R.md:15`). That makes it a good guide to *how to port and prove it*. It is a poor guide to TakeTwo's shell and installers.

---

## 1. Lessons that transfer, most useful first

### 1.1 Build the proof before the port: one black-box suite, passed by the old app first
- One parity suite, written as cargo integration tests. It starts either app from a command line, talks to it only over HTTP and the MCP stdio loop, and reads the files it writes. It never imports app code (`CONTRACT-R.md:790`, `CONTRACT-R.md:16`).
- The suite was run against the **Python** app first. Its snapshots were recorded there, and only then was porting allowed: "A test that fails against the Python app is a wrong test" (`CONTRACT-R.md:873`, `MIGRATION-PROMPT.md:90`). Slice R0 was "the proof first" (`CONTRACT-R.md:1020`).
- The snapshots record every request and reply, every file written under the data folder, and every argv the fakes saw. Ids, times, pids, ports and temporary paths are masked (`CONTRACT-R.md:865-872`). Only a Python run can write a snapshot. A judge later checked in git history that no snapshot was rewritten from a Rust run (`reviews/RUST-MIGRATION-judge-1-1.md:31-32`).
- A judge also mapped every old test to a new one: 361 Python tests against their Rust counterparts. That check found tests the contract claimed existed but did not (`reviews/RUST-MIGRATION-judge-1-1.md:29-30`, `:81-86`).
- **For TakeTwo:** write the HTTP-level suite against today's Python VoiceForge backend first, record its replies and data files, and only then port. The old app is the answer key.

### 1.2 Golden cases for pure logic, especially regex and text rules
- Before each pure module was ported, the Python functions wrote real inputs and their outputs to JSON, including odd ones: empty text, accents, emoji, numbers with commas. Rust unit tests replay them (`MIGRATION-PROMPT.md:92`). There were 19 files with 4,844 cases (`STATUS.md:25`).
- Python `re` and Rust `regex` differ at the edges. `\w`, `\s` and `\b` use different character sets, and `re.I` folds `İ`/`ı` differently. Python's `$` also matches before a final `\n`. Every pattern got golden cases, and some had to be written by hand (`CONTRACT-R.md:915-954`, see R7, R15-R16, R36).
- `serde_json` misread about one 17-digit float in ten by 1 ULP until the `float_roundtrip` feature was turned on (`CONTRACT-R.md:27`). It refuses `NaN`/`Infinity`, which Python accepts (`CONTRACT-R.md:1106`). Keeping JSON key order needed a hand-written ordered value (`CONTRACT-R.md:29`, `:1124`).
- **For TakeTwo:** VoiceForge's text splitting, ending checks and thresholds are exactly this kind of logic. Golden-case them from the Python before porting. Decide early whether TakeTwo must match Python's text and JSON **byte for byte**. Re:Create did, and paid for it in `pytext.rs` and `pyjson.rs` (`CONTRACT-R.md:43-45`).

### 1.3 Keep a Python-only ML library as a child process, not a reason to stay on Python
- `mlx_whisper` exists only for Python, so `whisper_run.py` (15 lines) stays. The Rust app starts it as a separate program in its own process group, with a timeout, and Stop kills it (`MIGRATION-PROMPT.md:83`, `CONTRACT-R.md:1118-1120`). The README makes this the only exception to "all app code is Rust" (`README.md:11`).
- Rust did not make the heavy work faster, because the work is done by the programs the app starts. The prompt said so up front: "Rust will not make clipping faster … promise nothing more" (`MIGRATION-PROMPT.md:82`).
- **For TakeTwo:** mlx-audio is in the same position. Plan a Python sidecar for inference behind a small, stable interface, and do not promise faster narration from the rewrite.

### 1.4 Process layout: port 0, a "ready line", and one binary with subcommands
- `recreate --port N` binds `127.0.0.1:N`, and `0` lets the system pick. When start-up is done the binary prints one fixed line, `Re:Create is running at http://127.0.0.1:<port>`. Callers read stdout until that line (30 s timeout) and take the port from it (`CONTRACT-R.md:810-811`). This makes parallel tests and side-by-side servers safe.
- A port that is already taken gives one plain sentence, exit code 1, and no file changed (`CONTRACT-R.md:813`).
- A fixed fallback port for fake services raced with parallel tests that had been handed the same freed port-0 port. They moved to a checked range of 30000-39999 (`CONTRACT-R.md:795`).
- The server, the storage check, the gate and the MCP server are one binary with subcommands (`CONTRACT-R.md:803-808`). A judge found that the MCP binary and the server binary can drift apart if only one is rebuilt (`reviews/RUST-MIGRATION-judge-1-1.md:114-116`).
- **For TakeTwo:** have the Python sidecar print a ready line with its real port, and have the Rust/Tauri side read it, instead of guessing a fixed port. VoiceForge already writes `engine.json` with its URL (see 1.9).

### 1.5 A build folder outside the project, and paths baked in at build time, broke worktrees
- The project's `app/.cargo/config.toml` set one absolute `target-dir` outside the repo. The default data folder came from `env!("CARGO_MANIFEST_DIR")` at build time.
- So an agent that built a release in a git worktree replaced the binary the owner and the MCP client used with one that served the worktree's jobs and pages. Two agents running `cargo test` at once could also rebuild each other's binaries (`reviews/RUST-MIGRATION-judge-1-1.md:88-93`).
- Agent worktrees inside the project also pushed it over its storage budget and failed the check command (`reviews/RUST-MIGRATION-judge-1-1.md:76-79`).
- **For TakeTwo:** never bake a source path into the binary as the data location. Pass the data folder at run time (VoiceForge already has `VOICEFORGE_DATA`). Give each worktree its own `CARGO_TARGET_DIR`.

### 1.6 Stopping children cleanly takes deliberate work, and the design is Unix-only
- Each child leads its own process group (`process_group(0)`). Stop sends TERM to the group, then KILL after 2 s. Signals were declared with `extern "C"` because no crate was allowed (`MIGRATION-PROMPT.md:71`, `CONTRACT-R.md:41-42`, `reviews/RUST-MIGRATION-judge-1-1.md:54`).
- A race was still found late. A child started in the instant the server exits is not killed, so a download or a billed provider call can outlive the app. Python had the same gap (`reviews/RUST-MIGRATION-judge-3-3-code.md:90`).
- **For TakeTwo:** the sidecar must die with the app, and generation children must die with Stop. Test both explicitly, including the moment of shutdown. On Windows, process groups and `killpg` do not exist (see section 3).

### 1.7 Atomic writes and tolerance of damaged state were added as named improvements
- Records go through write-to-temp, then rename (`reviews/RUST-MIGRATION-judge-1-1.md:55`). A later judge noted the rename happened without a flush, so `sync_all` was added (`CONTRACT-R.md:1096`, D13). Stray direct writes were still found later (`reviews/RUST-MIGRATION-judge-3-2-code.md:95`).
- In Python, one damaged `job.json` or settings record crashed start-up or the job list. The Rust app now skips or survives it, and each case is listed as a deliberate difference (`CONTRACT-R.md:1091-1094`, D8-D11).
- **For TakeTwo:** VoiceForge already sets aside a damaged `settings.json` and saves atomically. Carry both rules over, with tests, and include the flush.

### 1.8 Flaky tests came from load and randomness, not from Rust
- One race between "the job reads failed" and "its files are swept" showed only under the load of the whole suite, on **both** apps (`reviews/RUST-STAGE1-proof.md:53-62`, `:169-172`).
- A test video made with ffmpeg's `gradients` came out different on each build, because that filter picks random colours by default. A fresh build then failed snapshots (`reviews/RUST-MIGRATION-judge-3-1-code.md:110-115`). Test runs also left 36-117 MB temp folders behind (`reviews/RUST-MIGRATION-judge-3-1-code.md:122-126`).
- Many tests each running a real ffmpeg starved each other. The project caps test threads at 4 (`app/.cargo/config.toml`, the `RUST_TEST_THREADS` line, with the measured 408 s against 242 s). Later, waits that hit their limit under load were measured and raised, and the measured cause was written down instead of the guessed one (`STATUS.md:7`, `STATUS.md:22`).
- **For TakeTwo:** pin every generated test fixture (seeded audio, fixed WAVs). Limit how many tests run inference or ffmpeg at once. Clean temp folders in a `Drop`. Fix a flaky test only after you have measured its cause.

### 1.9 Calling VoiceForge: Re:Create is already a client, so TakeTwo must keep its API or break it knowingly
- Re:Create's Voice feature (built 2026-11-24, `STATUS.md:11`) finds VoiceForge by reading `~/Library/Application Support/VoiceForge/engine.json`, key `url`. It accepts only `127.0.0.1`/`localhost` on ports 8766-8785, never scans ports and never opens the app (`CONTRACT-1C.md:15`).
- It calls only `GET /api/health`, `GET /api/profiles`, `POST /api/generations`, `GET /api/generations/<id>`, `GET /api/generations/<id>/audio?fmt=mp3` and `POST /api/generations/<id>/cancel` (`CONTRACT-1C.md:21`).
- It depends on these reply shapes: `status`, `chunks[].status`, `outcome` (`clean` / `needs_listen`), `review[]`, `file`, `duration`, a 12-hex `id`, and a 409 with `detail.duplicate_of` (`CONTRACT-1C.md:144-146`).
- It knows that VoiceForge answers unknown GET paths with an HTML page and status 200. So it checks the JSON shape of `/api/health` and `/api/profiles` before trusting the address (`CONTRACT-1C.md:19`).
- It notes that "VoiceForge's interface is not a contract" (`CONTRACT-1C.md:133`).
- Today's VoiceForge backend serves exactly these: `ENGINE_FILE` at `backend/app/main.py:77`, and the routes at `:131`, `:392`, `:667`, `:711`, `:728`.
- **For TakeTwo:** treat these six routes, the `engine.json` handshake and the reply fields as a frozen external contract. Put them in the parity suite from 1.1, or decide on purpose to break Re:Create and tell its owner.

### 1.10 Smaller lessons
- **Test-only hooks compiled in but inert.** Knobs and `/_test/` routes ship in the binary and are read only when `RECREATE_TEST_HOOKS=1` is set, so the check stays a plain `cargo test` (`CONTRACT-R.md:1125`). A judge confirmed they answer 404 without the flag (`reviews/RUST-MIGRATION-judge-1-1.md:52`).
- **The frontend talks to the backend** through same-origin JSON over `fetch`. Every POST must be `application/json`, and an `Origin`, when present, must be the app's own `127.0.0.1`/`localhost` port, else 403. Bodies are capped at 64 KB. Errors always have the shape `{"error", "next"}` (`CONTRACT-R.md:172-179`). This matches the Host/Origin checks VoiceForge added before publishing. In Tauri, the webview's origin is not `http://127.0.0.1`, so this check needs a deliberate rule (not covered by Re:Create).
- **`tiny_http` quirks** became listed accepted differences. It switches to chunked replies at 32 KB unless the threshold is raised. It closes the connection with no reply on a non-ASCII header. It sends `100 Continue` (`CONTRACT-R.md:178`, `:1104`, `:1113-1116`).
- **Measured gains were small and honest.** Median ready time 6 ms against 37 ms, and idle memory 6.9 MB against 25.4 MB. The first start of a fresh binary took 403 ms, probably macOS checking a new executable; that cause is inferred, not measured (`reviews/RUST-STAGE1-proof.md:110-120`).
- **Deleting the old code safely.** Tag the last commit that has it (`python-final`). List the files, then remove them with `git rm` in a commit of their own (`MIGRATION-PROMPT.md:94-98`).
- **Fresh judges who built none of the work.** Five categories, a 9.0 bar, and the evidence written per round to `reviews/`. Real model calls were capped at 60 for the whole run (`MIGRATION-PROMPT.md:146-158`, `STATUS.md:117`).

## 2. What took longest (inferred from dated status entries; no timing data exists)
- **The port itself was fast.** The run started 2026-11-20. By 2026-11-21 the parity suite was green on Rust three runs in a row, the Python was tagged and deleted, and stage 2 code judging had begun (`STATUS.md:25`).
- **Product quality took longest, not Rust.** Stage 3 (clips that actually pass) used 46 of the 60 real calls and was paused on 2026-11-21 with no clip at the 9.0 bar (`STATUS.md:25`). Later contracts (1B4) were still "NOT FINISHED (no ready clip yet)" (`STATUS.md:23`).
- **The page judges never reached 9.0.** UI and UX were 8.0/7.0, then 8.8/8.6, then 8.9/8.7 (`reviews/RUST-MIGRATION-judge-2-1-pages.md:12-13`, `-2-2-pages.md:12-13`, `-3-3-pages.md:10-11`). The last MAJOR finding was a stopped server showing a developer command to the owner (`reviews/RUST-MIGRATION-judge-3-3-pages.md:74`).
- **Test flakiness under load kept coming back** for days after the port (`STATUS.md:7`, `STATUS.md:22`).
- **For TakeTwo:** budget most of the time for audio-quality proof, the "app not running" and error states in the UI, and test stability under load, not for the Rust translation.

## 3. Does not transfer, or no evidence in Re:Create
- **App shell, Tauri, installers, signing, notarisation, auto-update:** none. Re:Create is a browser page started by `Start ReCreate.command`, which builds with cargo on first run. There is no `src-tauri`, and the only `Cargo.toml` is `app/Cargo.toml`. TakeTwo needs separate research on these.
- **Windows:** none. Re:Create is macOS-only, and several pieces would not port as written:
  - job ids from `/dev/urandom` (`CONTRACT-R.md:36`)
  - `process_group(0)` and `killpg` via `extern "C"` (`CONTRACT-R.md:41-42`)
  - SIGTERM/SIGINT for stopping (`CONTRACT-R.md:812`)
  - `#!/bin/sh` wrapper fakes in tests (`CONTRACT-R.md:797`, `:856`)
  - the hard-coded `~/Library/Application Support` path to VoiceForge (`CONTRACT-1C.md:15`)

  TakeTwo needs a portable answer for each (for example, Windows Job Objects for killing children).
- **The six-crate whitelist** (`app/Cargo.toml`, `CONTRACT-R.md:25`) was the owner's choice for a small, disk-light tool. It forced hand-written base64, UTC dates, an HTTP client and signals (`CONTRACT-R.md:33-45`). That does not carry over to a Tauri app, which already pulls in a large dependency tree. Pick crates on their merits, with approval.
- **Byte-identical copying of Python's quirks** (HTTP/1.0 error pages, Python float `repr`, `string.Template`) mattered because Re:Create kept its old pages and data unchanged. TakeTwo should decide per surface (data files, API replies) whether it needs that, rather than adopting it wholesale.

## 4. Not verified
- Nothing in Re:Create was run. Test counts, timings and memory figures are as its own reviews report them.
- The `app/src` line numbers that reviews cite are from 2026-11-21 and may have moved.
- How long each stage took in hours is not recorded anywhere. Section 2 is inferred from dated entries.
- Whether Re:Create's Voice client still works against the current VoiceForge was not tested. The route list was matched by reading only.
