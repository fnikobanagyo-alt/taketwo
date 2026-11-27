#!/usr/bin/env bash
# Gate G8 (rust/PLAN.md): read-only proof that THIS work left Franz's Python VoiceForge alone.
# Fails only on something this work would have done:
#   1. the Rust port is on main or origin/main, or on any branch or tag other than rust/taketwo (local or GitHub);
#   2. a process of this work (command line or working folder inside this worktree) has a file open under
#      the Python app's data/ or ~/Library/Application Support/VoiceForge;
#   3. Franz's app on 8766 no longer answers as TakeTwo.
# Then prints, as information only, how main moved since the baseline and the metadata differences
# (path, size, mtime; file contents are never read) of his data folders against rust/.baseline/*.manifest.
# Franz uses his app while this work runs (new voices, narrations, the database, caches): those
# differences are his and are never a failure (Franz, 2026-11-27).
# Default output is counts only, safe to paste into PROGRESS.md. `--names` also prints the changed paths:
# those are private, never paste or commit them.
# Self-test hooks: G8_FIRST_COMMIT (another commit to look for) and G8_ALSO_WATCH (one more watched folder)
# exist to show the failing branches on stand-ins. A run with either set never passes (exit 2).
set -euo pipefail

FIRST_COMMIT="${G8_FIRST_COMMIT:-7e0a172}"             # first commit of the Rust port
BASELINE_MAIN=4448aaa9b241f2e52cfaa6c06fa98b89653be82e # main when the baseline was recorded

WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MAIN="$(dirname "$(git -C "$WT" rev-parse --path-format=absolute --git-common-dir)")"
BASELINE="$WT/rust/.baseline"
NAMES=0
[ "${1:-}" = "--names" ] && NAMES=1

fail=0
git -C "$WT" fetch --quiet origin main

for ref in main origin/main; do
    rc=0
    git -C "$WT" merge-base --is-ancestor "$FIRST_COMMIT" "$ref" || rc=$?
    case $rc in
        0) echo "FAIL: $FIRST_COMMIT (Rust port) is on $ref"; fail=1 ;;
        1) echo "ok: Rust port is not on $ref ($(git -C "$WT" rev-parse --short "$ref"))" ;;
        *) echo "FAIL: could not check $ref (git exit $rc)"; fail=1 ;;
    esac
done

# 1b. No branch or tag other than rust/taketwo carries this work, here or on GitHub.
OURS_LOCAL='refs/heads/rust/taketwo refs/remotes/origin/rust/taketwo'
stray=0
while read -r ref; do
    [ -n "$ref" ] || continue
    case " $OURS_LOCAL " in *" $ref "*) ;; *) echo "FAIL: Rust port is on $ref"; stray=1 ;; esac
done < <(git -C "$WT" for-each-ref --contains "$FIRST_COMMIT" --format='%(refname)' refs/heads refs/remotes refs/tags)
if remote="$(git -C "$WT" ls-remote --heads --tags origin)"; then
    while read -r sha ref; do
        [ -n "$sha" ] && [ "$ref" != refs/heads/rust/taketwo ] || continue
        # A commit this clone does not have was not made here.
        git -C "$WT" cat-file -e "$sha^{commit}" 2>/dev/null || continue
        if git -C "$WT" merge-base --is-ancestor "$FIRST_COMMIT" "$sha"; then
            echo "FAIL: Rust port is on GitHub $ref"; stray=1
        fi
    done <<<"$remote"
else
    echo "FAIL: could not list the branches on GitHub"; stray=1
fi
if [ "$stray" -eq 0 ]; then echo "ok: Rust port is only on rust/taketwo (local and GitHub)"; else fail=1; fi

# 2. No process of this work holds a file in Franz's folders. One lsof pass over this user's processes
# (never `lsof +D`, which walks the folders). Only pids and commands are printed, never file names.
W1="$MAIN/data"
W2="$HOME/Library/Application Support/VoiceForge"
W3="${G8_ALSO_WATCH:-}"
holders="$(lsof -nP -Fpfn 2>/dev/null | awk -v wt="$WT" -v w1="$W1" -v w2="$W2" -v w3="$W3" '
    function under(p, d) { return d != "" && (p == d || index(p, d "/") == 1) }
    /^p/ { pid = substr($0, 2); next }
    /^f/ { fd = substr($0, 2); next }
    /^n/ { n = substr($0, 2)
           if (fd == "cwd" && under(n, wt)) ours[pid] = 1
           if (under(n, w1) || under(n, w2) || under(n, w3)) holds[pid]++ }
    END { for (p in holds) print p, (p in ours ? "cwd" : "-"), holds[p] }' || true)"
