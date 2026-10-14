#!/usr/bin/env bash
# codex_sweep.sh — dump Codex review comments for PRs into .omo/pr-sweep/
# (persistent, gitignored — survives /tmp wipes; see AGENTS.md "Durable
# session artifacts"). The 2026-10-14 /tmp wipe took the previous sweep
# artifacts with it; this regenerates them from GitHub on demand.
#
# Usage:
#   scripts/codex_sweep.sh <pr> [<pr> ...]
#
# Output: .omo/pr-sweep/<pr>.txt — every Codex-bot review comment with
# file/line anchors, ready for triage against the tree. PRs with no
# badged findings produce an empty file (that is a finding of its own:
# it means the review pass has not run yet — check the PR timeline).
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
repo="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"
mkdir -p .omo/pr-sweep

for pr in "$@"; do
    gh api "repos/${repo}/pulls/${pr}/comments" \
        --jq '.[] | select(.user.login | test("codex"; "i")) |
            "=== \(.path):\(.line // .original_line) (comment \(.id), \(.created_at)) ===
\(.body)
"' >".omo/pr-sweep/${pr}.txt"
    echo "wrote .omo/pr-sweep/${pr}.txt ($(grep -c '^=== ' ".omo/pr-sweep/${pr}.txt") findings)"
done
