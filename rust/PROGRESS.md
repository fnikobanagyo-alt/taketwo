# TakeTwo Rust port: progress log

The loop appends one entry per session at the bottom: date, step id, what was built, the commands run with their real results, what is not done, questions for the lead. Never edit earlier entries.

## Step status

| Step | Status |
|---|---|
| S01 Workspace, safety rails, tools | todo |
| S02 Windows CUDA link proof | todo |
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
| G1 | not run | |
| G2 | not run | |
| G3 | not run | |
| G4 | not run | |
| G5 | not run | |
| G6 | not run | |
| G7 | not run | |
| G8 | not run | |

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
