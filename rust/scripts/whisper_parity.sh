#!/usr/bin/env bash
# Whisper parity run (step S13): the Python app's ml.transcribe against the Rust whisper.cpp
# transcriber on four clips made with macOS `say`. Writes rust/out/whisper/{python,parity}.json;
# rust/scripts/whisper_report.py reads them. Numbers only: no tolerance is judged here.
#   rust/scripts/whisper_parity.sh [--features metal]      (dev profile; whisper.cpp is built Release)
# Exit 75: Franz's app stayed busy for 10 minutes before a model process.
set -euo pipefail

: "${TMPDIR:=/tmp}"
WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$WT/rust"
source scripts/env.sh >/dev/null
FEATURES=()
GPU=0
if [ "${1:-}" = "--features" ] && [ "${2:-}" = "metal" ]; then FEATURES=(--features metal); GPU=1; fi

CLIPS="$(mktemp -d "${TMPDIR%/}/taketwo-whisper-clips.XXXXXX")"
PYDATA="$(mktemp -d "${TMPDIR%/}/taketwo-whisper-data.XXXXXX")"
trap 'rm -rf "$CLIPS" "$PYDATA"' EXIT
mkdir -p out/whisper

# Made-up texts. Clip 4 crosses the 30 s window (report only).
say -v Samantha --data-format=LEI16@16000 -o "$CLIPS/clip1.wav" \
    "The little wooden boat drifted slowly across the quiet lake at dawn."
say -v Samantha --data-format=LEI16@16000 -o "$CLIPS/clip2.wav" \
    "Our train leaves at 7 in the morning, so please pack your bags tonight before you go to bed. Bring the blue map, a warm coat and 3 cheese sandwiches for the long journey north. Do you think it will rain when we arrive?"
say -v Samantha --data-format=LEI16@16000 -o "$CLIPS/clip3.wav" \
    "Dr. Rivera opened the old workshop at the end of the lane. Inside, a well-worn bench held jars of screws, coils of wire and a clock that had stopped years ago. [[slnc 800]] She wound it carefully, set the hands to noon, and after a long moment it began to tick again. Outside, the evening light turned the windows gold, and somewhere down the road a dog barked twice."
say -v Samantha --data-format=LEI16@16000 -o "$CLIPS/clip4.wav" \
    "Every autumn the village held a market in the square. Farmers brought apples, pears and round orange pumpkins, and the baker sold warm bread straight from the oven. Children ran between the stalls while a fiddler played near the fountain. By evening the lamps were lit, the last carts rolled home along the river road, and the square was quiet again until the next year. In winter the same square filled with snow. The children built forts near the fountain and threw snowballs at anyone who walked past, while their parents drank hot cider under the bare trees. On the coldest nights the river froze from bank to bank. When the thaw finally came, the ice cracked with a sound like distant thunder, and everyone gathered on the bridge to watch it break apart and float away."
for c in clip1 clip2 clip3 clip4; do
    printf '%s: %s s\n' "$c" "$(afinfo "$CLIPS/$c.wav" | awk '/estimated duration/ {print $3}')"
done

app_idle() { # Franz's app: read-only GET of /api/status only
    local s
    for _ in $(seq 60); do
        s="$(curl -s --max-time 5 http://127.0.0.1:8766/api/status || true)"
        if grep -q '"running":null' <<<"$s" && grep -q '"queued":0' <<<"$s"; then return 0; fi
        echo "Franz's app is busy; waiting 10 s"
        sleep 10
    done
    echo "Franz's app stayed busy for 10 minutes; stopping"
    exit 75
}

# Build first, so compiling is not inside the model run.
cargo test -p taketwo-voice ${FEATURES[@]+"${FEATURES[@]}"} --test whisper_parity --no-run -q

app_idle
TAKETWO_DATA="$PYDATA" VOICEFORGE_DATA="$PYDATA" HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 \
    "$WT/backend/.venv/bin/python" -B scripts/whisper_python_ref.py "$CLIPS" out/whisper/python.json

app_idle
WHISPER_PARITY_CLIPS="$CLIPS" WHISPER_PARITY_GPU="$GPU" \
    cargo test -p taketwo-voice ${FEATURES[@]+"${FEATURES[@]}"} --test whisper_parity -- --ignored whisper_parity --nocapture
df -g . | tail -1
