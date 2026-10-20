#!/usr/bin/env bash
# pr-stamps-status.sh — stamp ledger for the local CI gate.
# Lists every open PR with its '### Local CI gate' stamp state:
#   FRESH     latest stamp pins the current head SHA
#   STALE     stamped head != current head (force-pushed/rebased since)
#   UNSTAMPED no gate comment at all
# Companion to scripts/pr-verify.sh (fleet local-ci-gate pattern, #306).
#
# Usage:
#   scripts/pr-stamps-status.sh                     # this repo's open PRs
#   REPO=OpenTollGate/tollgate-module-basic-go scripts/pr-stamps-status.sh
#
# Read-only; no secrets; uses the invoking account's gh auth.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

SLUG="${REPO:-$(git -C "$ROOT" remote get-url origin 2>/dev/null | sed -E 's#.*github\.com[:/]##; s#\.git$##')}"
[ -n "$SLUG" ] || { echo "pr-stamps-status: cannot derive the GitHub slug" >&2; exit 2; }
command -v gh >/dev/null 2>&1 || { echo "pr-stamps-status: gh required" >&2; exit 2; }

prs="$(gh pr list -R "$SLUG" --state open --json number,title,headRefOid --limit 100 \
    --jq '.[] | [.number, .headRefOid, .title] | @tsv')" || exit 2
[ -n "$prs" ] || { echo "no open PRs on $SLUG"; exit 0; }

fresh=0; stale=0; unstamped=0; errored=0
rows=""
while IFS=$'\t' read -r num oid title; do
    # A failed per-PR lookup (auth, rate limit, transient API error) must
    # surface as ERRORED — silently reporting UNSTAMPED would mislabel a
    # reachable stamp as missing (Codex P2 on #300).
    if ! view_body="$(gh pr view "$num" -R "$SLUG" --json comments \
            --jq '[.comments[] | select(.body | test("^### Local CI gate")) | .body] | last' 2>/dev/null)"; then
        state="ERRORED"; errored=$((errored+1))
        rows+="$num|$state|${oid:0:10}|$title"$'\n'
        continue
    fi
    stamped_sha="$(
        printf '%s' "$view_body" \
        | grep -m1 -- '- \*\*Head:\*\*' | grep -oE '@ [`]?[0-9a-f]{10}' | grep -oE '[0-9a-f]{10}$'
    )"
    if [ -z "$stamped_sha" ]; then
        state="UNSTAMPED"; unstamped=$((unstamped+1))
    elif [ "${oid:0:10}" = "$stamped_sha" ]; then
        state="FRESH"; fresh=$((fresh+1))
    else
        state="STALE($stamped_sha)"; stale=$((stale+1))
    fi
    rows+="$num|$state|${oid:0:10}|$title"$'\n'
done <<<"$prs"

printf '%-6s %-16s %-10s %s\n' "PR" "STAMP" "HEAD" "TITLE"
printf '%-6s %-16s %-10s %s\n' "--" "-----" "----" "-----"
printf '%s' "$rows" | while IFS='|' read -r n s h t; do
    printf '%-6s %-16s %-10s %s\n' "$n" "$s" "$h" "$t"
done
echo "-----------------------------------------"
echo "$SLUG: $fresh fresh, $stale stale, $unstamped unstamped, $errored errored"
[ "$errored" = 0 ]
