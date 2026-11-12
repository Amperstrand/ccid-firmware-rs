#!/usr/bin/env python3
"""Run a pcscd-disturbing command under the tollgate-lab pcscd domain.

Codifies the AGENTS doctrine (B32/tollgate-lab #15 lineage) that ccid
role switches, reflows, and soak cycles kept as prose: a pcscd restart
is a DOMAIN DISTURBANCE — cross-lane reader sessions (bolty HIL, tollgate
soaks, wallet-matrix) must drain first, and their holds must exclude us.
The domain file is /tmp/amperstrand-pcscd-domain.lock (NOT the legacy
/run/lock/bench-pcscd.lock the old prose referenced — that file never
excluded tollgate-domain holders; this helper unifies on the real one).

Usage:
    pcscd_disturbance.py --reason "role switch" -- systemctl restart pcscd

Semantics:
    - tollgate_lab absent (non-bench host): loud UNGUARDED warning, then
      the command runs anyway (setup.sh on a fresh box must work).
    - domain held after the 30s drain window: REFUSED, exit 3, holder
      printed — a live cross-lane session is not ours to kill.
"""

import subprocess
import sys

DRAIN_TIMEOUT_S = 30


def main() -> int:
    args = sys.argv[1:]
    if len(args) < 3 or args[0] != "--reason" or args[2] != "--":
        print(__doc__, file=sys.stderr)
        return 2
    reason, cmd = args[1], args[3:]
    try:
        from tollgate_lab import PcscdDomainHeldError, acquire_pcscd_disturbance
    except ImportError:
        print(f"pcscd-domain: tollgate_lab missing — UNGUARDED disturbance "
              f"({reason})", file=sys.stderr)
        return subprocess.run(cmd).returncode
    try:
        lock = acquire_pcscd_disturbance(
            project="ccid-firmware-rs", reason=reason, timeout_s=DRAIN_TIMEOUT_S,
        )
    except PcscdDomainHeldError as exc:
        print(f"pcscd-domain: REFUSED — {exc}", file=sys.stderr)
        return 3
    try:
        return subprocess.run(cmd).returncode
    finally:
        lock.release()


if __name__ == "__main__":
    raise SystemExit(main())
