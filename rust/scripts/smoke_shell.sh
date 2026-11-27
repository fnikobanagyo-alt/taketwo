#!/usr/bin/env bash
# Gate G3 (rust/PLAN.md S09, lead decision 10): the TakeTwo shell starts its own engine, shows the UI, and
# takes the engine with it when it goes. Prints "G3 PASS" as its last line and exits 0 only if every check holds.
# Builds taketwo and taketwo-engine (debug), builds frontend/dist only when it is missing, seeds a fresh
# temporary library (TAKETWO_DATA) and picks a free dev port (8790-8795, 8798, 8799; never 8765-8785).
# It never starts `taketwo-engine serve` itself: every engine here is the child of a shell. Every shell runs
# hidden (TAKETWO_SHELL_HIDDEN=1) without TAKETWO_URL / VOICEFORGE_URL. The trap stops only pids it started.
set -uo pipefail

WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DATA="$(mktemp -d "${TMPDIR:-/tmp}/taketwo-shell.XXXXXX")"
mkdir "$DATA/library"
export TAKETWO_DATA="$DATA/library"
source "$WT/rust/scripts/env.sh" >/dev/null   # keeps this TAKETWO_DATA
LIB="$TAKETWO_DATA"
# The shells' engines get an empty models folder: this smoke never loads a model.
export TAKETWO_MODELS="$DATA/no-models"
mkdir "$TAKETWO_MODELS"
PIDS=()
BUILT_UI=""
UI="$WT/frontend/dist"

cleanup() {
    for p in ${PIDS[@]+"${PIDS[@]}"}; do
        # Only while the pid is still one of our programs: a finished pid may since belong to someone else.
        ps -o command= -p "$p" 2>/dev/null | grep -q 'rust-taketwo/rust/target/' || continue
        kill -9 "$p" 2>/dev/null
        wait "$p" 2>/dev/null
    done
    rm -rf "$DATA"
    # A UI built here goes again: record_http.py's Python engine serves frontend/dist when it exists.
    [ -n "$BUILT_UI" ] && rm -rf "$UI"
}
trap cleanup EXIT INT TERM

fail() { echo "G3 FAIL: $*"; exit 1; }
health() { curl -s --max-time 2 "http://127.0.0.1:$1/api/health"; }
listening() { lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1; }
engine_pid() { sed -n 's/.*"pid": \([0-9]*\).*/\1/p' "$LIB/engine.json" 2>/dev/null; }
# wait_for SECONDS COMMAND...: true once COMMAND succeeds, polling every 0.2 s.
wait_for() {
    local n=$(( $1 * 5 )); shift
    for _ in $(seq 1 "$n"); do "$@" && return 0; sleep 0.2; done
    return 1
}
gone() { ! kill -0 "$1" 2>/dev/null; }
port_free() { ! listening "$1"; }
answers() { health "$1" | grep -q '"name":"TakeTwo"'; }
exited() { ! kill -0 "$1" 2>/dev/null; }
# start_shell ERRFILE [ENV=VALUE...] -- ARGS...: one hidden shell in the background; sets SHELL_PID.
start_shell() {
    local err="$1"; shift
    local envs=()
    while [ "$1" != "--" ]; do envs+=("$1"); shift; done
    shift
    env -u TAKETWO_URL -u VOICEFORGE_URL TAKETWO_SHELL_HIDDEN=1 ${envs[@]+"${envs[@]}"} "$SHELL_BIN" "$@" 2>"$err" &
    SHELL_PID=$!
    PIDS+=("$SHELL_PID")
}

(cd "$WT/rust" && cargo build -q -p taketwo -p taketwo-engine) || fail "build"
SHELL_BIN="$WT/rust/target/debug/taketwo"
ENGINE_BIN="$WT/rust/target/debug/taketwo-engine"
if [ ! -f "$UI/index.html" ]; then
    # --configLoader runner: vite reads its config without writing a temp file into the symlinked node_modules.
    (cd "$WT/frontend" && npm run build -- --configLoader runner >/dev/null) || fail "npm run build"
    BUILT_UI=1
