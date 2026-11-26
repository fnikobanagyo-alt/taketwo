# Contributing to TakeTwo

Thanks for helping. TakeTwo is a local voice-cloning narration studio; a few rules keep it safe and honest.

## Ground rules
- **Consent.** Only use voices you own or have permission to clone, in code, tests and issues alike.
- **Your library is sacred.** Never write tests or tools that touch a real `data/` folder. Point
  `TAKETWO_DATA` at a temporary folder (the checks below do this for you).
- **Audio claims need ears.** The automated checks can't hear. If you change how narration sounds, say what
  you listened to; "tests pass" is not proof that audio is better.
- **Words.** Use the terms in [GLOSSARY.md](GLOSSARY.md) ("part", "take", "re-speak", "needs review").

## Set up
You need an Apple Silicon Mac with macOS 14+, Python 3.11, ffmpeg and Node 22 (see the README).

```bash
git clone https://github.com/fnikobanagyo-alt/taketwo.git
cd taketwo
./start.sh --setup
```

`--setup` installs the Python packages and builds the interface without downloading any models.

## Check your change
```bash
backend/.venv/bin/python -B scripts/check.py
```

It runs the backend tests on a temporary library, type-checks and builds the interface. GitHub runs the same
command on every pull request. For a live interface while you work: start the engine with `./start.sh`, then
`cd frontend && npm run dev`.

## Where things live
- `backend/app/`: the FastAPI engine. `pipeline.py` renders and checks narrations; `main.py` is the HTTP API.
- `backend/tests/`: unit tests (no models needed).
- `frontend/src/`: the React interface.
- `desktop/`: the Electron shell that starts the engine and opens the window.
- `mcp/taketwo_mcp.py`: the MCP server for Claude and other AI apps.

## Pull requests
Keep each one to one change, describe how you checked it, and add a test for any bug you fix. Note anything
user-facing in [CHANGELOG.md](CHANGELOG.md) under the next version.
