#!/bin/zsh
# TakeTwo (formerly VoiceForge) launcher: checks this Mac, sets up on first run, then starts at the first free port from 8766 (up to 8785).
#   ./start.sh             set up if needed and open the app
#   ./start.sh --no-open   the same, without opening the browser
#   ./start.sh --check     only check the requirements
#   ./start.sh --setup     only set up (used before building the desktop app)
set -e
cd "$(dirname "$0")"
export PATH="${TAKETWO_PATH:-${VOICEFORGE_PATH:-$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH}}"
fail() { echo "TakeTwo can't start: $1" >&2; exit 1 }

# --- requirements, before anything is downloaded
[[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]] || fail "it needs a Mac with Apple Silicon (M1 or newer)."
(( $(sw_vers -productVersion | cut -d. -f1) >= 14 )) || fail "it needs macOS 14 or newer."
PY=$(command -v python3.11) || fail "Python 3.11 is missing. Run: brew install python@3.11"  # the version TakeTwo is tested with
{ command -v ffmpeg && command -v ffprobe } >/dev/null || fail "ffmpeg is missing. Run: brew install ffmpeg"
{ command -v node && command -v npm } >/dev/null || fail "Node.js is missing. Run: brew install node"
NODE=(${(s:.:)$(node --version | tr -d v)})
(( NODE[1] >= 23 || (NODE[1] == 22 && NODE[2] >= 12) || (NODE[1] == 20 && NODE[2] >= 19) )) || fail "Node.js $(node --version) is too old. Run: brew upgrade node"
[[ "$1" == "--check" ]] && { echo "All requirements found."; exit 0 }

# --- Python packages: reinstalled if the list changed or an earlier install didn't finish
STAMP=backend/.venv/.installed
WANT=$(shasum -a 256 backend/requirements.lock | cut -d' ' -f1)
if [[ ! -x backend/.venv/bin/python || "$(cat $STAMP 2>/dev/null)" != "$WANT" ]]; then
  echo "Installing the voice engine (about 1 GB the first time)…"
  [[ -x backend/.venv/bin/python ]] || "$PY" -m venv backend/.venv
  rm -f "$STAMP"
  backend/.venv/bin/python -m pip install -q --disable-pip-version-check -r backend/requirements.lock || fail "the Python packages didn't install. Check your connection, then run start.sh again."
  echo "$WANT" > "$STAMP"
fi

# --- interface: rebuilt when its source is newer
if [[ ! -f frontend/dist/index.html || -n "$(find frontend/src frontend/index.html frontend/package-lock.json -newer frontend/dist/index.html -print -quit)" ]]; then
  echo "Building the interface…"
  (cd frontend && { [[ -d node_modules && node_modules -nt package-lock.json ]] || npm ci --silent --include=optional } && npm run --silent build >/dev/null) || fail "the interface didn't build. Run: cd frontend && npm ci && npm run build"
  touch frontend/dist/index.html
fi
[[ "$1" == "--setup" ]] && { echo "Setup finished."; exit 0 }

TAKETWO_PORT=${TAKETWO_PORT:-$VOICEFORGE_PORT}
if [[ -z "$TAKETWO_PORT" ]]; then
  for p in {8766..8785}; do
    lsof -nP -iTCP:$p -sTCP:LISTEN >/dev/null 2>&1 || { TAKETWO_PORT=$p; break; }
  done
fi
PORT=${TAKETWO_PORT:?No free port from 8766 to 8785}
echo "TakeTwo: http://127.0.0.1:$PORT (the first start takes up to a minute)"
cd backend
if [[ "$1" != "--no-open" ]]; then
  (for i in {1..120}; do curl -s -m 1 "http://127.0.0.1:$PORT/api/health" >/dev/null && { open "http://127.0.0.1:$PORT"; break; }; sleep 1; done) &
fi
exec .venv/bin/python -m uvicorn app.main:app --host 127.0.0.1 --port "$PORT" --log-level warning
