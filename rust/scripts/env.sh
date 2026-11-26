# Source this (bash or zsh) before working on the Rust port:  source rust/scripts/env.sh
# It puts rust/.tools/bin (cmake) on PATH, points TAKETWO_MODELS at rust/.models, and makes a fresh
# temporary TAKETWO_DATA, so every program runs in isolated mode. It never passes --live.
# No `set -e`, `exit` or `cd` here: it runs inside the caller's shell.

_tt_rust="$(dirname "$(dirname "${BASH_SOURCE[0]:-$0}")")"
_tt_rust="$(cd "$_tt_rust" && pwd)"

export PATH="$_tt_rust/.tools/bin:$PATH"

export TAKETWO_MODELS="$_tt_rust/.models"
mkdir -p "$TAKETWO_MODELS"
# Read-only use of the converted model; nothing here writes into it.
_tt_qwen="$HOME/Library/Application Support/TakeTwo/models/qwen3-tts-1.7b-base"
if [ -d "$_tt_qwen" ] && [ ! -e "$TAKETWO_MODELS/qwen3-tts-1.7b-base" ]; then
    ln -s "$_tt_qwen" "$TAKETWO_MODELS/qwen3-tts-1.7b-base"
fi

_tt_tmp="${TMPDIR:-/tmp}"
TAKETWO_DATA="$(mktemp -d "${_tt_tmp%/}/taketwo-data.XXXXXX")"
export TAKETWO_DATA

unset _tt_rust _tt_qwen _tt_tmp
echo "TakeTwo dev env: isolated mode, TAKETWO_DATA=$TAKETWO_DATA, TAKETWO_MODELS=$TAKETWO_MODELS"
