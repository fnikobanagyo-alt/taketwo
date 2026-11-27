#!/usr/bin/env bash
# Gate G5 (rust/PLAN.md S19, lead decision 21): one real synthesis through the Rust engine, in isolated mode.
#   rust/scripts/synth_smoke.sh
# 1. Franz's app on 8766 must be idle (else exit 75, "busy, try later"). Only GETs ever go to 8766: /api/status,
#    /api/profiles and ONE reference clip of the first ready English voice, through source_get below.
# 2. The release engine (built with --features metal) starts on a dev port with TAKETWO_DATA under
#    rust/target/smoke-tmp (new, empty), TAKETWO_MAX_TAKES=10, under /usr/bin/time -l; stderr to engine.log.
# 3. It makes a voice from the clip (studio requested: Sidon runs when its files are there), waits for its analysis and preview, renders
#    one fixed narration, saves WAV, SRT, timings and the narration document in rust/out/smoke/, stops the engine
#    and deletes rust/target/smoke-tmp (an EXIT trap does both on every path) and writes report.json.
# At most two attempts and 20 takes over all attempts (attempts.json keeps the count; delete rust/out/smoke to
# start a new experiment). Exit 0: WAV over 1 s and SRT exist and this attempt used <= 10 takes.
# Exit 1: an error (the engine's error text is printed). Exit 75: Franz's app was busy.
# Dry run without a model (not an attempt of the experiment): SMOKE_OUT=<dir> SMOKE_CLIP=<wav>
#   SMOKE_ENGINE=rust/target/debug/taketwo-engine SMOKE_ENGINE_ARGS=--stub-renderer (SMOKE_CLIP skips the two
#   profile reads of 8766; the status read stays).
# No voice name, profile id or file name of Franz's is printed or written anywhere.
set -euo pipefail

RUST="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WT="$(dirname "$RUST")"
SOURCE=http://127.0.0.1:8766
OUT="${SMOKE_OUT:-$RUST/out/smoke}"
TMP="$RUST/target/smoke-tmp"
ENGINE="${SMOKE_ENGINE:-$RUST/target/release/taketwo-engine}"
ENGINE_ARGS="${SMOKE_ENGINE_ARGS:-}"
TAKE_CAP=10 TOTAL_CAP=20 MAX_ATTEMPTS=2
NARRATION_LIMIT=1200   # seconds
TEXT="The harbour was quiet before sunrise. A single boat moved slowly across the water, and the gulls followed it out. By noon the market was full of voices."
NOTE="no quality claim; Franz listens"
DEV_PORTS="8790 8791 8792 8793 8794 8795 8798 8799"

die() { echo "synth_smoke: $*" >&2; exit 1; }
for t in curl python3 ffmpeg /usr/bin/time; do command -v "$t" >/dev/null || die "$t is not on PATH"; done
[ -x "$ENGINE" ] || die "no engine at $ENGINE: cd rust && source scripts/env.sh && cargo build --release -p taketwo-engine --features metal"