fi
"$ENGINE_BIN" seed --fixture "$WT/contract/fixture.json" >/dev/null || fail "seed"

PORT=""
for p in 8790 8791 8792 8793 8794 8795 8798 8799; do
    listening "$p" || { PORT="$p"; break; }
done
[ -n "$PORT" ] || fail "no free dev port"
echo "library $LIB, dev port $PORT"

MARK="$DATA/home-marker"
touch "$MARK"
sleep 1   # so anything written after this point is strictly newer than the marker

# g. Refusals: nothing starts.
env -u TAKETWO_DATA -u TAKETWO_URL -u VOICEFORGE_URL TAKETWO_SHELL_HIDDEN=1 "$SHELL_BIN" --port "$PORT" --ui "$UI" 2>"$DATA/g1.err"
rc=$?
[ $rc -eq 2 ] || fail "g: without TAKETWO_DATA the shell exited $rc, not 2"
grep -q 'refusing to start' "$DATA/g1.err" || fail "g: no refusal wording: $(cat "$DATA/g1.err")"
env -u TAKETWO_UI -u TAKETWO_URL -u VOICEFORGE_URL TAKETWO_SHELL_HIDDEN=1 "$SHELL_BIN" --port "$PORT" 2>"$DATA/g2.err"
rc=$?
[ $rc -eq 2 ] || fail "g: without --ui the shell exited $rc, not 2"
grep -q 'Interface not built' "$DATA/g2.err" || fail "g: no 'Interface not built': $(cat "$DATA/g2.err")"
listening "$PORT" && fail "g: something listens on $PORT after a refusal"
[ -e "$LIB/engine.json" ] || [ -e "$LIB/engine.log" ] && fail "g: a refused shell started an engine"
echo "g ok: refusals exit 2 and start nothing"

# Run 1 ends by TAKETWO_SHELL_CLOSE_AFTER_MS, set from its start with room for checks a-d.
CLOSE_MS=20000
start_shell "$DATA/run1.err" TAKETWO_SHELL_CLOSE_AFTER_MS=$CLOSE_MS -- --port "$PORT" --ui "$UI"
S1=$SHELL_PID
wait_for 90 answers "$PORT" || fail "a: no TakeTwo health on $PORT; shell said: $(cat "$DATA/run1.err")"

# a. Health, the page, one asset, the fixture narrations.
h="$(health "$PORT")"
echo "$h" | grep -q '"formerly":"VoiceForge"' || fail "a: health lacks formerly VoiceForge: $h"
page="$(curl -s --max-time 5 "http://127.0.0.1:$PORT/")"
echo "$page" | grep -q '<div id="root"' || fail "a: GET / has no <div id=\"root\""
asset="$(echo "$page" | grep -o '/assets/[^"]*\.js' | head -1)"
[ -n "$asset" ] || fail "a: GET / names no /assets/*.js"
code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "http://127.0.0.1:$PORT$asset")"
[ "$code" = 200 ] || fail "a: $asset answered $code"
gens="$(curl -s --max-time 5 "http://127.0.0.1:$PORT/api/generations")"
for id in $(grep -o '"id": "ct[a-z0-9]*"' "$WT/contract/fixture.json" | sed 's/.*"\(ct[a-z0-9]*\)"/\1/' | grep -v '^ctvoice'); do
    echo "$gens" | grep -q "\"$id\"" || fail "a: /api/generations lacks fixture narration $id"
done
echo "a ok: health, page, $asset 200, fixture narrations listed"

# b. The engine is the shell's child, started with --exit-with-stdin.
E1="$(engine_pid)"
[ -n "$E1" ] || fail "b: no pid in engine.json"
PIDS+=("$E1")
ppid="$(ps -o ppid= -p "$E1" | tr -d ' ')"
[ "$ppid" = "$S1" ] || fail "b: engine $E1 has parent $ppid, not the shell $S1"
ps -o command= -p "$E1" | grep -q -- '--exit-with-stdin' || fail "b: engine command line lacks --exit-with-stdin"
echo "b ok: engine $E1 is the child of shell $S1"

