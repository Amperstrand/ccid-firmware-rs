# Branch Cleanup Audit

**Date**: 2026-05-02
**Auditor**: Sisyphus (automated)
**Base branch**: main @ f85c0c6

## Branch Inventory

| Branch | Type | HEAD | Last Updated | Classification |
|--------|------|------|--------------|----------------|
| `main` | Local + Remote | f85c0c6 | 2026-05-02 | Active |
| `sisyphus/finish-shared-ccid-core` | Local | f85c0c6 | 2026-05-02 | Active (current work) |
| `origin/copilot/add-nfc-only-build-variant` | Remote | 5bc0760 | 2026-04-13 | Stale |

## Detailed Analysis

### `origin/copilot/add-nfc-only-build-variant`

- **Ahead of main**: 2 commits from merge base (6dbf54c)
- **Behind main**: 53 commits
- **Classification**: Stale / Do Not Merge

**Changed files (2 commits):**

| File | Size | Description |
|------|------|-------------|
| `src/main_nfc.rs` | 209 lines | ESP32-S3 entry point template |
| `src/nfc/mod.rs` | 289 lines | NFC backend module |
| `src/nfc/pn532.rs` | 653 lines | PN532 SPI driver |
| `src/nfc/pn532_driver.rs` | 255 lines | SmartcardDriver impl |
| `Cargo.toml` | — | Adds "nfc" feature + ccid-nfc binary |
| `BUILDING.md` | +154 lines | NFC build documentation |

**Why it cannot be merged:**

1. Adds files to `src/` that conflict with the STM32-focused structure
2. Main has since adopted a separate workspace crate (`esp32-ccid/`) architecture
3. The feature-flag approach (NFC as a feature on STM32 crate) was abandoned in favor of separate crates
4. 53 commits of divergence makes a clean merge impossible

**What was already implemented differently in main:**

- ESP32 support via `esp32-ccid/` workspace crate (not feature flags)
- PN532 NFC driver: `esp32-ccid/src/pn532_driver.rs`
- MFRC522 NFC backend: `esp32-ccid/src/mfrc522_driver.rs`
- ISO-DEP/ISO 14443 protocol support
- Full CCID handler: `esp32-ccid/src/ccid_handler.rs`
- GemPC Twin serial framing: `esp32-ccid/src/serial_framing.rs`

**Salvageable items (low value):**

- BUILDING.md wiring diagrams and quick-start instructions — compare against `esp32-ccid/README.md` before discarding
- `src/main_nfc.rs` has architecture TODO comments — mostly outdated

**Recommendation**: Safe to delete after verifying no unique documentation is lost.

### `sisyphus/finish-shared-ccid-core`

- **Ahead of main**: 0 commits (branched from current main HEAD)
- **Behind main**: 0 commits
- **Classification**: Active working branch

This is the current development branch for the shared CCID architecture refactoring (Phases 4-5). Will be merged to main via PR when complete.

## Recommendations

| Branch | Action | Priority |
|--------|--------|----------|
| `sisyphus/finish-shared-ccid-core` | Keep — active work | — |
| `origin/copilot/add-nfc-only-build-variant` | Delete remote after PR review | Low |
| `sisyphus/finish-shared-ccid-core` (after merge) | Delete local | After PR merged |


## Sweep 2026-10-17 (#166, ops5-orc S5wC)

Main @ 718e014. 23 branches deleted — every tip verified
`git merge-base --is-ancestor <tip> origin/main` before deletion
(journal of tip SHAs in issue #166). Audit discipline note: the first
pass produced false "merged" verdicts from a STALE origin/main ref
(4afa3e5 vs 718e014) — always `git fetch <remote> refs/heads/main:
refs/remotes/origin/main` before ancestry claims; a plain
`fetch --prune` does not guarantee the tracking ref moves.

KEPT (3 superseded/preserved, do not auto-delete):
- `sisyphus/audit-wave4` — 3 commits, the DECISION_REGISTER D-8
  preserved-evidence branch (citation anchor).
- `sisyphus/181-per-command-windows` — 1 commit ahead (#204 Codex P1
  variant, superseded by the landed implementation; end-state far
  behind main).
- `sisyphus/lcd-ticker-split` — 5 commits (#215 pre-squash variant;
  the squash landed different final patches).

Worktrees: `/tmp` farm (pr82/idf555/pr158-161) already gone from prior
sweeps; `ccid-c3-wt` (May c3-port lane, 0 unique commits, local-only
branch) removed this sweep. Remaining: the hub + tonight's open-PR
lanes.

## Sweep 2026-10-28 (#476 — remote), same-session local-farm pass

Base: main @ cff1e15d (post #464/#465/#452 merges). Fresh
`git fetch origin refs/heads/main:refs/remotes/origin/main` before
every claim, per the #166 discipline.

**Remote: 8 branches deleted, every tip archived as
`archive/2026-10-28/<name>` tags** (journal + per-branch verdicts in
issue #476; ancestry was false for all eight — squash/re-merged
landing — so verdicts are CONTENT verifications: per-file tip-vs-main
diffs + signature greps). Deleted:
`wave1/issue-262-defmt-refresh` (NOT landed, WIP, issue #262 points at
the tag), `sisyphus/fix275-lcd` (landed-equivalent, #275 open for the
hardware half), `sisyphus/fix276-pin-counters`,
`sisyphus/post279-codex-p2`, `sisyphus/p3-hil-cadence`,
`bench/battery-pty-tolerance`, `window2/sweep-404`,
`sisyphus/ifd-nucula-hardening` (all landed). KEPT: `main`,
`bench/status` (hil-bench nightly publish target — never a work
branch), `fix/473-uart-serve-evidence` (open PR #474).

Companion bolty sweep same day (issue bolty-rs#238): 7 archived+
deleted (pin-churn trio + fuzz-start + fork-feeding-prep + merged
hce-card-registry), remote down to `main` + the active
`fix/h1-codex-233-235` head.

Local farm (this checkout): 8 stale worktrees removed; 5 branches
already merged into main deleted outright, 3 local-only unmerged lanes
archived as `archive/2026-10-28/local-*` tags and pushed before
worktree removal (`refactor/ccid-service-crate` +7 — superseded: main
carries crates/ccid-service via a different landing; `ccid-service-finish`
+10; `feat/m3-product-presets` +2). The unpushed-work rule (host
hygiene §2) is satisfied by the pushed tags.

Search-and-destroy class added this sweep: plain `git fetch --prune`
reported 65 stale bolty remote refs the branch list was still carrying
— run `git fetch --prune` BEFORE counting branches or the inventory
lies about its own size.
