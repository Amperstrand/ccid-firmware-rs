#!/usr/bin/env bash
# pr-verify.sh — the LOCAL CI gate for ccid-firmware-rs.
# Fleet pattern: Amperstrand/.github workflow-templates/local-ci-gate.md
# (reference implementation: tollgate-module-basic-go scripts/pr-verify.sh;
#  sibling ports: heywaitr / nodns-poc / numo-pos-spike scripts/pr-verify.sh).
#
# Legs (host-runnable mirror of .github/workflows/ci.yml):
#   hygiene           conflict-marker grep + bash -n on tracked shell scripts
#   fmt               cargo fmt --all -- --check                      (stm32-lint)
#   clippy-f469       clippy --release, default features              (stm32-lint pass 1)
#   clippy-f746       clippy --release, f746 bitbang features         (stm32-lint pass 2)
#   clippy-crates     pn7160-nci no_std check + never-linted trio     (crates-lint)
#   host-tests        cargo test --workspace, x86_64                  (stm32-test)
#   esp32-host-tests  esp32-ccid host tests + clippy + pn7160 variant (esp32-host-test)
#   python-collect    ruff F+E9 + scripts unit tests + pytest collect (python-collect)
#
# NOT covered on purpose (say so, never imply otherwise): the 4-entry
# stm32-build release+objcopy matrix, ALL esp32 target builds (xtensa fork /
# C3 + IDF v5.5.5 toolchains) and their sdkconfig gates, the sdkconfig
# clean-wire gate (tools/check_sdkconfigs.py — reads built artifacts),
# every hardware suite (labgrid HIL, conformance battery, differential,
# GPG, perf, bench nightly), and the Codex review. GitHub Actions (ci.yml)
# remains the authoritative CI; this gate reproduces its host-side legs
# locally and posts the evidence.
#
# Usage:
#   scripts/pr-verify.sh 12                 # PR mode: verify + comment
#   scripts/pr-verify.sh 12 --no-post       # verify, print table only
#   scripts/pr-verify.sh origin/main        # ref mode: verify + print
#   scripts/pr-verify.sh 12 --only fmt,host-tests
#
# No secrets are read or embedded; PR mode uses the invoking account's gh
# auth for view/comment only. PR mode pins the head SHA at start-of-run.
# Logs + worktree live under .omo/pr-verify/ (persistent per the 2026-10-14
# durable-artifacts rule — never /tmp).
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LEGS=(hygiene fmt clippy-f469 clippy-f746 clippy-crates host-tests esp32-host-tests python-collect)

usage() {
    echo "usage: $0 <PR-number | git-ref> [--no-post] [--only LEG[,LEG...]] [--remove-logs]" >&2
    echo "legs: ${LEGS[*]}" >&2
    exit 2
}
die() { echo "pr-verify: $*" >&2; exit 2; }

TARGET=""; NO_POST=0; REMOVE_LOGS=0; ONLY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --no-post) NO_POST=1 ;;
        --remove-logs) REMOVE_LOGS=1 ;;
        --only) [ $# -ge 2 ] || die "--only needs a comma-separated leg list"; ONLY="$2"; shift ;;
        --only=*) ONLY="${1#--only=}" ;;
        -h|--help) usage ;;
        -*) die "unknown option: $1" ;;
        *) [ -z "$TARGET" ] || die "one target only (got '$TARGET' and '$1')"; TARGET="$1" ;;
    esac
    shift
done
[ -n "$TARGET" ] || usage

if [ -n "$ONLY" ]; then
    IFS=',' read -r -a wanted <<<"$ONLY"
    for w in "${wanted[@]}"; do
        ok=0; for l in "${LEGS[@]}"; do [ "$w" = "$l" ] && ok=1; done
        [ "$ok" = 1 ] || die "unknown leg '$w' (legs: ${LEGS[*]})"
    done
else
    wanted=("${LEGS[@]}")
fi
want_leg() { local L; for L in "${wanted[@]}"; do [ "$L" = "$1" ] && return 0; done; return 1; }

for c in git cargo python3; do command -v "$c" >/dev/null 2>&1 || die "$c is required but not on PATH"; done

