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
