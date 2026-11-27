# Source this (bash or zsh) before working on the Rust port:  source rust/scripts/env.sh
# It puts rust/.tools/bin (cmake) on PATH, points TAKETWO_MODELS at rust/.models, and keeps the caller's
# TAKETWO_DATA folder or makes a fresh temporary one, so every program runs in isolated mode. It never passes --live.
# No `set -e`, `exit` or `cd` here: it runs inside the caller's shell.

_tt_rust="$(dirname "$(dirname "${BASH_SOURCE[0]:-$0}")")"
_tt_rust="$(cd "$_tt_rust" && pwd)"

export PATH="$_tt_rust/.tools/bin:$PATH"

# No incremental cache: it filled the disk (lead decision 11).
export CARGO_INCREMENTAL=0

export TAKETWO_MODELS="$_tt_rust/.models"
mkdir -p "$TAKETWO_MODELS"
# Read-only use of the converted model; nothing here writes into it.
_tt_qwen="$HOME/Library/Application Support/TakeTwo/models/qwen3-tts-1.7b-base"
if [ -d "$_tt_qwen" ] && [ ! -e "$TAKETWO_MODELS/qwen3-tts-1.7b-base" ]; then
    ln -s "$_tt_qwen" "$TAKETWO_MODELS/qwen3-tts-1.7b-base"
fi
# The speaker encoder (S14), read in place from the Hugging Face cache; `find`, not a glob (zsh errors on no match).
_tt_ve="$(find "$HOME/.cache/huggingface/hub/models--ResembleAI--chatterbox/snapshots" -name ve.safetensors 2>/dev/null | head -1)"
if [ -n "$_tt_ve" ] && [ ! -e "$TAKETWO_MODELS/voice-encoder/ve.safetensors" ]; then
    mkdir -p "$TAKETWO_MODELS/voice-encoder"
    ln -s "$_tt_ve" "$TAKETWO_MODELS/voice-encoder/ve.safetensors"
fi

# Studio restoration (S18): the two Sidon fp16 ONNX files, read in place from the Hugging Face cache, the same way.
for _tt_f in sidon-predictor.onnx sidon-vocoder.onnx; do
    _tt_sid="$(find "$HOME/.cache/huggingface/hub/models--soniqo--Sidon-ONNX/snapshots" -path "*/fp16/$_tt_f" 2>/dev/null | head -1)"
    if [ -n "$_tt_sid" ] && [ ! -e "$TAKETWO_MODELS/sidon/$_tt_f" ]; then
        mkdir -p "$TAKETWO_MODELS/sidon"
        ln -s "$_tt_sid" "$TAKETWO_MODELS/sidon/$_tt_f"
    fi
done

# Keep the caller's TAKETWO_DATA when it names an existing folder; otherwise make a new temporary one.
if [ -z "${TAKETWO_DATA:-}" ] || [ ! -d "$TAKETWO_DATA" ]; then
    _tt_tmp="${TMPDIR:-/tmp}"
    TAKETWO_DATA="$(mktemp -d "${_tt_tmp%/}/taketwo-data.XXXXXX")"
fi
export TAKETWO_DATA

unset _tt_rust _tt_qwen _tt_tmp _tt_ve _tt_f _tt_sid
echo "TakeTwo dev env: isolated mode, TAKETWO_DATA=$TAKETWO_DATA, TAKETWO_MODELS=$TAKETWO_MODELS"