# The slug is only needed in PR mode (gh view/comment) — ref mode never
# talks to GitHub, so a checkout with a missing/non-GitHub origin can still
# verify local refs (Codex P2 on #300). Enforced below, inside PR mode.
SLUG="$(git -C "$ROOT" remote get-url origin 2>/dev/null | sed -E 's#.*github\.com[:/]##; s#\.git$##')"

PR_NUMBER=""; PR_TITLE=""; PR_STATE=""; HEAD_REF=""; BASE_SHA=""
if [[ "$TARGET" =~ ^[0-9]+$ ]]; then
    command -v gh >/dev/null 2>&1 || die "PR mode needs gh"
    [ -n "$SLUG" ] || die "PR mode needs the GitHub slug — origin remote is not a github URL"
    PR_NUMBER="$TARGET"
    meta="$(gh pr view "$PR_NUMBER" -R "$SLUG" --json title,state,headRefName,headRefOid --jq='[.title,.state,.headRefName,.headRefOid]|@tsv' 2>/dev/null)" \
        || die "gh pr view $PR_NUMBER failed"
    IFS=$'\t' read -r PR_TITLE PR_STATE HEAD_REF HEAD_OID <<<"$meta"
    [ "$PR_STATE" = "OPEN" ] || die "PR #$PR_NUMBER is $PR_STATE — verify open PRs"
    git -C "$ROOT" fetch --quiet origin main 2>/dev/null || true
    BASE_SHA="$(git -C "$ROOT" merge-base "$HEAD_OID" origin/main 2>/dev/null || echo '?')"
    echo "== PR #$PR_NUMBER: $PR_TITLE"
    echo "== head $HEAD_REF @ ${HEAD_OID:0:10} (base ${BASE_SHA:0:10})"
else
    HEAD_REF="$TARGET"
    if HEAD_OID="$(git -C "$ROOT" rev-parse --verify --quiet "$TARGET^{commit}")"; then
        echo "== ref $TARGET @ ${HEAD_OID:0:10} (already local)"
    else
        git -C "$ROOT" fetch --quiet origin "$TARGET" || die "cannot fetch '$TARGET' from origin"
        HEAD_OID="$(git -C "$ROOT" rev-parse --verify FETCH_HEAD^{commit})" || die "FETCH_HEAD did not resolve to a commit"
    fi
    BASE_SHA="$(git -C "$ROOT" rev-parse origin/main 2>/dev/null || echo "?")"
    echo "== ref $TARGET @ ${HEAD_OID:0:10}"
fi

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
# Slash-unsafe ref names (any ref-mode "review/x" target) must not nest
# log dirs — sanitize every char outside [a-zA-Z0-9._-], not just dashes.
LOGDIR="$ROOT/.omo/pr-verify/${TARGET//[^a-zA-Z0-9._-]/_}-$STAMP"
mkdir -p "$LOGDIR"
WT="$LOGDIR/worktree"
cleanup() {
    git -C "$ROOT" worktree remove --force "$WT" >/dev/null 2>&1 || true
    git -C "$ROOT" worktree prune >/dev/null 2>&1 || true
    if [ "$REMOVE_LOGS" = 1 ]; then rm -rf "$LOGDIR"; else
        rm -rf "$WT" 2>/dev/null || true
        echo "== logs kept in $LOGDIR"
    fi
}
trap cleanup EXIT

git -C "$ROOT" fetch --quiet origin "$HEAD_REF" 2>/dev/null || true
# Fork-PR fallback (#306): a fork's head branch does not exist on OUR origin,
# so the branch-name fetch above cannot bring the head commit local (found
# live on 5 felixfelix-bot PRs in tollgate batch 2). GitHub serves every
# PR head as refs/pull/<N>/head on the BASE repo — fetch that when the
# branch fetch came up empty.
if ! git -C "$ROOT" rev-parse --verify --quiet "${HEAD_OID}^{commit}" >/dev/null; then
    [ -n "$PR_NUMBER" ] && git -C "$ROOT" fetch --quiet origin "refs/pull/${PR_NUMBER}/head" 2>/dev/null || true
fi
git -C "$ROOT" rev-parse --verify --quiet "${HEAD_OID}^{commit}" >/dev/null \
    || die "head commit $HEAD_OID is not local after fetch — rerun"
