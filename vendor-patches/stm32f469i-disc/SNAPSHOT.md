# stm32f469i-disc — registration of the stranded vendor snapshot (issue #271-B)

Lifecycle registration for `vendor/stm32f469i-disc` (the manifest row
in `docs/vendor-patches.md` stays the SSOT; this file is its evidence
base). The snapshot's home stays `vendor/` — that is the manifest's
convention for a path-patch BSP delta and the `[patch]` entry in root
`Cargo.toml` points there. This registration makes the snapshot a
**managed artifact** (pinned provenance + integrity manifest) instead
of a stray untracked-upstream tree.

## Status: ⚠️ sole provider (stranded snapshot)

The dependency is declared as git `branch = "sdio-support"` in
`firmware/ccid-firmware/Cargo.toml`, but that branch **is deleted from
the fork remote** (verified 2026-10-21 via `git ls-remote
https://github.com/Amperstrand/stm32f469i-disc` — remaining branches:
`main`, `greatspectations-setup`, `vls-defmt-optional`). The
`[patch."https://github.com/Amperstrand/stm32f469i-disc.git"]` path
patch is therefore the **sole provider** of the BSP: remove it and
`cargo metadata` fails outright (the empty-`vendor/` CI failure class).
A patch must inject a delta, never be the sole provider
(`docs/VENDOR_PATCH_AUDIT.md` policy §1) — this file exists until that
violation is resolved by the clean path below.

## Provenance (verified 2026-10-21)

| field | value |
|---|---|
| upstream fork | `https://github.com/Amperstrand/stm32f469i-disc` |
| snapshot base | `fa6dc86c887da4c6a567981207ebbf69027f880a` — "fix: align HAL revision with main project (789e5e8)", the tip-era commit of the since-deleted `sdio-support` branch (2026-03-16) |
| local realignment | HAL dep `789e5e8` → **`05d999d600d457f99aeb23ff93275d2c8f998908`** (this repo's `stm32f4xx-hal` pin) + `src/lcd.rs` adapted to that HAL's `DisplayController::new_dsi` API (HSE-frequency argument; the DISCO's fixed 8 MHz xtal) |
| vendoring artifacts | `+ .cargo-ok`, `− .gitignore` |
| total local delta vs base | 2 files, 41 diff lines (characterized exhaustively; nothing else differs) |

Why frozen here: this exact tree is the **#193 bench-proven** state
(display feature resurrected, 61 errors → zero, HIL + owner-eyes LCD
check). Fork `main` has since diverged (~855-line lcd delta,
ownership-based `ft6x06-rs` touch shim, its own HAL line `0c5bc3d` — a
descendant of our `05d999d` per DR-BSP-2026-10-18). **Do not swap
blind** — see the decision record on issue #271 and issue #265.

## Integrity

`SHA256SUMS` (this directory) pins every one of the 28 files of the
vendor tree (paths relative to the repo root). Re-verify:

```sh
./vendor-patches/verify.sh snapshot      # or: sha256sum -c vendor-patches/stm32f469i-disc/SHA256SUMS
```

Any edit to `vendor/stm32f469i-disc` — however small — must update
`SHA256SUMS` in the same change and say why (the snapshot is otherwise
append-only until retirement). Generated this session from the
pre-existing tree; the migration itself changed no byte (pre/post
checksums identical, see PR evidence).

## Retirement trigger (wired to the manifest row)

Clean path (bench-gated; issues #265 + #271-B, decision record
DR-BSP-2026-10-18): lift the snapshot to the fork as a branch
(`hal-05d999d`, owner-gated) → port the `display` feature onto fork
`main` per the #266 HAL-pin decision → bench-verify per #193 → repoint
the dep → **delete the `[patch]` entry, the whole `vendor/` tree, the
manifest row's stranded status, and this directory in the same change**
(~4k lines out). Until then this registration is the tripwire:
`verify.sh snapshot` failing in CI means the sole-provider tree
drifted.
