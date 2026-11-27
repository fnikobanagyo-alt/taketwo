# Blind A/B harness

`ab.py` compares today's Python app with the Rust engine on the same 10 of Franz's finished narrations, as a blind listening page. Python standard library only. Everything it stores goes under the git-ignored `rust/ab/private/` (or `--out DIR`): the scripts, the rendered audio, the page and the key. Nothing private is printed: standard output has counts and ordinals only, never a title, script text, voice name or id.

It talks to the source app (Franz's running app unless `--source URL`) through one function, `source_get`, which sends GET only.

| Command | What it does |
|---|---|
| `python3 rust/ab/ab.py plan [--refresh]` | Reads the source's narration list, each candidate's detail and the voice list. Picks 10 scripts by the rule below and saves them in `private/scripts.json`. Prints the counts and the take bound. A second `plan` reuses the saved file (no request, no rewrite) unless `--refresh`. |
| `python3 rust/ab/ab.py render --franz-ok N` | Refuses (exit 3) without Franz's OK or with a number that is not the planned bound. With it: builds a temp library with only the planned voices (each profile document and its files, copied by GET), written with the Python app's own `db` module; gives each engine its own copy; runs the Python engine, then the Rust engine, one at a time on dev ports 8790-8795, 8798, 8799, never `--live`; renders the 10 scripts through the ordinary HTTP API with each script's stored language, speed, params, voice settings and settings; keeps WAV and SRT under `private/render/a` and `private/render/b`, with wall time, takes used, outcome and review count in `private/render.json`; deletes the temp libraries. Stops if the Rust engine answers 501 for rendering. |
| `python3 rust/ab/ab.py page` | Builds `private/page.html`, self-contained (audio embedded): per script two players, A and B, in a random order drawn per script; a choice (A, B, no difference), a notes box and a "copy my answers" button. The key is written to `private/key.json` and is nowhere in the page. |
| `python3 rust/ab/ab.py selftest` | No model and no request to Franz's app. Seeds the contract fixture into a temp library, serves it with a stub-renderer Rust engine as the source, then runs `plan --selftest-rule`, `render` (both engines are stub-renderer Rust engines) and `page` into a temp folder, and checks the result. Prints `AB SELFTEST PASS`. |
| `python3 rust/ab/check_private.py` | Fails if a title, a 40-character run of a script or a voice name from `private/scripts.json` occurs in `rust/PROGRESS.md`, this README or `ab.py`. |

## The rule

Applied in this order (the docstring of `ab.py` is the reference): status `done`; engine `qwen3-tts-1.7b`; outcome `clean`; an ordinary narration (every narration is: re-speaks and voice previews never make one of their own, so there is no value to filter on); its voice still exists and is `ready`; 2 to 5 parts; no two with the same script after lower-casing and squeezing spaces; newest first, ties by id. Then 3 scripts with 2 parts, 4 with 3 parts, 3 with 4-5 parts; voices take turns within a bucket, at most 5 voices in all; a short bucket is filled from the nearest one, and `plan` says so.

`--selftest-rule` exists only for the fixture (4 finished narrations, 1 voice): it drops the outcome filter, allows 1 part and accepts fewer than 10 scripts.

## The take bound

Takes are synthesis calls. Per part: `max_attempts + compare_takes - 1` takes, and the final check may record the part once more, so twice that. Per voice without a preview file: 3 more, because the Python engine makes one at startup. Two engines double the sum. Each engine runs with `TAKETWO_MAX_TAKES` set to its half. The Python engine has no such cap, so `render` stops when a script is split into more parts than planned. The bound has no open-ended term while the ending guard setting is off; it is off by default in a fresh library, and `render` checks it before each script.

## Rendering waits for Franz

`render` is never run without Franz's OK. His OK has to cover two things:

1. **The take count**, far over the 20-take limit per experiment: `plan` prints it (`takes, most: N`) and `render --franz-ok N` needs exactly that number.
2. **Copying reference clips of several voices into temp folders**: the profile documents and voice files (reference clips, original recording, preview, fingerprint; `plan` prints the count, an upper bound because fixed-name files a voice lacks are skipped), read by GET from his app into temporary libraries that are deleted after the run. Nothing is written to his app or his library.
