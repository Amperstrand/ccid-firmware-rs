#!/usr/bin/env python3
"""Post generated STATUS.md rollups on tracked issues (#183 Phase 2).

The nightly already publishes docs/STATUS.md to the bench/status branch;
this closes the loop on the issue tracker: hardware-verification issues
(e.g. #167-class) get the SAME generated evidence as a comment, so their
threads never rot into hand-written claims (the anti-#110 doctrine).

Anti-spam contract: a comment is posted ONLY when the outcome digest
changes — the per-section tallies and PASS/FAIL/SKIP rows, with stamp
lines (commit + date) normalized out. Identical results on consecutive
nights stay silent; every real change lands exactly one comment carrying
the `status-rollup:v1` marker.

    python3 scripts/status_rollup.py --status docs/STATUS.md \
        --repo Amperstrand/ccid-firmware-rs --issues 167,170 [--token T]

`--issues` empty/absent (or --token absent) is a logged no-op — the
nightly stays silent until the operator opts a repo variable in, and
this script runs dry in tests and on stray invocations. Exit 0 always:
status tooling must never fail the run that publishes status.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.request
from pathlib import Path

MARKER = "<!-- status-rollup:v1 -->"
# Per-run boilerplate that must not count as an outcome change: the
# Verified stamp lines (commit + date) and the rollup wrapper lines
# (marker + header with the per-run link).
_NOISE_LINES = re.compile(
    r"^(?:Verified: .*|<!-- status-rollup:v1 -->|\*\*Bench status rollup\*\*.*)$",
    re.MULTILINE,
)


def digest(status_md: str) -> str:
    """Outcome fingerprint: everything except per-run boilerplate.

    Stamp lines carry commit + date and the wrapper carries the run
    link — both change every run; the digest of the REMAINING rows is
    what 'results unchanged' means (a prior comment and a fresh STATUS
    with identical outcomes digest identically).
    """
    return _NOISE_LINES.sub("", status_md).strip()


def render_comment(status_md: str, run_url: str) -> str:
    """Full rollup comment: marker + run link + the generated body."""
    return (
        f"{MARKER}\n"
        f"**Bench status rollup** (generated — issue #183; "
        f"[full run]({run_url})). `PASS` = verified by test, "
        f"`SKIP` = hardware absent/busy, `FAIL` = broken.\n\n"
        f"{status_md.strip()}\n"
    )


def should_post(last_rollup_body: str | None, new_status_md: str) -> bool:
    """True unless the issue's last rollup comment has the same digest."""
    if last_rollup_body is None or MARKER not in last_rollup_body:
        return True
    return digest(last_rollup_body) != digest(new_status_md)


def fetch_last_rollup(repo: str, issue: str, token: str) -> str | None:
    """Body of the issue's newest comment, or None (API errors -> None:
    a fetch failure must degrade to 'post', never silently skip)."""
    req = urllib.request.Request(
        f"https://api.github.com/repos/{repo}/issues/{issue}/comments?per_page=100",
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "User-Agent": "ccid-status-rollup",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            comments = json.loads(resp.read().decode())
    except (OSError, ValueError) as e:
        print(f"status_rollup: fetch failed for #{issue} ({e}) — will post", file=sys.stderr)
        return None
    return comments[-1]["body"] if comments else None


def post_comment(repo: str, issue: str, body: str, token: str) -> bool:
    data = json.dumps({"body": body}).encode()
    req = urllib.request.Request(
        f"https://api.github.com/repos/{repo}/issues/{issue}/comments",
        data=data,
        method="POST",
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "User-Agent": "ccid-status-rollup",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            resp.read()
            return resp.status == 201
    except OSError as e:
        print(f"status_rollup: post to #{issue} failed ({e})", file=sys.stderr)
        return False


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--status", type=Path, default=Path("docs/STATUS.md"))
    ap.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", ""))
    ap.add_argument("--issues", default="",
                    help="comma-separated issue numbers (empty = no-op)")
    ap.add_argument("--token", default="",
                    help="GitHub token; absent = dry run (no posts)")
    ap.add_argument("--run-url", default=os.environ.get("GITHUB_SERVER_URL", "https://github.com")
                    and f"{os.environ.get('GITHUB_SERVER_URL', 'https://github.com')}/"
                        f"{os.environ.get('GITHUB_REPOSITORY', '')}/actions/runs/"
                        f"{os.environ.get('GITHUB_RUN_ID', '')}",
                    help="workflow run URL linked from the comment")
    args = ap.parse_args()

    issues = [n.strip() for n in args.issues.split(",") if n.strip().isdigit()]
    if not issues:
        print("status_rollup: no tracked issues configured — no-op")
        return 0
    if not args.repo or not args.status.is_file():
        print(f"status_rollup: --repo/{args.status} unusable — no-op", file=sys.stderr)
        return 0

    status_md = args.status.read_text(encoding="utf-8")
    body = render_comment(status_md, args.run_url)
    for issue in issues:
        if not args.token:
            print(f"status_rollup: DRY RUN #{issue} — digest "
                  f"{'CHANGED' if should_post(None, status_md) else 'unchanged'}")
            continue
        last = fetch_last_rollup(args.repo, issue, args.token)
        if should_post(last, status_md):
            if post_comment(args.repo, issue, body, args.token):
                print(f"status_rollup: posted rollup to #{issue}")
        else:
            print(f"status_rollup: #{issue} outcomes unchanged — silent")
    return 0


if __name__ == "__main__":
    sys.exit(main())