# The only function that talks to Franz's app: GET, with a time limit.
source_get() { curl -sS -f -m "${2:-15}" -X GET "$SOURCE$1"; }
idle_or_75() {
    local s
    s="$(source_get /api/status 5)" || { echo "busy, try later (8766 did not answer /api/status)"; exit 75; }
    python3 -c 'import json,sys; s=json.loads(sys.argv[1]); sys.exit(0 if s.get("running") is None and s.get("queued")==0 else 1)' "$s" \
        || { echo "busy, try later (Franz's app is rendering or has a queue)"; exit 75; }
}
# Our engine only: the port is checked to be a dev port.
dev() { # method path [curl args...]
    local m="$1" p="$2"; shift 2
    case " $DEV_PORTS " in *" $PORT "*) ;; *) die "refusing a non-dev port $PORT";; esac
    curl -sS -m 60 -X "$m" "http://127.0.0.1:$PORT$p" "$@"
}
jget() { python3 -c 'import json,sys; v=json.load(sys.stdin)
for k in sys.argv[1:]: v=v[int(k)] if isinstance(v,list) else (v or {}).get(k)
print("" if v is None else v)' "$@"; }
takes_in_log() { local n; n="$(grep -cE '^take [0-9]+' "$OUT/engine.log" 2>/dev/null || true)"; echo "${n:-0}"; }

mkdir -p "$OUT"
LEDGER="$OUT/attempts.json"
read -r PREV_ATTEMPTS PREV_TAKES < <(python3 -c 'import json,sys,os
p=sys.argv[1]; a=json.load(open(p))["attempts"] if os.path.exists(p) else []
print(len(a), sum(x["takes"] for x in a))' "$LEDGER")
ATTEMPT=$((PREV_ATTEMPTS + 1))
[ "$ATTEMPT" -le "$MAX_ATTEMPTS" ] || die "already $PREV_ATTEMPTS attempts in $OUT (at most $MAX_ATTEMPTS); not starting"
CAP=$TAKE_CAP
[ $((TOTAL_CAP - PREV_TAKES)) -lt "$CAP" ] && CAP=$((TOTAL_CAP - PREV_TAKES))
[ "$CAP" -ge 1 ] || die "$PREV_TAKES takes already used of $TOTAL_CAP; not starting"

idle_or_75
[ -e "$TMP" ] && die "$TMP exists (left from a run that did not clean up); look at it and remove it first"

TIME_PID="" ENGINE_PID="" PORT="" STARTED=0 REPORTED=0
cleanup() {
    local code=$?
    set +e
    if [ -n "$ENGINE_PID" ] && kill -0 "$ENGINE_PID" 2>/dev/null; then
        kill -TERM "$ENGINE_PID"
        for _ in $(seq 1 60); do kill -0 "$ENGINE_PID" 2>/dev/null || break; sleep 0.5; done
        kill -0 "$ENGINE_PID" 2>/dev/null && kill -KILL "$ENGINE_PID"
    fi
    [ -n "$TIME_PID" ] && wait "$TIME_PID" 2>/dev/null
    if [ "$STARTED" = 1 ] && [ "$REPORTED" = 0 ]; then
        # a failed attempt still counts its takes
        python3 -c 'import json,sys,os
p,n,t=sys.argv[1],int(sys.argv[2]),int(sys.argv[3]); d=json.load(open(p)) if os.path.exists(p) else {"attempts":[]}
d["attempts"].append({"attempt":n,"takes":t,"result":"failed"}); json.dump(d,open(p,"w"),indent=1)' "$LEDGER" "$ATTEMPT" "$(takes_in_log)"
        [ -f "$OUT/engine.log" ] && cp "$OUT/engine.log" "$OUT/engine.attempt$ATTEMPT.log"
    fi
    rm -rf "$TMP"
    exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

mkdir -p "$TMP/clip" "$TMP/lib"
CLIP="$TMP/clip/reference.wav"
if [ -n "${SMOKE_CLIP:-}" ]; then
    cp "$SMOKE_CLIP" "$CLIP"
else
    # the first ready English voice; its active reference clip (a short WAV), never the long original
    PATH_ON_SOURCE="$(source_get /api/profiles 30 | python3 -c 'import json,sys,urllib.parse as u
for p in json.load(sys.stdin):
    v=p.get("variants") or []
    if p.get("status")=="ready" and (p.get("language") or "en")=="en" and v:
        a=p.get("active_variant") or 0
        f=(v[a] if 0<=a<len(v) else v[0]).get("file")
        if f:
            print("/api/profiles/%s/audio/%s" % (u.quote(p["id"],safe=""), u.quote(f,safe=""))); break')"
    [ -n "$PATH_ON_SOURCE" ] || die "no ready English voice with a reference clip in Franz's app"
    source_get "$PATH_ON_SOURCE" 60 > "$CLIP" || die "could not read the reference clip"
    unset PATH_ON_SOURCE
fi
CLIP_SECONDS="$(python3 -c 'import wave,sys; w=wave.open(sys.argv[1]); print(round(w.getnframes()/w.getframerate(),2))' "$CLIP")"

for p in $DEV_PORTS; do
    if python3 -c 'import socket,sys; s=socket.socket(); s.bind(("127.0.0.1",int(sys.argv[1])))' "$p" 2>/dev/null; then PORT=$p; break; fi
done
[ -n "$PORT" ] || die "no free dev port among $DEV_PORTS"

idle_or_75   # again, immediately before a model process may start
rm -f "$OUT/engine.log" "$OUT/narration.wav" "$OUT/narration.srt" "$OUT/timings.json" "$OUT/narration.json" "$OUT/report.json"
T0=$(python3 -c 'import time; print(time.time())')
# shellcheck disable=SC2086
env -u TAKETWO_URL -u VOICEFORGE_URL -u TAKETWO_STUB_GATE \
    TAKETWO_DATA="$TMP/lib" TAKETWO_MODELS="$RUST/.models" TAKETWO_MAX_TAKES="$CAP" \
    /usr/bin/time -l "$ENGINE" serve --port "$PORT" $ENGINE_ARGS </dev/null >"$TMP/engine.out" 2>"$OUT/engine.log" &
TIME_PID=$!
STARTED=1
for _ in $(seq 1 240); do
    ENGINE_PID="$(python3 -c 'import json,sys
for l in open(sys.argv[1]):
    if l.startswith("{"): print(json.loads(l).get("pid","")); break' "$TMP/engine.out" 2>/dev/null || true)"
    [ -n "$ENGINE_PID" ] && break
    kill -0 "$TIME_PID" 2>/dev/null || die "the engine exited before it was ready: $(tail -5 "$OUT/engine.log")"
    sleep 0.5
done
[ -n "$ENGINE_PID" ] || die "the engine was not ready in 120 s"
now() { python3 -c 'import time; print(time.time())'; }
since() { python3 -c 'import sys; print(round(float(sys.argv[2])-float(sys.argv[1]),1))' "$1" "$2"; }
T1=$(now)

wait_job() { # job id, limit seconds -> prints the final job document
    local jid="$1" limit="$2" start j st
    start=$(date +%s)
    while :; do
        j="$(dev GET "/api/jobs/$jid")"
        st="$(printf '%s' "$j" | jget status)"
        case "$st" in done|error|cancelled) printf '%s' "$j"; return 0;; esac
        [ $(( $(date +%s) - start )) -lt "$limit" ] || { printf '%s' "$j"; return 1; }
        sleep 2
    done
}
wait_idle() { # our engine: nothing running or queued
    local s
    for _ in $(seq 1 900); do
        s="$(dev GET /api/status)"
        python3 -c 'import json,sys; s=json.loads(sys.argv[1]); sys.exit(0 if s.get("running") is None and s.get("queued")==0 else 1)' "$s" && return 0
        sleep 2
    done
    return 1
}

# The voice: name "Smoke voice", English, defaults otherwise (studio is requested; without the Sidon files it falls back).
P="$(dev POST /api/profiles -F "file=@$CLIP;filename=reference.wav;type=audio/wav" --form-string "name=Smoke voice" --form-string "language=en")"
PID_="$(printf '%s' "$P" | jget id)"
AJOB="$(printf '%s' "$P" | jget job id)"
[ -n "$PID_" ] && [ -n "$AJOB" ] || die "creating the voice failed: $P"
A="$(wait_job "$AJOB" 900)" || die "the analysis did not end in 900 s"
T2=$(now)
[ "$(printf '%s' "$A" | jget status)" = done ] || die "the analysis ended $(printf '%s' "$A" | jget status): $(printf '%s' "$A" | jget error)"
wait_idle || die "the preview did not end in 30 minutes"
T3=$(now)
TAKES_PREVIEW="$(takes_in_log)"
PROFILE="$(dev GET "/api/profiles/$PID_")"
[ "$(printf '%s' "$PROFILE" | jget status)" = ready ] || die "the voice is not ready: $(printf '%s' "$PROFILE" | jget error)"

# The narration.
BODY="$(python3 -c 'import json,sys; print(json.dumps({"profile_id":sys.argv[1],"text":sys.argv[2]}))' "$PID_" "$TEXT")"
G="$(dev POST /api/generations -H 'Content-Type: application/json' --data-binary "$BODY")"
GID="$(printf '%s' "$G" | jget id)"
NJOB="$(printf '%s' "$G" | jget job id)"
[ -n "$GID" ] && [ -n "$NJOB" ] || die "creating the narration failed: $G"
if ! N="$(wait_job "$NJOB" "$NARRATION_LIMIT")"; then
    dev POST "/api/generations/$GID/cancel" >/dev/null || true
    wait_job "$NJOB" 60 >/dev/null || true
    die "the narration did not leave the queue in $NARRATION_LIMIT s; stopped it"
fi
T4=$(now)
dev GET "/api/generations/$GID" > "$OUT/narration.json"
NSTATUS="$(jget status < "$OUT/narration.json")"
if [ "$(printf '%s' "$N" | jget status)" != done ] || [ "$NSTATUS" = error ]; then
    die "the narration ended $(printf '%s' "$N" | jget status) / $NSTATUS: $(printf '%s' "$N" | jget error) $(jget message < "$OUT/narration.json")"
fi
dev GET "/api/generations/$GID/audio?fmt=wav" -f -o "$OUT/narration.wav" || die "no WAV"
dev GET "/api/generations/$GID/subtitles?fmt=srt" -f -o "$OUT/narration.srt" || die "no SRT"
dev GET "/api/generations/$GID/timings" -f -o "$OUT/timings.json" || die "no timings"
TAKES_ALL="$(takes_in_log)"

# Stop the engine by its pid and wait for it (time -l then writes its numbers to engine.log).
kill -TERM "$ENGINE_PID"
wait "$TIME_PID" || true
TIME_PID=""
kill -0 "$ENGINE_PID" 2>/dev/null && die "the engine $ENGINE_PID did not stop"
ENGINE_PID=""
python3 -c 'import socket,sys; s=socket.socket(); s.settimeout(1); sys.exit(1 if s.connect_ex(("127.0.0.1",int(sys.argv[1])))==0 else 0)' "$PORT" \
    || die "port $PORT is still taken after the engine stopped"
rm -rf "$TMP"

COMMIT="$(git -C "$WT" rev-parse HEAD)"
RC=0
python3 - "$OUT" "$LEDGER" "$ATTEMPT" "$PREV_TAKES" "$TAKES_PREVIEW" "$TAKES_ALL" "$TAKE_CAP" "$CAP" "$COMMIT" "$CLIP_SECONDS" \
    "$NOTE" "$T0" "$T1" "$T2" "$T3" "$T4" "$(printf '%s' "$PROFILE")" <<'PY' || RC=$?
import json, re, sys, wave
from pathlib import Path
out, ledger = Path(sys.argv[1]), Path(sys.argv[2])
attempt, prev, tp, tall, cap, run_cap = (int(x) for x in sys.argv[3:9])
commit, clip_s, note = sys.argv[9], float(sys.argv[10]), sys.argv[11]
t0, t1, t2, t3, t4 = (float(x) for x in sys.argv[12:17])
profile = json.loads(sys.argv[17])
log = (out / "engine.log").read_text(errors="replace")
rss = re.search(r"^\s*(\d+)\s+maximum resident set size", log, re.M)
g = json.loads((out / "narration.json").read_text())
with wave.open(str(out / "narration.wav")) as w:
    audio_s = w.getnframes() / w.getframerate()
loads = [l.strip() for l in log.splitlines() if re.search(r"load", l, re.I) and not l.startswith("take ")]
parts = [{"part": c.get("i"), "status": c.get("status"), "issues": c.get("issues"),
          "takes": len(c.get("attempts") or []), "takes_kept": len(c.get("takes") or [])} for c in g.get("chunks") or []]
tips = profile.get("tips") or []
r = {
    "attempt": attempt,
    "takes_preview": tp,
    "takes_narration": tall - tp,
    "takes_this_attempt": tall,
    "takes_total": prev + tall,
    "take_cap": cap,
    "take_cap_this_run": run_cap,
    "seconds": {"engine_start": round(t1 - t0, 1), "analysis": round(t2 - t1, 1), "preview": round(t3 - t2, 1),
                "narration": round(t4 - t3, 1)},
    "model_load_lines": loads,
    "audio_seconds": round(audio_s, 2),
    "peak_rss_bytes": int(rss.group(1)) if rss else 0,
    "parts": parts,
    "outcome": g.get("outcome"),
    "review": len(g.get("review") or []),
    "narration_status": g.get("status"),
    "studio": profile.get("studio"),
    "studio_tip": next((t for t in tips if "Studio restoration" in str(t)), None),
    "room_echo_db": profile.get("room_echo_db"),
    "room_echo_after_db": profile.get("room_echo_after_db"),
    "denoised": profile.get("denoised"),
    "voice_preview": bool(profile.get("preview")),
    "reference_clip_seconds": clip_s,
    "commit": commit,
    "note": note,
}
(out / "report.json").write_text(json.dumps(r, indent=1) + "\n")
ok = (out / "narration.srt").stat().st_size > 0 and audio_s > 1 and tall <= cap and r["peak_rss_bytes"] > 0
print(json.dumps(r, indent=1))
sys.exit(0 if ok else 1)
PY
python3 -c 'import json,sys,os
p,n,t=sys.argv[1],int(sys.argv[2]),int(sys.argv[3]); d=json.load(open(p)) if os.path.exists(p) else {"attempts":[]}
d["attempts"].append({"attempt":n,"takes":t,"result":"report"}); json.dump(d,open(p,"w"),indent=1)' "$LEDGER" "$ATTEMPT" "$TAKES_ALL"
REPORTED=1
echo "$NOTE"
exit "$RC"