ours_holding=0 others_holding=0
while read -r pid flag count; do
    [ -n "$pid" ] || continue
    cmd="$(ps -o command= -p "$pid" 2>/dev/null || true)"
    if [ "$flag" = cwd ] || [[ "$cmd" == *"$WT/"* ]]; then
        echo "FAIL: a process of this work (pid $pid) has $count file(s) open in Franz's folders: ${cmd:0:160}"
        ours_holding=1
    else
        others_holding=$((others_holding + 1))
    fi
done <<<"$holders"
if [ "$ours_holding" -eq 0 ]; then
    echo "ok: no process of this work has a file open under data/ or App Support/VoiceForge ($others_holding other process(es) do: Franz's app)"
else
    fail=1
fi

# 3. Franz's app still answers.
health="$(curl -s --max-time 5 http://127.0.0.1:8766/api/health || true)"
if [[ "$health" == *'"name":"TakeTwo"'* ]]; then
    echo "ok: 8766 health: $health"
else
    echo "FAIL: 8766 health did not name TakeTwo: ${health:-no answer}"
    fail=1
fi

echo
echo "--- information only: Franz's own app use and other sessions (never fails the gate) ---"
for ref in main origin/main; do
    echo "$ref since baseline ${BASELINE_MAIN:0:7}: $(git -C "$WT" rev-list --count "$BASELINE_MAIN..$ref") new commit(s)"
    git -C "$WT" log --oneline "$BASELINE_MAIN..$ref" | sed 's/^/  /'
done

# Same line format as the baseline manifests: <absolute path>|<size>|<mtime seconds>, C-sorted, files only.
manifest_now() {
    find "$1" -type f -exec stat -f '%N|%z|%m' {} + 2>/dev/null | LC_ALL=C sort
}

compare() {
    local label="$1" dir="$2" manifest="$3"
    if [ ! -f "$manifest" ]; then echo "$label: no baseline manifest at $manifest"; return; fi
    if [ ! -d "$dir" ]; then echo "$label: folder is missing"; return; fi
    # Counts per kind of change, top-level folder and audio/other; paths only with --names.
    manifest_now "$dir" | awk -F'|' -v root="$dir/" -v names="$NAMES" -v label="$label" '
        function kind(p,   rel, top) {
            rel = substr(p, length(root) + 1)
            top = (index(rel, "/") ? substr(rel, 1, index(rel, "/") - 1) "/" : "(top-level file)")
            return top " " (p ~ /\.(wav|mp3|m4a|flac|ogg|aac)$/ ? "audio" : "other")
        }
        function note(what, p) { count[what " " kind(p)]++; total[what]++; if (names) print "  " what ": " p }
        NR == FNR { base[$1] = $2 "|" $3; next }
        { now[$1] = 1
          if (!($1 in base)) note("added", $1)
          else if (base[$1] != $2 "|" $3) note("changed", $1) }
        END {
            for (p in base) if (!(p in now)) note("removed", p)
            printf "%s: %d added, %d removed, %d changed (size or mtime)\n", label, total["added"], total["removed"], total["changed"]
            for (k in count) printf "  %s: %d\n", k, count[k] | "LC_ALL=C sort"
        }' "$manifest" -
}

compare "data/" "$MAIN/data" "$BASELINE/data.manifest" || echo "data/: could not compare"
compare "App Support/VoiceForge" "$HOME/Library/Application Support/VoiceForge" "$BASELINE/appdata-voiceforge.manifest" \
    || echo "App Support/VoiceForge: could not compare"

echo
if [ "$fail" -ne 0 ]; then echo "G8 FAIL"; exit 1; fi
if [ -n "${G8_FIRST_COMMIT:-}${G8_ALSO_WATCH:-}" ]; then echo "G8 SELF-TEST RUN: not a verdict"; exit 2; fi
echo "G8 PASS"
