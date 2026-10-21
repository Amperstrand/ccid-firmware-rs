# synopsys-usb-otg — upstreamable series for the F469 delta (issue #271-A)

The fork `Amperstrand/synopsys-usb-otg` @ `764bc042` (pinned via
`[patch.crates-io]`, root `Cargo.toml`) carries exactly two commits on
top of upstream. `series/` reproduces that delta as a paste-ready,
`git am`-able series:

- `0000-cover-letter.patch` — cover letter (not applied by `git am`)
- `0001-Re-enable-endpoints-on-STM32F469-after-OUT-transacti.patch` —
  the F469 fix: core_id `0x0000_1200` joins the F446-class endpoint
  re-enablement condition in `src/bus.rs` (3 functional lines)
- `0002-Suppress-warnings-from-generated-RAL-code.patch` — crate-level
  allows for the generated RAL warnings in `src/lib.rs`

Commit messages are upstream-style (short imperative subject, rationale
body — cf. upstream's own "Re-enable endpoint after reading GRXSTSP"),
authored as the org noreply identity; the owner re-authors with their
own identity when actually sending.

## Recorded pins (verify before sending)

| what | value (verified 2026-10-21) |
|---|---|
| upstream base | `f5ed46479e742e0d9441ae321a2a2f2a15e397d0` (= tag `v0.5.0` = `stm32-rs/master` tip) |
| fork delta | `ffc83f8` (F469) + `764bc04` (RAL warnings) on that base |
| fork master tree | `6f5ee395420ebe2e2cae9d4006e0132680b6ef57` — **must** equal the series result |
| fork pin downstream | `764bc042f63cf00d0efd2417141438c1584cd187` |

Verified this session: series applies (`git apply --check` = 0, `git am`
= 0) onto the base and yields tree `6f5ee395…` byte-identical to fork
master — the series is the fork delta, not an approximation of it.

## Upstreaming checklist (in order)

1. **Re-check the base**: `git ls-remote
   https://github.com/stm32-rs/synopsys-usb-otg refs/heads/master`.
   If it moved past `f5ed464…`, rebase the series onto the new tip and
   re-run `vendor-patches/verify.sh synopsys` before doing anything
   else.
2. **Re-run the mechanical check**: `./verify.sh synopsys` (from this
   directory's parent). Green = the series still is the fork delta.
3. **OWNER STEP — channel rule** (agents never post outside
   Amperstrand/OpenTollGate): open the PR
   `Amperstrand:master → stm32-rs:synopsys-usb-otg:master` and lead the
   text with the evidence line: *"STM32F469I-DISCO: core_id
   0x0000_1200 requires F446-like endpoint re-enablement after OUT
   transactions"* (per `docs/VENDOR_PATCH_AUDIT.md` §1). Attach this
   series; record the PR number in the manifest row
   (`docs/vendor-patches.md`, upstream-status column).
4. **On an upstream release that contains it**: drop the
   `[patch.crates-io]` entry in root `Cargo.toml` (deletion condition is
   written at that entry), re-pin crates.io, gate = full STM32 build
   matrix + HIL green — and delete this directory in the same change.

## What blocks upstreaming today

- `0001` (F469): evidence is a single bench (STM32F469I-DISCO,
  Amperstrand ccid-firmware #193/#271) — upstream has no F469 CI or
  hardware coverage, so a maintainer must accept the core-behavior
  claim on reported evidence; the magic-number `core_id` chain may
  attract a refactor request (cosmetic, harmless to the fix). No API
  drift: the base is the current upstream master tip.
- `0002` (RAL warnings): crate-wide `#![allow]`s are likely to be
  contested — upstream may prefer targeted `#[allow]`s or a
  generator-side fix. The patch is explicitly droppable from the
  series (`0001` does not depend on it); if contested, drop it and keep
  it fork-local instead of burning review goodwill.

## Retirement trigger wiring

Manifest row (`docs/vendor-patches.md`) → "Upstream release ships F469
support → drop the `[patch]`, ride crates.io. Gate: full STM32 build
matrix + HIL green first." This directory is the artifact that trigger
deletes; `verify.sh synopsys` failing (series no longer applies onto
the recorded base) is the signal to rebase before the owner pastes.
