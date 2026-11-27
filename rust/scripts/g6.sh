#!/usr/bin/env bash
# Gate G6 (rust/PLAN.md): read-only against GitHub. Did a Windows job pass on the newest commit of
# rust/taketwo that changed anything other than *.md?
#   rust/scripts/g6.sh          judges the job named exactly "build (cuda)" (the default)
#   rust/scripts/g6.sh check    judges the job named exactly "check" (any job name of the workflow works)
# Exit 0: the judged job concluded success. Exit 75: queued, running, or no run yet.
# Exit 1: anything else. The other jobs are printed, not judged.
# A run counts for the target commit when its head is that commit or a descendant that differs from it
# only in *.md files (GitHub runs a push on its tip, which may be a later PROGRESS.md commit).
set -euo pipefail

REPO=fnikobanagyo-alt/taketwo
BRANCH=rust/taketwo
WORKFLOW="Windows CUDA build"   # the name: in .github/workflows/windows-cuda.yml, matched exactly
GATE_JOB="${1:-build (cuda)}"
[ $# -le 1 ] || { echo "usage: g6.sh [job name]"; exit 1; }
WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$WT"

target="$(git log -1 --format=%H "$BRANCH" -- . ':(exclude,glob)**/*.md')"
[ -n "$target" ] || { echo "FAIL: no non-.md commit on $BRANCH"; exit 1; }
echo "target: $(git log -1 --format='%h %s' "$target")"

counts_for_target() { # $1 = run head sha
    [ "$1" = "$target" ] && return 0
    git cat-file -e "$1^{commit}" 2>/dev/null || return 1
    git merge-base --is-ancestor "$target" "$1" || return 1
    [ -z "$(git diff --name-only "$target" "$1" | grep -v '\.md$' || true)" ]
}

# By name, not file: GitHub cannot look a workflow up by file until it is on the default branch.
runs="$(gh run list -R "$REPO" --branch "$BRANCH" -L 100 --json databaseId,headSha,workflowName \
        | jq -r --arg w "$WORKFLOW" '.[] | select(.workflowName == $w) | "\(.databaseId) \(.headSha)"')"
run_id=""
# Newest first; a re-run keeps its id, so the first match is the newest attempt for that commit.
while read -r id sha; do
    [ -n "$id" ] || continue
    if counts_for_target "$sha"; then run_id="$id"; run_sha="$sha"; break; fi
done <<<"$runs"

if [ -z "$run_id" ]; then
    echo "WAIT: no $WORKFLOW run yet for ${target:0:7} (is it pushed?)"
    exit 75
fi

run="$(gh run view "$run_id" -R "$REPO" --json status,conclusion,url,jobs)"
echo "run $run_id on ${run_sha:0:7}: $(jq -r '"\(.status) \(.conclusion) \(.url)"' <<<"$run")"
job() { jq -r --arg n "$1" '[.jobs[] | select(.name == $n)][0] // empty
        | "\(.status) \(.conclusion) \(.startedAt) \(.completedAt)"' <<<"$run"; }

for n in "build (cuda)" "build (cuda,flash-attn)" "check"; do
    [ "$n" = "$GATE_JOB" ] || echo "info, not judged: $n: $(job "$n" || true)"
done

plain="$(job "$GATE_JOB")"
echo "gate job: $GATE_JOB: ${plain:-missing}"
read -r status conclusion _ <<<"${plain:-missing missing}"
if [ "$status" = completed ]; then
    [ "$conclusion" = success ] && { echo "G6 PASS"; exit 0; }
    echo "G6 FAIL: $GATE_JOB concluded $conclusion"; exit 1
fi
if [ "$status" = missing ] && [ "$(jq -r .status <<<"$run")" = completed ]; then
    echo "G6 FAIL: run completed without a job named exactly '$GATE_JOB'"; exit 1
fi
echo "G6 WAIT: $GATE_JOB is $status"
exit 75
