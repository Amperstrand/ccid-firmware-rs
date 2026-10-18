# Vendor patch & fork audit — why each delta exists, and the clean end state

Written 2026-10-18 (post-#242, which retired the esp-idf-hal patch — the
first patch to complete the full lifecycle). Every claim below was
re-verified against the remotes/tree this session, not trusted from
comments (#110: docs lie until re-verified).

## The lifecycle pattern (what the history shows)

Every patch in this repo started life as **"unblock the hardware today"** —
a minimal delta to make the bench work — and the house pattern that
emerged is a four-stage lifecycle:

```
1. UNBLOCK      vendor/ or fork with the minimal delta, undocumented lifetime
2. CANONICAL    move to a named Amperstrand fork / repo, rev-pinned git dep
3. CONVERGE     migrate consumers so the delta shrinks to zero (or upstream it)
4. DELETE       drop the [patch] entry, ride crates.io; vendor tree deleted
```

Case studies already run:

| Delta | 1→2 | 2→3 | 3→4 |
|---|---|---|---|
| `vendor/mfrc522` | vendored copy | → canonical `Amperstrand/mfrc522-rs` git dep (#19) | — (fork is the home; see below) |
| `vendor/iso14443-rs` | vendored copy | → canonical `Amperstrand/iso14443-rs` git dep (#6) | — (fork is the home; see below) |
| `vendor/esp-idf-hal` | vendored copy | → fork `ai-experiments-0.47` (#195) | consumers migrated to driver_ng adapter → **[patch] deleted, crates.io again (#242)** |
| `amp-embedded-common` | extracted shared crates | reversed — dissolved back in-repo (necessity audit) | — |

**The esp-idf-hal case is the template**: the patch existed for exactly
one feature (`legacy-i2c`); when the last consumer moved to the shared
adapter, the patch resolved to nothing and died in a two-commit PR.

## Current inventory (verified 2026-10-18)

| # | Dependency | Shape | Delta vs upstream | Why it exists | Still needed? | Clean end state |
|---|---|---|---|---|---|---|
| 1 | `synopsys-usb-otg` | `[patch.crates-io]` → **submodule** @ `764bc04` (Amperstrand fork of stm32-rs) | **2 commits** on upstream v0.5.0: `ffc83f8` F469 support — **3 lines in `src/bus.rs`** (core_id `0x0000_1200` needs F446-like endpoint re-enable after OUT); `764bc04` RAL warning allow | upstream has **zero** F469 support; without it the F469-DISO does not enumerate USB at all | **YES** (hard) | upstream PR → stm32-rs; then drop submodule + patch, ride crates.io |
| 2 | `stm32f469i-disc` | `[patch."Amperstrand/stm32f469i-disc"]` → in-repo `vendor/` copy | vs fork `main`: **diverged code line** — our copy is a stranded snapshot of the **deleted `sdio-support` branch** (fa6dc86 era) + HAL-pin realignment to `05d999d`; fork `main` evolved separately (feature gates, docs, button module, embassy sibling, ~855-line lcd.rs delta) and pins the HAL at `0c5bc3d` | #193: the BSP's HAL pin created a second HAL instance → display-init `Pin`/`Rcc` type mismatch; the patch injects the coherent pin | **YES** — and fragile: the dep declares `branch = "sdio-support"` which **no longer exists on the remote**; the patch is the sole provider | migration: branch off fork `main` with the HAL-pin bump → port the display feature onto it → bench-verify (#193-class) → dep points at the branch, delete patch + vendor tree |
| 3 | `stm32f4xx-hal` | rev-pinned git dep (`05d999d`, Amperstrand fork of stm32-rs) — **no patch entry** | LTDC/DSI fixes (`b72958b` PLLSAI/R pixel clock, `ec6dc08` race-free `swap_buffers`), USB dep bump, time-crate pin | display bring-up needed working DSI/LTDC; not upstream | **YES** (display + USB + SDIO features ride it) | keep fork; upstream the LTDC fixes opportunistically; this git-dep shape (no `[patch]`) is the target form for all deltas |
| 4 | `mfrc522` | git dep `e9ced1e` (Amperstrand **root repo**, not a GitHub fork) | fork content: 0.8.0 base + esp32 wait-loop iteration caps, eh02 test repair, licensing | upstream crate lacked esp32-safe blocking behavior | YES (shared canonically with bolty-rs) | it IS the canonical home (B28 doctrine); optionally offer the wait-loop cap upstream |
| 5 | `iso14443` | git dep `268db79` (Amperstrand fork of `Foundation-Devices/iso14443-rs`) | NTAG424 S(WTX), chain-recovery re-send, block-number toggle fixes, lint/licensing | protocol bugs hit on real NTAG424 cards | YES | upstream the protocol fixes to Foundation-Devices when energy allows; fork stays canonical for both projects until then |

## What this means, per patch

### 1. `synopsys-usb-otg` — the cheapest permanent win on the board

The entire delta is **3 functional lines + 1 lint allow**. The fork's
`master` already carries both commits; upstream (`stm32-rs/synopsys-usb-otg`,
latest release v0.5.0, tip = the same release merge we sit on) has no F469
anywhere. **Owner action (external repo — channel rule, owner pastes):**
open a PR `Amperstrand:master → stm32-rs:master` — the commits are
`ffc83f8` + `764bc04`; PR text should lead with "STM32F469I-DISCO:
core_id 0x0000_1200 requires F446-like endpoint re-enablement after OUT
transactions". On the next upstream release: delete the submodule + the
`[patch.crates-io]` entry, bump the crates.io pin. Until then the
submodule+patch shape is correct (it keeps the delta visible and the
submodule pinned).

### 2. `stm32f469i-disc` — the fragile one (act on this first)

Two verified facts make this the priority:

- The declared dep branch `sdio-support` **is deleted from the remote**.
  Anyone who removes the patch (or the patch entry) without noticing gets
  an unresolvable dep — the patch is currently the *sole provider* of the
  BSP code, not a delta injector.
- Fork `main` has moved on (its own README: "Plan is to upstream fixes
  and improvements to stm32f4xx-hal and related crates once testing is
  complete"). Our vendor copy is frozen at the #193 bench-proven state.

**Clean path (bench-gated, one session):** push a branch off fork `main`
with the HAL pin bumped to `05d999d` (one hunk — coherence with this
repo's HAL dep), port this repo's `display` feature onto it, run the
#193 display verification (HIL + owner-eyes LCD check), then flip the
dep to the branch and delete `[patch]` + `vendor/stm32f469i-disc` (~4k
lines out of the tree). Do NOT swap blind: the display path was proven
against the snapshot's code, and `main`'s lcd/touch/sdio have diverged.

### 3–5. The fork layer — healthy, keep, upstream opportunistically

`stm32f4xx-hal` is the model: rev-pinned git dep, no patch table, delta
documented in AGENTS. The two NFC protocol forks are canonical shared
homes (bolty consumes the identical revs — B28 single-home doctrine).
None of these need action beyond upstreaming when convenient.

## Policy conclusion (for the vendoring section, AGENTS.md)

The AGENTS class-2 rule ("vendor/ only when the patch cannot live in a
fork") remains right, but the audit adds two teeth:

1. **A patch exists to inject a delta, never to be the sole provider.**
   The moment the upstream source of a patched dep disappears (as
   `sdio-support` did), the patch stops being a delta and becomes an
   untracked upstream — migrate or re-home deliberately.
2. **Every `[patch]` entry names its deletion condition** (the
   esp-idf-hal entry did this correctly: "Drop when upstream >=0.48
   ships legacy-i2c equivalents"; the synopsys entry's condition is
   "next upstream release containing F469 support"). Entries without a
   stated end state rot into the BSP's current shape.