# c. The window really loaded the UI.
wait_for 30 grep -q "{\"page\":\"loaded\",\"url\":\"http://127.0.0.1:$PORT/" "$DATA/run1.err" \
    || fail "c: no page-loaded line; shell said: $(cat "$DATA/run1.err")"
echo "c ok: $(grep -m1 '"page":"loaded"' "$DATA/run1.err")"

# d. A second shell on the same library is refused and leaves the first alone.
start_shell "$DATA/second.err" -- --ui "$UI"
S2=$SHELL_PID
wait_for 15 exited "$S2" || fail "d: the second shell still runs after 15 s"
wait "$S2"; rc=$?
[ $rc -ne 0 ] || fail "d: the second shell exited 0"
grep -q 'another TakeTwo engine is running for this library' "$DATA/second.err" \
    || fail "d: the second shell did not print the engine's refusal: $(cat "$DATA/second.err")"
answers "$PORT" || fail "d: the first shell's engine stopped answering"
kill -0 "$S1" 2>/dev/null || fail "d: the first shell is gone before its close time (checks a-d took too long)"
echo "d ok: second shell exited $rc with the engine's refusal; the first still answers"

# e. The window closes by itself after CLOSE_MS: shell exits 0, engine gone, port free, address gone, lock released.
wait_for 60 exited "$S1" || fail "e: the shell did not exit after its close time"
wait "$S1"; rc=$?
[ $rc -eq 0 ] || fail "e: the shell exited $rc, not 0; it said: $(cat "$DATA/run1.err")"
wait_for 10 gone "$E1" || fail "e: engine $E1 still runs 10 s after the shell exited"
wait_for 10 port_free "$PORT" || fail "e: port $PORT still listening"
[ -e "$LIB/engine.json" ] && fail "e: engine.json left behind"
# The lock file stays (lead decision 11); it must be released, so another engine on this library can take it.
python3 -c 'import fcntl, sys; f = open(sys.argv[1], "a"); fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)' "$LIB/engine.lock" 2>/dev/null \
    || fail "e: engine.lock is still held after the engine exited"
echo "e ok: shell exited 0 by itself; engine gone, port free, engine.json gone, engine.lock released"

# f. Run 2: kill -9 the shell; the engine follows (its standard input closes).
start_shell "$DATA/run2.err" -- --port "$PORT" --ui "$UI"
S3=$SHELL_PID
wait_for 90 answers "$PORT" || fail "f: no health in run 2; shell said: $(cat "$DATA/run2.err")"
E2="$(engine_pid)"
[ -n "$E2" ] || fail "f: no pid in engine.json"
PIDS+=("$E2")
kill -9 "$S3"
wait "$S3" 2>/dev/null
wait_for 10 gone "$E2" || fail "f: engine $E2 still runs 10 s after kill -9 of the shell"
wait_for 10 port_free "$PORT" || fail "f: port $PORT still listening"
echo "f ok: after kill -9 of shell $S3, engine $E2 gone and port free"

# h. Nothing of ours is left.
for p in 8790 8791 8792 8793 8794 8795 8798 8799; do
    listening "$p" && fail "h: something listens on dev port $p"
done
left="$(pgrep -fl 'rust-taketwo/rust/target/')"
[ -z "$left" ] || fail "h: processes left: $left"
echo "h ok: no dev port listening, no rust/target process"

# i. Home folder report (names and mtimes only; no file is read). Reported, not judged. Claude Code's own
# MCP log folders (Caches/claude-cli-nodejs/*/mcp-logs-taketwo, written by other sessions) are left out.
echo "HOME WRITES:"
for d in Library/WebKit Library/Caches Library/HTTPStorages; do
    [ -d "$HOME/$d" ] || continue
    (cd "$HOME" && find "$d" -maxdepth 3 -iname '*taketwo*' -not -path '*/claude-cli-nodejs/*' -newer "$MARK" -exec stat -f '  %Sm  ~/%N' {} + 2>/dev/null)
done
echo "(end of HOME WRITES)"

trap - EXIT INT TERM
cleanup
echo "G3 PASS"