git -C "$ROOT" worktree add --quiet --detach "$WT" "$HEAD_OID" || die "worktree add failed for $HEAD_OID"
echo "== worktree: $WT"

# NO submodule init (P1 fix on #300): the old step ran
# `submodule update --init vendor/synopsys-usb-otg`, but that vendor copy
# was retired to a rev-pinned git-fork [patch] dep in #264 — the path no
# longer exists and git died ("pathspec ... did not match") before any
# leg, on every ref (DOA, rc=2). No host leg needs a submodule:
# synopsys-usb-otg resolves as a git dep, vendor/stm32f469i-disc is a
# normally-tracked path, and reference/* are quote-only (never compiled).

# The gate script itself may not be committed at the verified head (it can
# post on PRs that predate it) — hash the INVOKING copy, not the worktree's.
SCRIPT_ID="$(sha256sum "$ROOT/scripts/pr-verify.sh" 2>/dev/null | cut -c1-12)"
HOST="$(hostname -s 2>/dev/null || echo unknown)"

leg_names=() leg_status=() leg_detail=() leg_secs=()
FAILED_LEGS=()
record() {
    leg_names+=("$1"); leg_status+=("$2"); leg_detail+=("$3"); leg_secs+=("$4")
    [ "$2" = "FAIL" ] && FAILED_LEGS+=("$1")
}

cd "$WT" || die "cannot cd into worktree"

# ruff resolution: CI pins 0.17.0 exactly (ci.yml `pip install ruff==0.17.0`)
# and rule behavior moves between releases — accept a PATH ruff only on the
# exact pin, else a pinned uvx/pip install. Never a floating one (Codex P2
# on #300: an unpinned PATH ruff silently diverged from CI).
RUFF=""
resolve_ruff() {
    if command -v ruff >/dev/null 2>&1 \
        && [ "$(ruff --version 2>/dev/null | awk '{print $2}')" = "0.17.0" ]; then
        RUFF="ruff"; return 0
    fi
    if command -v uvx >/dev/null 2>&1; then RUFF="uvx ruff@0.17.0"; return 0; fi
    pip install --quiet ruff==0.17.0 >/dev/null 2>&1 && RUFF="ruff" && return 0
    return 1
}

run_hygiene() {
    echo "== leg: hygiene"
    local t=$SECONDS log="$LOGDIR/hygiene.log"
    if git grep -nE '^(<{7} |>{7} )' -- . >"$log" 2>&1; then
        record hygiene FAIL "committed conflict markers — see hygiene.log" $((SECONDS - t))
        return
    fi
    # Every tracked shell script, enumerated — a static list rots the
    # moment a new script lands (pr-stamps-status.sh was missing from
    # round 2 while shipping in the same PR). s_check_sdkconfig carries
    # no extension, so it is added explicitly.
    local shells
    shells="$(git ls-files '*.sh') firmware/esp32-ccid/s_check_sdkconfig"
    local bad=0 s
    for s in $shells; do
        [ -f "$s" ] || continue
        bash -n "$s" >>"$log" 2>&1 || { echo "bash -n failed: $s" >>"$log"; bad=1; }
    done
    if [ "$bad" = 0 ]; then
        record hygiene PASS "no conflict markers; bash -n clean" $((SECONDS - t))
    else
        record hygiene FAIL "bash -n failed — see hygiene.log" $((SECONDS - t))
    fi
}

run_fmt() {
    echo "== leg: fmt"
    local t=$SECONDS log="$LOGDIR/fmt.log"
    if cargo fmt --all -- --check >"$log" 2>&1; then
        record fmt PASS "cargo fmt --all --check" $((SECONDS - t))
    else
        record fmt FAIL "see fmt.log" $((SECONDS - t))
    fi
}

run_clippy_f469() {
    echo "== leg: clippy-f469"
    local t=$SECONDS log="$LOGDIR/clippy-f469.log"
    if RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf -- -D warnings >"$log" 2>&1; then
        record clippy-f469 PASS "clippy --release, default features" $((SECONDS - t))
    else
        record clippy-f469 FAIL "see clippy-f469.log" $((SECONDS - t))
    fi
}

