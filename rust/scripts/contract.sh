#!/usr/bin/env bash
# Run the black-box contract suite (contract/test_contract.py) against the Rust engine.
#   rust/scripts/contract.sh            all 33 tests; exit 0 only with "Ran 33 tests", "OK" and no skips
#   rust/scripts/contract.sh -k Health  arguments go to unittest; its exit code is returned
# Builds taketwo-engine, seeds a fresh temporary library (TAKETWO_DATA), starts `serve` on a free dev port
# (8790-8795, 8798, 8799; never 8765-8785), runs the suite, then stops that engine by its own pid and
# deletes the library. It never touches a process it did not start.
set -uo pipefail

WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$WT/rust/scripts/env.sh" >/dev/null   # makes its own temp TAKETWO_DATA; removed below
ENV_DATA="$TAKETWO_DATA"
DATA="$(mktemp -d "${TMPDIR:-/tmp}/taketwo-contract.XXXXXX")"
LOG="$DATA.log"
PID=""

cleanup() {
    if [ -n "$PID" ]; then
        kill "$PID" 2>/dev/null
        wait "$PID" 2>/dev/null
    fi
    rm -rf "$DATA" "$ENV_DATA" "$LOG"
}
trap cleanup EXIT INT TERM

(cd "$WT/rust" && cargo build -q -p taketwo-engine) || exit 2
BIN="$WT/rust/target/debug/taketwo-engine"
export TAKETWO_DATA="$DATA/library"
"$BIN" seed --fixture "$WT/contract/fixture.json" >/dev/null || exit 2

"$BIN" serve >"$LOG" 2>&1 &
PID=$!
PORT=""
for _ in $(seq 1 300); do   # 30 s
    PORT="$(sed -n 's/^{"ready":true,"port":\([0-9]*\),.*/\1/p' "$LOG" | head -1)"
    [ -n "$PORT" ] && break
    kill -0 "$PID" 2>/dev/null || { echo "engine exited before it was ready:"; cat "$LOG"; exit 2; }
    sleep 0.1
done
[ -n "$PORT" ] || { echo "engine not ready after 30 s:"; cat "$LOG"; exit 2; }
echo "Rust engine ready on 127.0.0.1:$PORT (pid $PID)"

OUT="$DATA/unittest.txt"
cd "$WT"
TAKETWO_URL="http://127.0.0.1:$PORT" python3 -m unittest discover -s "$WT/contract" -t "$WT/contract" "$@" >"$OUT" 2>&1
rc=$?
cat "$OUT"
if [ $# -gt 0 ]; then
    exit $rc
fi
grep -q '^Ran 33 tests' "$OUT" && grep -q '^OK$' "$OUT" && ! grep -q 'skipped' "$OUT" && [ $rc -eq 0 ]
