#!/usr/bin/env python3
"""ai-legion host hygiene audit (issue #108): report, never delete.

The two standing risks from the 2026-10-10 disk-full incident, as code:

1. Shared CARGO_TARGET_DIR growth — /root/.cargo-target is pinned
   host-wide by /root/.cargo/config.toml and nothing evicts from it.
   Reports the prunable candidates (incremental, stale IDF copies,
   re-downloadable tarballs) against the manifest pin.

2. Session worktree farm — /tmp/opencode worktrees carrying branches
   or detached commits that exist ONLY on this disk. Every branch is
   classified against the remote (UNPUSHED / MERGED-LABEL / DIRTY),
   the exact audit AGENTS.md mandates before any disk cleanup.

Read-only by design (the lobehub cleanup.sh doctrine): pruning and
worktree removal are owner-executed. Exit 1 only when something needs
action NOW (unpushed work found, or disk >= 90%); advisory findings
(stale-but-prunable artifacts, dirty-but-pushed trees) exit 0.

Usage:
    python3 scripts/host_hygiene.py                 # bench defaults
    python3 scripts/host_hygiene.py --json          # machine-readable
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]

def _default_target_dir() -> Path:
    """Resolve the invoking user's real cargo target dir.

    The original /root/.cargo-target hardcode only fit the ai-legion
    root bench user — on any other account the audit died with
    PermissionError before reporting anything. The shared-target-dir
    pin (when present) lives in ~/.cargo/config.toml; fall back to the
    conventional per-user path, then ./target.
    """
    cfg = Path.home() / ".cargo" / "config.toml"
    if cfg.is_file():
        for line in cfg.read_text().splitlines():
            line = line.strip()
            if line.startswith("target-dir") and "=" in line:
                return Path(line.split("=", 1)[1].strip().strip('"'))
    per_user = Path.home() / ".cargo-target"
    if per_user.is_dir():
        return per_user
    return Path("/root/.cargo-target")


DEFAULT_TARGET_DIR = _default_target_dir()
DEFAULT_WORKTREE_ROOT = Path("/tmp/opencode")

# Protected branch names — never proposed for deletion, never counted as
# stale labels, regardless of how merged they look (a deleted remote on
# main is a repo migration, not a cleanup opportunity). Enforced here in
# code, not in the report text, so it cannot be argued away.
PROTECTED_BRANCHES = {"main", "master", "develop", "release"}

DISK_WARN_PCT = 85
DISK_RISK_PCT = 90

findings: list[dict] = []  # {severity, section, item, detail}
advisories: list[dict] = []
_json_mode = False


def say(msg: str = "") -> None:
    # Under --json the human report moves to stderr: stdout stays pure JSON.
    print(msg, file=sys.stderr if _json_mode else sys.stdout)


def sh(cmd: list[str], timeout: int = 60, cwd: Path | None = None) -> tuple[int, str]:
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, cwd=cwd)
    return r.returncode, (r.stdout + r.stderr).strip()


def du(path: Path) -> int:
    """Size in bytes via du (handles sparse/apparent sizes correctly)."""
    rc, out = sh(["du", "-sb", str(path)], timeout=300)
    if rc != 0:
        return 0
    return int(out.split()[0]) if out.split() else 0


def human(n: int) -> str:
    size = float(n)
    for unit in ("B", "K", "M", "G", "T"):
        if size < 1024 or unit == "T":
            return f"{size:.0f}{unit}" if unit in ("B", "K") else f"{size:.1f}{unit}"
        size /= 1024
    return f"{size:.1f}T"


def pinned_idf_version() -> str | None:
    """The esp-idf tag this repo pins (firmware/esp32-ccid/Cargo.toml)."""
    manifest = REPO_ROOT / "firmware" / "esp32-ccid" / "Cargo.toml"
    if not manifest.exists():
        return None
    m = re.search(r'esp_idf_version\s*=\s*"tag:(v[0-9.]+)"', manifest.read_text())
    return m.group(1) if m else None


def audit_disk(paths: list[Path]) -> None:
    say("== disk pressure ==")
    seen_fs = set()
    for p in paths:
        if not p.exists():
            say(f"  [----] {p} (absent — non-bench host?)")
            continue
        usage = shutil.disk_usage(p)
        pct = usage.used * 100 // usage.total
        fs_key = (usage.total, usage.free)  # crude same-filesystem dedupe
        mark = "OK  " if pct < DISK_WARN_PCT else ("WARN" if pct < DISK_RISK_PCT else "RISK")
        line = f"  [{mark}] {p}: {human(usage.used)} / {human(usage.total)} ({pct}%)"
        if fs_key in seen_fs:
            line += "  [same fs]"
        say(line)
        seen_fs.add(fs_key)
        if pct >= DISK_RISK_PCT:
            findings.append({"severity": "risk", "section": "disk",
                             "item": str(p), "detail": f"{pct}% used"})
        elif pct >= DISK_WARN_PCT:
            advisories.append({"section": "disk", "item": str(p),
                               "detail": f"{pct}% used"})


def audit_cargo_target(target: Path) -> None:
    say("== cargo target dir ==")
    if not target.exists():
        say(f"  [----] {target} absent — skipping")
        return

    total = du(target)
    say(f"  total: {human(total)}")

    incremental = target / "debug" / "incremental"
    inc_size = du(incremental) if incremental.exists() else 0
    if inc_size:
        advisories.append({"section": "cargo-target", "item": str(incremental),
                           "detail": f"{human(inc_size)} — always safe to delete, regenerated"})
        say(f"  [PRUN] debug/incremental: {human(inc_size)} (always safe)")

    dist = target / ".embuild" / "espressif" / "dist"
    dist_size = du(dist) if dist.exists() else 0
    if dist_size:
        advisories.append({"section": "cargo-target", "item": str(dist),
                           "detail": f"{human(dist_size)} toolchain tarballs — re-downloadable"})
        say(f"  [PRUN] .embuild/espressif/dist: {human(dist_size)} (re-downloadable)")

    pinned = pinned_idf_version()
    idf_root = target / ".embuild" / "espressif" / "esp-idf"
    if idf_root.exists():
        for vdir in sorted(idf_root.iterdir()):
            if not vdir.is_dir():
                continue
            if pinned and vdir.name == pinned:
                say(f"  [KEEP] esp-idf/{vdir.name}: {human(du(vdir))} (manifest pin)")
            else:
                advisories.append({
                    "section": "cargo-target", "item": str(vdir),
                    "detail": f"{human(du(vdir))} — stale "
                              f"(manifest pins {pinned or '?'})"})
                say(f"  [PRUN] esp-idf/{vdir.name}: {human(du(vdir))} "
                      f"(stale, pin={pinned or '?'})")

    for bench_bin in sorted(target.glob("esp32-ccid-*.bin")):
        say(f"  [KEEP] {bench_bin.name} (bench recovery image)")


def git_branches(git_dir: Path) -> list[tuple[str, str]]:
    """(branch, tip-sha) for local branches of a repo."""
    rc, out = sh(["git", "for-each-ref", "refs/heads",
                  "--format=%(refname:short) %(objectname)"],
                 cwd=git_dir)
    if rc != 0:
        return []
    rows = []
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 2:
            rows.append((parts[0], parts[1]))
    return rows


def remote_contains(git_dir: Path, sha: str) -> bool:
    rc, out = sh(["git", "branch", "-r", "--contains", sha], cwd=git_dir)
    return rc == 0 and bool(out.strip())


def classify_branch(git_dir: Path, branch: str, tip: str) -> dict | None:
    """UNPUSHED / MERGED-LABEL / TRACKED classification for one branch."""
    if branch in PROTECTED_BRANCHES:
        return None
    rc, out = sh(["git", "rev-list", "--count", f"{branch}@{{u}}..{branch}"],
                 cwd=git_dir)
    if rc == 0:
        ahead = int(out or "0")
        if ahead > 0:
            return {"branch": branch, "state": "UNPUSHED",
                    "detail": f"{ahead} commit(s) ahead of upstream"}
        return {"branch": branch, "state": "TRACKED",
                "detail": "in sync with upstream"}
    if remote_contains(git_dir, tip):
        return {"branch": branch, "state": "MERGED-LABEL",
                "detail": "tip on a remote — `git branch -d` candidate"}
    return {"branch": branch, "state": "UNPUSHED-NO-REMOTE",
            "detail": "tip exists nowhere outside this host"}


def audit_worktrees(root: Path) -> None:
    say("== worktree farm ==")
    if not root.exists():
        say(f"  [----] {root} absent — skipping")
        return

    repos: dict[Path, list[Path]] = {}
    detached: list[tuple[Path, str]] = []
    dirty: list[Path] = []

    for wt in sorted(root.iterdir()):
        git_marker = wt / ".git"
        if not wt.is_dir() or not git_marker.exists():
            continue
        rc, top = sh(["git", "rev-parse", "--show-toplevel"], cwd=wt)
        if rc != 0:
            continue
        toplevel = Path(top.splitlines()[0])
        # Worktrees of one repo each report a DIFFERENT --absolute-git-dir
        # (<main-repo>/.git/worktrees/<name>); the COMMON dir is the repo
        # identity — keying by it audits the branch list exactly once.
        rc, gitdir = sh(["git", "rev-parse", "--git-common-dir"], cwd=wt)
        if rc != 0:
            continue
        common = Path(gitdir.splitlines()[0])
        if not common.is_absolute():
            common = (toplevel / common).resolve()
        repos.setdefault(common, []).append(toplevel)

        rc, branch = sh(["git", "branch", "--show-current"], cwd=wt)
        if rc == 0 and not branch:
            rc2, sha = sh(["git", "rev-parse", "HEAD"], cwd=wt)
            if rc2 == 0:
                detached.append((toplevel, sha.splitlines()[0][:12]))
        rc, st = sh(["git", "status", "--porcelain"], cwd=wt, timeout=120)
        if rc == 0 and st:
            dirty.append(toplevel)

    say(f"  {len(repos)} repos, {sum(len(v) for v in repos.values())} checkouts "
          f"under {root}")

    for git_dir, toplevels in sorted(repos.items(), key=lambda kv: str(kv[0])):
        wt_names = ", ".join(t.name for t in toplevels)
        for branch, tip in git_branches(git_dir):
            cls = classify_branch(git_dir, branch, tip)
            if cls is None:
                continue
            mark = "RISK" if cls["state"].startswith("UNPUSHED") else (
                "OK  " if cls["state"] == "TRACKED" else "NOTE")
            say(f"  [{mark}] [{wt_names}] {branch}: {cls['state']} — {cls['detail']}")
            if cls["state"].startswith("UNPUSHED"):
                findings.append({"severity": "risk", "section": "worktrees",
                                 "item": f"{wt_names}:{branch}",
                                 "detail": cls["detail"]})
            elif cls["state"] == "MERGED-LABEL":
                advisories.append({"section": "worktrees",
                                   "item": f"{wt_names}:{branch}",
                                   "detail": cls["detail"]})

    for toplevel, sha in detached:
        git_dir = next((g for g, ts in repos.items() if toplevel in ts), None)
        if git_dir and remote_contains(git_dir, sha):
            say(f"  [OK  ] [{toplevel.name}] detached @{sha}: on a remote")
        else:
            say(f"  [RISK] [{toplevel.name}] detached @{sha}: NOT on any remote")
            findings.append({"severity": "risk", "section": "worktrees",
                             "item": f"{toplevel.name} (detached @{sha})",
                             "detail": "commit exists only on this host"})

    for toplevel in dirty:
        advisories.append({"section": "worktrees", "item": toplevel.name,
                           "detail": "dirty working tree — commit or stash "
                                     "before removal"})
        say(f"  [NOTE] [{toplevel.name}] dirty working tree")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--target-dir", type=Path, default=DEFAULT_TARGET_DIR)
    parser.add_argument("--worktree-root", type=Path, default=DEFAULT_WORKTREE_ROOT)
    parser.add_argument("--json", action="store_true",
                        help="emit the findings/advisories as JSON on stdout")
    args = parser.parse_args()

    global _json_mode
    _json_mode = args.json

    audit_disk([args.target_dir, args.worktree_root])
    audit_cargo_target(args.target_dir)
    audit_worktrees(args.worktree_root)

    if args.json:
        print(json.dumps({"findings": findings, "advisories": advisories}, indent=2))

    say()
    if findings:
        say(f"HYGIENE: {len(findings)} ACTION-NEEDED — "
              + "; ".join(f"{f['item']}: {f['detail']}" for f in findings))
        return 1
    say(f"HYGIENE: no unpushed work, disk below {DISK_RISK_PCT}% — "
          f"{len(advisories)} advisory item(s) (prune/stale-label candidates)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