run_clippy_f746() {
    echo "== leg: clippy-f746"
    local t=$SECONDS log="$LOGDIR/clippy-f746.log"
    if RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf \
        --no-default-features --features "stm32f746,profile-cherry-smartterminal-st2xxx" \
        -- -D warnings >"$log" 2>&1; then
        record clippy-f746 PASS "clippy --release, f746 bitbang" $((SECONDS - t))
    else
        record clippy-f746 FAIL "see clippy-f746.log" $((SECONDS - t))
    fi
}

run_clippy_crates() {
    echo "== leg: clippy-crates"
    local t=$SECONDS log="$LOGDIR/clippy-crates.log"
    if cargo check -p pn7160-nci --no-default-features --target x86_64-unknown-linux-gnu >"$log" 2>&1 \
    && cargo clippy -p pn7160-nci -p ccid-pcsc-host -p ccid-host-tools --all-targets \
        --target x86_64-unknown-linux-gnu -- -D warnings >>"$log" 2>&1; then
        record clippy-crates PASS "pn7160-nci no_std + never-linted trio" $((SECONDS - t))
    else
        record clippy-crates FAIL "see clippy-crates.log" $((SECONDS - t))
    fi
}

run_host_tests() {
    echo "== leg: host-tests"
    local t=$SECONDS log="$LOGDIR/host-tests.log"
    # Mirrors stm32-test INCLUDING its display-feature step (ci.yml, #285):
    # --workspace enables only default features, so the pinpad-UI/PIN-flow
    # test surface behind #[cfg(feature = "display")] needs the explicit
    # -p run or the gate can go green where CI goes red (Codex P1 on #300).
    if cargo test --workspace --target x86_64-unknown-linux-gnu >"$log" 2>&1 \
    && cargo test -p ccid-firmware-rs --features display --target x86_64-unknown-linux-gnu >>"$log" 2>&1; then
        record host-tests PASS "workspace + display-feature tests (x86_64)" $((SECONDS - t))
    else
        record host-tests FAIL "see host-tests.log" $((SECONDS - t))
    fi
}

run_esp32_host_tests() {
    echo "== leg: esp32-host-tests"
    local t=$SECONDS log="$LOGDIR/esp32-host-tests.log"
    if { cd firmware/esp32-ccid || exit 1; } \
    && cargo test --target x86_64-unknown-linux-gnu >"$log" 2>&1 \
    && cargo clippy --target x86_64-unknown-linux-gnu -- -D warnings >>"$log" 2>&1 \
    && cargo test --target x86_64-unknown-linux-gnu --no-default-features --features backend-pn7160 >>"$log" 2>&1; then
        record esp32-host-tests PASS "tests + clippy + backend-pn7160 (x86_64)" $((SECONDS - t))
    else
        record esp32-host-tests FAIL "see esp32-host-tests.log" $((SECONDS - t))
    fi
    cd "$WT" || die "cannot cd back to worktree"
}

run_python_collect() {
    echo "== leg: python-collect"
    local t=$SECONDS log="$LOGDIR/python-collect.log"
    if ! resolve_ruff; then
        record python-collect FAIL "no ruff on PATH and pinned install failed" $((SECONDS - t))
        return
    fi
    local collect_txt="$LOGDIR/pytest-collect.txt"
    if $RUFF check scripts tests/hardware host-tools >"$log" 2>&1 \
    && python3 scripts/test_gen_status.py >>"$log" 2>&1 \
    && python3 scripts/test_status_rollup.py >>"$log" 2>&1 \
    && python3 scripts/test_status_drift.py >>"$log" 2>&1 \
    && python3 -m pytest tests/hardware/ --collect-only -q >"$collect_txt" 2>&1 \
    && grep -q " tests collected" "$collect_txt"; then
        record python-collect PASS "ruff F+E9 + scripts unit tests + pytest collect" $((SECONDS - t))
    else
        [ -f "$collect_txt" ] && cat "$collect_txt" >>"$log"
        record python-collect FAIL "see python-collect.log" $((SECONDS - t))
    fi
}

