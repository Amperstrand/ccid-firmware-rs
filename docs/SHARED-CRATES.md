# SHARED-CRATES — the cross-repo kernel crate graph (SSOT)

One kernel, two repos (strategy 2026-10-20, epic `t_506dfe03` on the ops
kanban): `ccid-firmware-rs` is the embedded platform home; `bolty-rs` is the
standalone-mode product. Crates are **homed** in exactly one repo and consumed
from the other via **rev-pinned git deps** — today the pins point at each other
(`ccid → bolty@2a1afea`, `bolty → ccid@0b8e33e`), which is exactly why drift
must be checked mechanically, not remembered.

This file is the map. **The map is checked**: `ops/shared_crates_drift.py`
runs in the weekly ops sweep (Mondays) and compares every consumer pin against
its home repo's default-branch HEAD; the report lands in
`ops/reports/shared-crates-drift.txt`. Pins below are illustrative of the
2026-10-21 state — the script, not this prose, is the live truth.

## Kernel crates (one home each)

| crate | home | consumed by | consumer pin (2026-10-21) | notes |
|---|---|---|---|---|
| `pn7160-nci` (incl. `hal_i2c`) | ccid `crates/pn7160-nci` | bolty `crates/bolty-pn7160` | `ccid@0b8e33e` | PN7160 NCI frontend + shared `driver_ng` embedded-hal I2C adapter (Write-run coalescing, #321) |
| `mfrc522-pcd` | bolty `crates/mfrc522-pcd` | ccid `firmware/esp32-ccid` (optional) | `bolty@f9fb57b7` (branch `chore/pin-iso14443-0f7d9d70`) | MFRC522 PCD layer. **M2: migrates to ccid** (one-way kernel). 2026-10-21: bumped to iso14443@941131eb (limits API) for #376/#372 |
| `pn532-transport` | bolty `crates/pn532-transport` | ccid `firmware/esp32-ccid` (optional) | `bolty@2a1afea` | PN532 transport. **M2: migrates to ccid** |

## Protocol layer

| crate | pins (2026-10-21) | notes |
|---|---|---|
| `iso14443` | ccid: `Amperstrand/iso14443-rs@941131eb` (fork default branch `ai-experiments`; adds `PcdSession::limits/set_limits`, PR #11) · bolty: `Foundation-Devices/iso14443-rs@216d1b9` (canonical upstream) · bolty fork-pin branch: `941131eb` via `chore/pin-iso14443-941131eb` | bolty's session API + WTX hook live consumer-side in `bolty/crates/iso14443-session`; re-pin history in #348. Split-pins are INTENTIONAL — the drift check reports them for awareness, not as failures |
| `ntag424` | bolty: `Amperstrand/ntag424@02f23f5` | bolty-only today; SDM + key diversification features |

## Vendor / driver forks (repo-scoped, not shared)

Per-fork fields — fork delta, **runtime role**, upstream status/PR, retirement
trigger — live in the #294 vendor-patch manifest: `docs/vendor-patches.md`.
This table is the pin map only.

| dep | repo | pin | scope |
|---|---|---|---|
| `synopsys-usb-otg` | `Amperstrand/synopsys-usb-otg` | `764bc042` | ccid only (F469 USB; deletion condition in root `Cargo.toml` — drop when upstream ships F469) |
| `stm32f4xx-hal` | `Amperstrand/stm32f4xx-hal` | `05d999d6` | bolty (unified-family pin) |
| `mfrc522` (driver crate) | `Amperstrand/mfrc522-rs` | `e9ced1e2` | bolty dependency; same rev allow-listed in ccid `deny.toml` — **unified pin**: both sides move together |
| `ft6x06` | `DougAnderson444/ft6x06` (upstream) | `cc352f80` | ccid only (touch) |

## Bump rules (fork-policy §2 style, both repos)

1. **Rev pins are immutable.** A rev-pinned build must resolve to the same
   bytes forever; never let `cargo update` float a git dep (the unpinned
   entry from bolty #105 is the precedent this rule comes from).
2. **A bump is one reviewed commit** that moves the `rev` in `Cargo.toml`
   AND the matching `Cargo.lock` entry together — never one without the other.
3. **Home lands first, consumers follow**: merge the home-repo PR, then each
   consumer bumps its pin in a single commit whose message quotes the home
   PR number.
4. **Unified pins move in one wave**: `mfrc522` and `stm32f4xx-hal` must land
   at the same rev in both repos within the same wave (the drift check flags
   disagreement).
5. **Direction of homing only changes with the kernel strategy** (M2 one-way
   migration, epic `t_506dfe03`) — never as a side effect of a feature PR.

## Drift check

- Script: `shared_crates_drift.py` in the [ops repo](https://github.com/Amperstrand/ops),
  run by `.github/workflows/weekly-sweeps.yml` (Mondays 06:33 UTC), report
  committed to `ops/reports/shared-crates-drift.txt`.
- What it flags: consumer pin ≠ home default-branch HEAD (home has moved on),
  missing/unpinned entries (FETCH-FAIL / UNPINNED), and unified-pin
  disagreement across repos.
- Run locally any time: `python3 shared_crates_drift.py` (needs `gh` auth).
