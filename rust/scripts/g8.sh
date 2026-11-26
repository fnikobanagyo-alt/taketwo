#!/usr/bin/env bash
# Gate G8 (rust/PLAN.md): read-only safety check of Franz's side.
# Fails if this work reached main or origin/main, or if Franz's app on 8766 does not answer as TakeTwo.
# Then prints, without failing, how main moved since the baseline and the metadata differences
# (path, size, mtime; file contents are never read) of his data folders against rust/.baseline/*.manifest.
# Default output is counts only, safe to paste into PROGRESS.md. `--names` also prints the changed paths:
# those are private, never paste or commit them.
set -euo pipefail

FIRST_COMMIT=7e0a172                                   # first commit of the Rust port
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

health="$(curl -s --max-time 5 http://127.0.0.1:8766/api/health || true)"
if [[ "$health" == *'"name":"TakeTwo"'* ]]; then
    echo "ok: 8766 health: $health"
else
    echo "FAIL: 8766 health did not name TakeTwo: ${health:-no answer}"
    fail=1
fi

echo
echo "--- for the lead to attribute (does not fail the gate) ---"
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
echo "G8 PASS"