STARTED=$SECONDS
want_leg hygiene          && run_hygiene
want_leg fmt              && run_fmt
want_leg clippy-f469      && run_clippy_f469
want_leg clippy-f746      && run_clippy_f746
want_leg clippy-crates    && run_clippy_crates
want_leg host-tests       && run_host_tests
want_leg esp32-host-tests && run_esp32_host_tests
want_leg python-collect   && run_python_collect
WALL=$((SECONDS - STARTED))

green=0; red=0
for st in "${leg_status[@]}"; do case "$st" in PASS) green=$((green+1)) ;; FAIL) red=$((red+1)) ;; esac; done
VERDICT="$green legs green"
[ "$red" = 0 ] || VERDICT="$red leg(s) RED — $green green"

echo
echo "=================================================="
printf '%-16s %-5s %-8s %s\n' "LEG" "RES" "SECS" "DETAIL"
printf '%-16s %-5s %-8s %s\n' "---" "---" "----" "------"
for i in "${!leg_names[@]}"; do
    printf '%-16s %-5s %-8s %s\n' "${leg_names[$i]}" "${leg_status[$i]}" "${leg_secs[$i]}" "${leg_detail[$i]}"
done
echo "=================================================="
echo "VERDICT: $VERDICT (${WALL}s)"

compose_comment() {
    local body="$LOGDIR/comment.md"
    {
        if [ -n "$PR_NUMBER" ]; then
            echo "### Local CI gate — PR #${PR_NUMBER}: ${PR_TITLE}"
            echo
        else
            echo "### Local CI gate — ref \`${HEAD_REF}\`"
            echo
        fi
        echo "Run of \`scripts/pr-verify.sh\` (fleet pattern: Amperstrand/.github \`workflow-templates/local-ci-gate.md\` — this comment is the local verification evidence, produced on the pinned head SHA; ci.yml remains the authoritative CI)."
        echo
        echo "- **Head:** \`${HEAD_REF}\` @ \`${HEAD_OID:0:10}\`"
        [ -n "$PR_NUMBER" ] && echo "- **Base:** \`main\` @ \`${BASE_SHA:0:10}\`"
        echo "- **Gate:** pr-verify \`$SCRIPT_ID\` on \`${HOST}\`, started $(date -u '+%Y-%m-%d %H:%M UTC'), wall ${WALL}s"
        echo "- **Logs:** \`${LOGDIR}\` (on the verifying host)"
        echo
        echo "| Leg | Result | Detail |"
        echo "| --- | --- | --- |"
        for i in "${!leg_names[@]}"; do
            echo "| ${leg_names[$i]} | **${leg_status[$i]}** | ${leg_detail[$i]} (${leg_secs[$i]}s) |"
        done
        echo
        echo "**Verdict: ${VERDICT}.**"
        echo
        if [ "${#wanted[@]}" != "${#LEGS[@]}" ]; then
            echo "> Note: narrowed run (--only) — legs not listed here did not run."
            echo
        fi
        echo "<details><summary>What this gate does <b>not</b> run (no vacuous green)</summary>"
        echo
        echo "- the 4-entry \`stm32-build\` release + objcopy matrix (cross LTO artifacts)"
        echo "- every esp32 target build (xtensa fork / C3 + IDF v5.5.5) and the sdkconfig gates"
        echo "- the sdkconfig clean-wire gate (\`tools/check_sdkconfigs.py\` — reads built artifacts)"
        echo "- all hardware: labgrid HIL, conformance battery, differential/GPG HIL, perf, bench nightly"
        echo "- the Codex review (bot comment sweep runs separately)"
        echo
        echo "</details>"
    } >"$body"
    echo "$body"
}

if [ -n "$PR_NUMBER" ] && [ "$NO_POST" = 0 ]; then
    BODY="$(compose_comment)"
    if URL="$(gh pr comment "$PR_NUMBER" -R "$SLUG" --body-file "$BODY")"; then
        echo "== posted: $URL"
    else
        echo "== FAILED to post comment (body kept at $BODY)" >&2
        red=$((red + 1))
    fi
elif [ -n "$PR_NUMBER" ]; then
    echo "== --no-post: comment body kept at $(compose_comment)"
fi

[ "$red" = 0 ] && exit 0 || exit 1
