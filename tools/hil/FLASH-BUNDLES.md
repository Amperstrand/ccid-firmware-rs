# Flash bundles — `amperstrand.flash-bundle/1` tooling

Implementation of the [flash-via-labgrid design][design] §4 (kanban
`t_7a2ec5c3`, M1+software-M2). A **flash bundle** is the flashable
unit: a directory with a `manifest.json` + the target binaries, carrying
a machine-checkable statement of what a CONSISTENT flash set is — so the
mixed-family incident class (old v5.5.1 bootloader under v5.5.5 images,
OTA erases on a factory table, three provenances on one chip) is
refused structurally instead of by operator care.

[design]: ../../docs/FLASH-VIA-LABGRID-DESIGN.md

```
<bundles-root>/<board>/<bundle_id>/
  manifest.json          # schema amperstrand.flash-bundle/1
  bootloader.bin         # ESP32: always present (§4.4 check 6)
  partition-table.bin
  app.bin
```

`bundle_id` = sha256 over the canonical manifest JSON (volatile fields
`bundle_id`/`built_at` removed, keys sorted, no whitespace) — two builds
of the same inputs hash identically; any content change re-hashes.

## Tools

| Tool | Role |
|---|---|
| `tools/hil/make_bundle.py` | producer — parses the partition CSV, hashes targets, emits the manifest + bundle dir |
| `tools/hil/flash_bundle.py build …` | CLI passthrough to the producer (the seam `build.sh` will call) |
| `tools/hil/flash_bundle.py check <dir>` | §4.4 self-consistency, producer AND flasher side |
| `tools/hil/flash_bundle.py verify <dir> …` | §4.5 refusal matrix against a device inventory entry; on bless, emits the flash plan |

Python 3.12, stdlib only (matches the design's "20 lines with `csv` +
`int(x,0)`" hint; no labgrid/hardware imports — flashing itself stays
in `esp32_flash.py`, integration is a later slice).

## Usage

```sh
# produce a bundle from a built target dir (real bench example; the
# smoke run of 2026-10-23 used the m2-mfrc522 xtensa release dir):
tools/hil/flash_bundle.py build \
  --project esp32-ccid --board m5stick --chip esp32 \
  --layout-source firmware/esp32-ccid/partitions-ota.csv --layout-id ota:v1 \
  --app  target/xtensa-esp32-espidf/release/esp32-ccid.bin \
  --bootloader   target/xtensa-esp32-espidf/release/bootloader.bin \
  --partition-table target/xtensa-esp32-espidf/release/partition-table.bin \
  --esp-idf-version v5.5.5 --sdkconfig firmware/esp32-ccid/sdkconfig-xtensa.full \
  --baud-cap 115200 --out "$CARGO_TARGET_DIR/bundles"

# self-consistency (§4.4): digests, app-in-slot, CSV re-parse, DIO
# header, family, complete bl+PT+app set, chip-derived bootloader offset
tools/hil/flash_bundle.py check  <bundle-dir>

# bless-or-refuse (§4.5) against the device inventory; --json for the
# machine verdict, exit 0 = blessed, 1 = refused
tools/hil/flash_bundle.py verify <bundle-dir> \
  --device m5stick --inventory /var/lib/amperstrand-bench/device-inventory.json \
  [--targets app] [--erase-otadata-at 0x30000] [--json]
```

Derived, never declared: the bootloader offset comes from `--chip`
(`esp32` → `0x1000`, C3/S3/C2 → `0x0` — §4.4 check 7), the app offset
and every erase offset come from the PARSED partition CSV.

## Refusal matrix (§4.5)

Rows match top-down; the **full-bundle row wins over unknown inventory**
— a full bundle is the only path allowed to change family/layout and is
exactly the first-adoption/seed path.

| Request | Inventory says | Verdict | Reason code |
|---|---|---|---|
| full bundle (bl+PT+app) | anything / unknown | **FLASH** (seeds the inventory) | — |
| app-only, same `bundle_id` | same id | **FLASH** (idempotent, otadata preserved) | — |
| app-only, newer rev | family+layout match | **FLASH** (no erase) | — |
| app-only | `family.esp_idf_version` differs | **REFUSE** | `family-mismatch` |
| app-only | `family.sdkconfig_id` differs (both known) | **REFUSE** | `family-mismatch` |
| app-only | `layout_id` (or recorded rows) differ | **REFUSE** | `layout-mismatch` |
| app-only | device not in inventory | **REFUSE** (unless `--allow-unknown-inventory`) | `unknown-device` |
| erase at X | X ≠ the bundle layout's `otadata` | **REFUSE** | `otadata-offset-not-in-layout` |
| app-only w/ erase | erase overlaps a device app slot | **REFUSE** | `erase-overlaps-device-app` |
| PT-only / bootloader-only / bl+PT | anything | **REFUSE** | `partial-structural-targets` |
| any | §4.4 check fails (digest, offset, header, family, set) | **REFUSE** | see `check` output |

### The 0x30000 class, refused by construction

`build.sh` erases "otadata" at a hardcoded `0x30000` — but under the
micronuts factory table (the seeded m5stick state) **0x30000 is the
running factory app**, and the OTA app slot is elsewhere. The 2026-10-20
silent-boot-loop incident was three provenances (bootloader from an old
flash, PT+app from build A, erases tuned for table B) meeting on one
chip. The tooling kills every path to that erase:

1. erase offsets are derived from the bundle's **parsed** CSV layout —
   there is no constant to get wrong (`test_blessed_plans_only_erase_parsed_otadata`);
2. an explicitly requested erase must BE that parsed offset
   (`otadata-offset-not-in-layout`, proven with the micronuts-style CSV
   fixture reconstructed from the seed forensics);
3. an app-only erase may not overlap an inventory-recorded device app
   slot (`erase-overlaps-device-app` — full bundles are exempt: they
   rewrite the table and are the sanctioned reprovision);
4. app-only flashes onto a device whose family (bootloader generation)
   or layout differs are refused before any plan exists.

## Device inventory

`--inventory` takes an `amperstrand.device-inventory/1` file (design
§4.6; the bench-local cache of the conwrt-lab registry — seed rows in
`docs/device-inventory.json`). Entries carry the reconciled state
(`bundle_id`, `family.esp_idf_version`/`sdkconfig_id`, `layout_id`) plus
an additive `layout.app_partitions` map (`name → {offset, size}`) this
tool uses for the overlap guard — recorded from forensics when the
device's table is known but no bundle flashed it.

## Tests

`tests/tools/` — real fixtures only: the repo's own
`firmware/esp32-ccid/partitions-ota.csv`, the micronuts-factory CSV +
seed inventory (from `docs/device-inventory.json`), and a committed
**golden bundle** that must regenerate byte-identically
(`bundle_id 59403d0e…`). Run: `python3 -m pytest tests/tools/ -q`.
CI: the `python-collect` leg (stdlib+pytest env) runs it on `tools/**`
and `tests/**` changes.

## Later slices (not tonight)

- `esp32_flash.py --bundle` consumes the blessed plan (`images`
  descending + `erases` + `boot_signature`/`baud_cap`) under the bench
  flock (design M2) — the CLI output shape is already that plan.
- `build.sh --bundle-out` calls `flash_bundle.py build` after `elf2image`.
- registry commit + cache/tags/ledger writes on success (§4.6).
