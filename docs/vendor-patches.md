# Dependency Patches — the #294 vendor-patch manifest (WS3)

This file is **the** per-fork manifest for every patched/forked dependency
that ships in a built image (stability-backlog #294 item 3). One row per dep
below, five required fields each: pin, fork delta, **runtime role** (what
breaks if it is removed), upstream status/PR, retirement trigger. The
#242 lesson it exists to prevent: describing a fork delta without its runtime
role is how "the knob doesn't exist" misses happen.

Scope split with `docs/SHARED-CRATES.md`: that file is the cross-repo kernel
**graph** SSOT (homes, consumer pins, bump rules); this file is the
**per-fork patch manifest** (deltas, runtime roles, retirement). Cross-repo
pins move under SHARED-CRATES bump rules; a fork's reason to exist is retired
under the triggers here.

## Manifest (all five patched/forked deps)

| dep | pin (declared where) | fork delta | runtime role — what breaks if removed | upstream status / PR | retirement trigger |
|---|---|---|---|---|---|
| `synopsys-usb-otg` | `Amperstrand/synopsys-usb-otg` @ `764bc042` — `[patch.crates-io]`, root `Cargo.toml`; lock `764bc042f63c…cd187` (v0.5.0) | 2 commits on upstream v0.5.0: `ffc83f8` F469 endpoint re-enablement (core_id `0x0000_1200`, `src/bus.rs`), `764bc04` generated-RAL warning suppression | USB device stack for the STM32F469 (`usb_fs`). crates.io 0.5.0 never enables endpoints for the F469 OTG core → the stm32f469 image boots but **never enumerates on USB: no CCID transport, host sees nothing** | No F469 feature upstream; crates.io stuck at 0.5.0 (verified 2026-10-18). No upstream PR yet — owner-gated; agents never post to stm32-rs | Upstream release ships F469 support → drop the `[patch]`, ride crates.io. Gate: full STM32 build matrix + HIL green first. Deletion condition also documented at the patch entry in root `Cargo.toml` |
| `stm32f469i-disc` | path patch `[patch."https://github.com/Amperstrand/stm32f469i-disc.git"]` → `vendor/stm32f469i-disc` (plain tracked tree); dep declared as git `branch = "sdio-support"` in `firmware/ccid-firmware/Cargo.toml` (feature `stm32f469`) | ⚠️ STRANDED SNAPSHOT of the fork's **deleted** `sdio-support` branch (fa6dc86 era) + HAL dep realigned to `05d999d` (fork `main` pins ancestor `789e5e8`; two HAL instances broke display-init `Pin`/`Rcc` types, #193) | F469I-DISCO BSP: display/framebuffer + SDIO bring-up for the `stm32f469` feature. Removed → the declared branch no longer exists on the remote, so **cargo metadata fails outright** (the empty-`vendor/` CI failure class) — no stm32f469 display build at all | Fork branch is gone; this vendor copy is the SOLE PROVIDER. Clean path (bench-gated) in `docs/VENDOR_PATCH_AUDIT.md` (#2) + issue #265: branch off fork `main` with the HAL-pin bump, port the display feature, bench-verify per #193, repoint the dep | That clean-path branch lands bench-verified → repoint dep, then **delete this patch + the whole `vendor/` tree** in the same change |
| `stm32f4xx-hal` | `Amperstrand/stm32f4xx-hal` @ `05d999d6` — plain rev-pinned git dep (no `[patch]`), `firmware/ccid-firmware/Cargo.toml` (feature `stm32f469`: `stm32f469`,`usb_fs`,`framebuffer`); lock `05d999d600d4…98908` (v0.23.0) | 8 commits over stm32-rs `master`: DSI/LTDC work (`583a8f2` DSI ctor + framebuffer `DrawTarget`, `ec6dc08` race-free `swap_buffers` w/ VBR-wait, `b72958b` PLLSAI/R pixel clock, `0aa1b1d` missing DSI write cmds), `789e5e8` synopsys-usb-otg 0.5.0 bump, `4defe54` time-crate pin, docs | The HAL under the **entire** stm32f469 firmware: display (DSI/LTDC framebuffer), USB, SDIO. Swapped for crates.io → the release has none of the DSI/LTDC fixes, **F469 display init breaks** (no `DrawTarget`/DSI ctor, wrong pixel clock, tear/race); and any rev other than `05d999d` next to the BSP re-triggers the two-HAL-instance `Pin`/`Rcc` type clash (#193) | stm32-rs carries none of the DSI/LTDC work; upstream the LTDC fixes opportunistically (audit #3). **Unified pin** with bolty-rs — both locks must agree (drift-check UNIFIED row) | stm32-rs ships the DSI/LTDC fixes in a release that meets the BSP/feature needs → re-pin to registry in **one wave with bolty** (SHARED-CRATES bump rule 4). Until then: keep fork; this git-dep shape is the target form for all deltas |
| `mfrc522` | `Amperstrand/mfrc522-rs` @ `e9ced1e` — plain git dep (not vendored since #19), `firmware/esp32-ccid/Cargo.toml` (optional; `backend-mfrc522`); lock `e9ced1e2acc5…92aff` (v0.8.0) | Amperstrand **root repo, the canonical home** (B28 single-home doctrine), not a GitHub fork: 0.8.0 base + `e9ced1e` software iteration cap on wait loops (esp32-safe blocking) + eh02 test repair; `bc3ebfb` licensing/metadata | MFRC522 CDC backend driver — `backend-mfrc522` is in esp32-ccid's **default** features (`default = ["backend-mfrc522","board-m5atom"]`). Removed → **default esp32 builds stop compiling**; the MFRC522 reader lane (m5atom/m5stick wifi-vpcd, `mfrc522_bring_up`) loses its PCD driver. Also the sole heapless-0.8 consumer left (deny.toml duplicate note) | It IS the upstream/home, shared canonically with bolty-rs (**UNIFIED** pin, drift-check row); optionally offer the wait-loop cap to the historical upstream crate | None planned — canonical home. If ever superseded (M2 `mfrc522-pcd` migration, epic `t_506dfe03`, or an upstream release carrying the cap), the unified pin moves in one wave with bolty |
| `iso14443` | `Amperstrand/iso14443-rs` @ `941131eb6d0f…d6036` (branch `ai-experiments`) — plain git dep, `firmware/esp32-ccid/Cargo.toml` (optional; `backend-mfrc522` pulls it, features `alloc`) | Fork of canonical `Foundation-Devices/iso14443-rs`: NTAG424 S(WTX) handling, chain-recovery re-send, block-number toggle fixes, lint/licensing + `PcdSession::limits/set_limits` (fork PR #11, merged) — the limits API `mfrc522-pcd` @ `519823c` is bumped against (#376/#372) | ISO-14443 protocol layer for the MFRC522 backend (framing, `PcdSession` WTX/limits). Removed → `backend-mfrc522` fails to resolve; substituting canonical upstream alone also breaks the build — **`PcdSession::limits/set_limits` does not exist there**, so the consumer session layer won't compile | **INTENTIONAL SPLIT-PIN** (drift-check ISO row reports for awareness, not failure): ccid → Amperstrand fork `941131eb`; bolty → canonical `Foundation-Devices` `216d1b9` (fork-pin branch `chore/pin-iso14443-941131eb`). Fork stays canonical for both until protocol fixes go upstream; re-pin history #348, latest bump #376/#372 | Foundation-Devices accepts the protocol fixes + limits API in a release → re-pin ccid **and** bolty to canonical in one wave; the split-pin row disappears |

## Drift-check cross-check (crate-name SSOT)

Crate names above are cross-checked against `ops/shared_crates_drift.py`
(weekly sweep; report `ops/reports/shared-crates-drift.txt` in the ops repo):

- `mfrc522`, `stm32f4xx-hal` — **UNIFIED** rows: same rev required in both
  repos' `Cargo.lock`.
- `iso14443` — **ISO** split-pin row: intentional ccid-fork vs bolty-canonical.
- `synopsys-usb-otg`, `stm32f469i-disc` — deliberately **absent** from the
  script's registry: repo-scoped ccid-only deps, not cross-repo shared crates
  (matches the "repo-scoped, not shared" section of `docs/SHARED-CRATES.md`).

If a dep moves between those classes, update this file, `docs/SHARED-CRATES.md`,
and the script registry in the same session (the script mirrors the doc).

## Git-fork patch: synopsys-usb-otg (retired from vendor/ — #264)

- **Type**: Git dependency (fork of `stm32-rs/synopsys-usb-otg`)
- **Source**: `https://github.com/Amperstrand/synopsys-usb-otg` @ `764bc042`
- **Used by**: STM32 firmware (via `[patch.crates-io]` in root `Cargo.toml`)
- **Upstream status**: No F469 feature upstream; crates.io stuck at 0.5.0 (verified 2026-10-18)

### Fork delta (2 commits on top of upstream v0.5.0)

1. `ffc83f8` feat: add STM32F469 support — re-enables endpoints for core_id `0x0000_1200` in `src/bus.rs`
2. `764bc04` fix: suppress warnings from generated RAL code — `#![allow(unexpected_cfgs)]`, `unused_imports`, `dead_code` in `src/lib.rs`

### Lifecycle

- **Current stage**: fork/git dep (vendoring-policy class 2 — the patch can ride a fork)
- **Next stage**: upstream PR of the F469 delta (owner-gated; agents never post to stm32-rs)
- **Final stage**: plain crates.io dep when upstream releases F469 support

### Upgrade trigger

If crates.io publishes a version with STM32F469 support, drop the patch entirely and let the crates.io version resolve. Verify with the full STM32 build matrix + HIL before removing.

## Vendor-path patch: stm32f469i-disc (stranded snapshot)

- **Type**: Path patch (`[patch."https://github.com/Amperstrand/stm32f469i-disc.git"]`)
- **Used by**: STM32F469 firmware display
- **Status**: ⚠️ STRANDED SNAPSHOT — the fork's `sdio-support` branch no longer exists; this vendor copy is the sole provider. Clean path documented in `docs/VENDOR_PATCH_AUDIT.md` (#2) and issue #265.

## Git-fork dep: stm32f4xx-hal (no `[patch]` entry — the target shape)

Rev-pinned git dep on the Amperstrand fork of `stm32-rs/stm32f4xx-hal`
(see manifest row above for delta/role/trigger). Audited as #3 in
`docs/VENDOR_PATCH_AUDIT.md`: "this git-dep shape (no `[patch]`) is the
target form for all deltas". Display + USB + SDIO features ride it; the BSP
vendor copy is HAL-realigned to exactly this rev (#193).

## Git-fork dep: mfrc522 (canonical home, shared with bolty-rs)

Plain git dep on `Amperstrand/mfrc522-rs` @ `e9ced1e` — the former divergent
`vendor/mfrc522` copy was removed (issue #19); this repo is the crate's
canonical home under the B28 single-home doctrine and bolty-rs consumes the
identical rev (**unified pin** — see `docs/SHARED-CRATES.md` bump rule 4).

## Git-fork dep: iso14443 (intentional split-pin vs bolty)

Plain git dep on `Amperstrand/iso14443-rs` @ `941131eb` (`ai-experiments`
branch) — the former `vendor/iso14443-rs` copy was removed (issue #6).
bolty-rs pins canonical upstream `Foundation-Devices/iso14443-rs` instead;
the split is **intentional** (fork carries the NTAG424 protocol fixes +
`PcdSession::limits/set_limits`, fork PR #11) and the drift check reports it
for awareness only. Re-pin history: #348; latest bump #376/#372.

## General

Before updating any patched dependency, run the full test suite (`cargo test --workspace --target x86_64-unknown-linux-gnu`) and verify on hardware.
