# Engine contract

What TakeTwo (formerly VoiceForge) does over its HTTP API and MCP tools, written as tests that any engine must pass:
today's Python engine and the planned Rust engine. The suite talks to the engine over HTTP only. It never imports `backend/app`.

| File | What it is |
|---|---|
| `test_contract.py` | The black-box suite. Standard library only; it reads `TAKETWO_URL`. |
| `fixture.json` | The library every engine under test is seeded with: one voice and five narrations. |
| `golden/*.json` | Recorded outputs of the Python implementation: text cleaning, chunking, and the timings, subtitles and review items of each fixture narration. |
| `seed_python.py` | The Python engine's seeder and runner. It is engine-specific. |
| `record_golden.py` | Re-records `golden/` from the Python code. It is the only file here that imports `backend/app`. |

## Run against the Python engine

```sh
backend/.venv/bin/python -B contract/seed_python.py
```

This one command does the whole run (about 2 s):

1. It writes `fixture.json` into a fresh temporary library and creates synthetic tones as audio.
2. It starts the engine on a free port in 8790-8799, offline with an empty model cache, so nothing can render or download.
3. It runs the suite, then stops the engine and deletes the library.

`scripts/check.py` runs it too.

The suite refuses to start unless `TAKETWO_URL` points at this computer outside ports 8765-8785, which belong to the desktop app and its real library. It writes to the library it is pointed at; every write is undone before the test ends.

## Run against the Rust engine

The Rust engine has its own seeder, which produces the same observable state from `fixture.json`:

- the same ids, texts, statuses, timelines, `words` and `created` order;
- a WAV at each chunk's `file`, a `final.wav` for each narration with a `file`, and nothing for `ctpending01`.

Seed a fresh temporary library with it (from the repository root, after `cd rust && source scripts/env.sh && cargo build -p taketwo-engine`; `env.sh` puts the cmake the build needs on `PATH`):

```sh
D=$(mktemp -d) && TAKETWO_DATA="$D" rust/target/debug/taketwo-engine seed --fixture contract/fixture.json
```

It refuses to run without `TAKETWO_DATA`, or with `--live`, and writes nothing then. `cargo test -p taketwo-engine seed` checks its output against what `seed_python.py` writes (documents, file list, WAV headers and sample counts, recorded in `rust/golden/library.json`).

`rust/scripts/contract.sh` does the whole run (from any folder):

```sh
rust/scripts/contract.sh              # all 33 tests; exit 0 only with "Ran 33 tests", "OK" and no skips
rust/scripts/contract.sh -k Health    # arguments go to unittest, and its exit code is returned
```

1. It builds `taketwo-engine`, seeds a fresh temporary library with it and starts `taketwo-engine serve` on the first free port of 8790-8795, 8798, 8799 (never 8765-8785).
2. It reads the port from the engine's ready line (`{"ready":true,"port":N,"pid":P}`, 30 s limit) and runs the suite with `TAKETWO_URL` pointing at it.
3. It stops that engine by its own pid and deletes the library.

Since step S07a the Rust engine passes all 33 (`Ran 33 tests`, `OK`).

### Not judged by this suite yet

The suite checks refusals and library writes, not rendering. On the Rust engine these paths answer 501 `{"detail": "Not in TakeTwo 1.0 yet."}` after all their checks pass, and change nothing:

- until S07b (the worker): creating a narration, resume, re-speaking a part (`regenerate`), switching to a kept take (`select-take`) and `fix-remaining`;
- until S16: `PATCH` with a `speed` that differs from the stored one by more than 0.001.

`rust/crates/taketwo-engine/tests/http.rs` checks that each of them answers 501 and leaves every file and document unchanged.

To run the suite by hand, start the engine on a port in 8790-8799 and run:

```sh
TAKETWO_URL=http://127.0.0.1:8791 python3 -m unittest discover -s contract
```

The `McpTools` tests run the Python MCP server's tool functions (`mcp/taketwo_mcp.py`) against the engine under test, so `contract.sh` still runs them against the Rust engine. The same calls against the Rust `taketwo-mcp` program live in `rust/crates/taketwo-mcp/tests/tools.rs` (`all_twelve_tools`, each `McpTools` case named in a comment): an rmcp client starts the built program against a stub engine on a seeded temporary library. Run them with `cd rust && source scripts/env.sh && cargo test -p taketwo-mcp` (it builds `taketwo-engine` itself).

## What is covered

- **Health:** `ok`, `name: TakeTwo`, `formerly: VoiceForge`, and `version`. Also `/api/pipeline-version` (the MCP probe) and an idle `/api/status`.
- **Voices:** the `/api/profiles` list shape.
- **Narration list:** summaries without `chunks`, with `outcome` and `review_count`, newest first.
- **Narration detail:** `outcome` (`clean`, `needs_listen` or `null`) and review items (part, text, issues, reasons, start, end, checked_by) compared with the golden copy. This includes a take-check flag and a script-check (`&39;`) flag.
- **Search:** by words, title, status (`needs_review`, `cancelled`), voice and `same_script`.
- **Timings and subtitles:** srt, vtt and txt match the golden copy exactly. The timings cover speed 1.25, a part with measured word times, a misheard word, a part with estimated times, and a 404 before rendering.
- **Audio:** the final WAV, the download filename, part audio, take 1 and an unknown take. Missing files return 404.
- **Review actions:**
  - "Sounds fine" and its undo.
  - Refusals: 400 for a part that passed or has no audio, 404 for an unknown part.
  - select-take for an unknown take returns 404.
  - fix-remaining with nothing to fix returns 400.
  - Soft delete and restore, with files kept.
  - Title and favourite edits.
- **Creating narrations:** an unknown voice returns 404, and an empty script after cleaning returns 400 with nothing queued.
- **Safety guards:**
  - A foreign `Host` returns 403.
  - A foreign `Origin` on writes returns 403, and the narration survives.
  - Writes without an `Origin` (MCP, curl) still work.
  - Ids or paths containing `..` or an encoded `/` are refused with 4xx.
- **MCP tools against the engine:** `list_voices`, `get_status` (review_status and parts_needing_review), `get_timings`, `get_subtitles`, `find_narration`, `get_audio` (to a temporary path) and `mark_sounds_fine` with its undo.

## Goldens without an HTTP route

- **Text cleaning:** observed black-box only for the fixture scripts, through `search?same_script=`. The other cases in `golden/clean_text.json` (entities, citations, dashes, markdown, URLs, pronunciations, unwrapping hard-wrapped lines) are for Rust unit tests.
- **Chunking:** `golden/chunk_text.json` has no route that exposes chunking without rendering. It is for Rust unit tests.

## Known gaps

The following need a render or a busy engine, so they can't be reached black-box without models:

- **Duplicate refusal** (409 with `duplicate_of`) needs a narration queued or rendering. The engine also rewrites `queued` and `running` to `interrupted` at startup, so it can't be seeded. `backend/tests/test_review_loop.py` and `test_mcp_tools.py` cover it in-process.
- **The busy 409** (accept, regenerate or resume while rendering) and **select-take of a kept take**: the second needs a remix.
- **Successful creating, rendering, re-speaking, fix-remaining, speed changes and voice analysis.**
- **MP3 audio:** the fixture has WAV only, and `?fmt=mp3` is not checked.
- **Transcripts and voice design:** their routes need models.
- **`/reveal`:** it opens the file manager.
- **Updates and engine downloads:** they reach the network.

Re-record the goldens only after an intended behaviour change: run `backend/.venv/bin/python -B contract/record_golden.py` and review the diff.
