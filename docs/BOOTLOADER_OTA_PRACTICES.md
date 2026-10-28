# Bootloader & OTA Practices Across Amperstrand ESP32 Projects

**Owner ask (2026-10-28):** audit the bootloader story including how the various
Amperstrand projects deal with it and whether we support OTA updates; research
industry best practices; make the story cross-compatible across all our
projects; document it.

**Scope.** Local checkouts audited read-only 2026-10-28:
`ccid-firmware-rs` (this repo), `bolty-rs`, `nucula` (Amperstrand fork of the
C wallet firmware), `nutbar`, `micronuts`, `t-a7670g-tollgate`. File/line
citations refer to those trees as of 2026-10-28; they are leads, not eternal
truths (re-verify before acting on any single one). Web sources cited inline.

**TL;DR.** Only ccid-firmware-rs has a real OTA story (dual-bank slots +
otadata, rollback with a serve-evidence mark-valid self-test, token auth,
coredump partition, chip-derived bootloader offsets, bundle tooling). bolty-rs
has half of one (single OTA slot + Ed25519-signed HTTP updates, no rollback).
The wallet/tollgate projects are factory-app-only by design and update over
serial. The canonical pattern below is "what ccid already does, written down";
the gap list says what each sibling should adopt. The "multiboot between
projects" idea is mechanically proven (bolty's role_switch does it) but is NOT
worth productizing — the cross-project win is a **shared partition layout**,
not a boot manager.

---

## 1. Audit

### 1.0 Cross-project scorecard

| Project | Partition layout | Bootloader flash | OTA | Secure boot / signing | Bench update path |
|---|---|---|---|---|---|
| **ccid-firmware-rs** | **dual-bank** ota_0+ota_1+otadata, coredump, no factory | offsets **derived per chip** (0x1000 / 0x0), bundle requires bl+PT+app | **custom TCP :3232**, OTA2 token auth, rollback + mark-valid self-test | none (deliberate, bench) | serial via `esp32_flash.py`; OTA seats via `ota_push.py` |
| **bolty-rs** | factory + **single** ota_0 + otadata, no coredump | espflash writes bl+PT+app (offsets implicit per chip) | HTTP pull + **Ed25519 sig verify before commit**; TCP :3232 push on nucula (no auth); **no rollback** | none (Ed25519 software chain instead) | serial espflash; **otadata role-switch** on shared m5stick |
| **nucula** (C wallet) | single factory, no otadata, no coredump | idf.py full set (`@flash_args`) | **absent** | absent | serial; full-dump backup + byte-exact restore |
| **nutbar** | single-app stock table | manual triple flash (0x1000/0x8000/0x10000) | **absent** (project superseded) | absent (CHANGELOG claim unbacked) | none (manual reflash) |
| **micronuts** | single factory ×2 sub-projects, no otadata/coredump | espflash / app-only writes; documented on-flash drift | **absent** | absent | serial (esp32 espflash; STM32 st-flash w/ backup+restore) |
| **t-a7670g-tollgate** | single factory (+spiffs), coredump explicitly NONE | full triple flash every update, correct per-chip offsets | **absent — deliberately dropped** (documented future plan) | absent | serial; full-flash image backup/restore |

### 1.1 ccid-firmware-rs — the canonical pattern

**Chips:** ESP32 classic (m5stick/m5atom, xtensa) + ESP32-C3 (nucula).

**(1) Partition layout** — `firmware/esp32-ccid/partitions-ota.csv:2-9`:

```
nvs,      data, nvs,     0x9000,  0x26000,
phy_init, data, phy,     0x2F000, 0x1000,
otadata,  data, ota,     0x30000, 0x2000,
ota_0,    app,  ota_0,   0x40000, 0x1A0000,
ota_1,    app,  ota_1,   0x1E0000,0x1A0000,
coredump, data, coredump,0x3F0000,0x10000,   # issue #64, last 64 KB of 4 MB
```

Dual-bank OTA: **yes** (ota_0 + ota_1 + otadata). Coredump: **yes**. Factory:
**no** ("There is no factory partition on this bench (ota_0/ota_1 only)" —
`tests/hardware/ota/README.md:69-70`). Selected by every shipped sdkconfig via
`CONFIG_PARTITION_TABLE_CUSTOM_FILENAME="partitions-ota.csv"` (e.g.
`sdkconfig.full:410-413`, `sdkconfig-xtensa-vpcd.full:363-366`); injected into
the esp-idf-sys build by `build.sh:489-490` (GLOB mechanism).

**(2) Bootloader flash handling** — offsets are **derived, never hardcoded by
callers**. `tools/hil/make_bundle.py:51-56`:

```python
BOOTLOADER_OFFSETS = {
    "esp32": "0x1000",   # classic xtensa
    "esp32c3": "0x0",    # C3/S3/C2 generations boot at 0x0
    "esp32s3": "0x0",
    "esp32c2": "0x0",
}
```

with the 2026-10-08 m5stick incident note (bootloader at the C3 offset on a
classic ESP32 → `flash read err, 1000` boot loop) at `make_bundle.py:47-50`,
and "unknown chip → refuse" (`bootloader_offset_for`, :67-76). Bundles
**require** bootloader + PT + app together ("a bundle is a complete
reprovision set" — check 6, `make_bundle.py:157-165`, plus a mandatory
`--esp-idf-version` identity, check 5, :205-212). Day-to-day serial flashes
(`build.sh --flash` / `flash_and_test.sh`) write **PT + app + otadata-erase,
no bootloader** (`build.sh:573,582-584`: `--image 0x8000=PT
--image 0x40000=app --erase 0x30000=0x2000`) — safe only because the OTA PT
rides in the same invocation (the micronuts factory table puts the running app
at 0x30000; that refusal class is fixture-tested in
`tests/tools/test_flash_bundle.py` and encoded in
`tools/hil/flash_bundle.py:14-19`). The canonical serial flasher
`tests/hardware/labgrid/esp32_flash.py` writes in ONE esptool invocation in
descending offset order (app before PT — a mid-write failure leaves the OLD
table describing flash, :577-583), erases only after all writes land
(:627-639), then proves the boot (`boot_check_no_conflict`, :786-797). The
DIO header guard (issue #309: esptool ≥5 defaults QIO headers that silently
boot-loop DIO boards) lives in `build.sh:546-553` and `make_bundle.py:79-95`.

**(3) OTA support** — device side, `firmware/esp32-ccid/src/ota.rs` +
`ota_header.rs`:

- **Mechanism:** custom TCP server on :3232 (`ota.rs:23`), raw
  `esp_ota_begin/write/end/set_boot_partition/restart`. Not `esp_https_ota`
  (the only OTA Kconfig present anywhere is
  `# CONFIG_OTA_ALLOW_HTTP is not set`). Protocol: `OTA1` (magic+u32 size,
  legacy) and `OTA2` (magic+size+32-byte token) — `ota_header.rs:21-25`.
- **Auth:** compile-time `OTA_TOKEN` (exactly 32 chars, wrong length panics
  loudly — silently disarmed would disable auth, `ota_header.rs:52-62`).
  Armed builds **refuse** OTA1 (`authorize()`, :70-94); token checked before
  any image byte is read. Benign-by-feature-gating: only `bench-net` builds
  spawn the listener at all (`wifi_common.rs:47-50`).
- **Commit gate:** size validated against the real partition first
  (`ota.rs:104-110`), `esp_ota_abort` on stream failure, and
  **`esp_ota_end` runs full image verification (magic, segments, checksum,
  appended SHA-256) BEFORE the boot switch** (`ota.rs:140-146`; semantics
  documented host-side in `tests/hardware/ota/ota_push.py:19-22`).
- **Rollback / mark-valid:** `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` in all
  seven shipped `.full` configs (e.g. `sdkconfig.full:1769`,
  `sdkconfig-xtensa-vpcd.full:288`), policy comment at `build.sh:96-107`. The
  self-test is **serve evidence, not a boot timer**: the image gets one boot
  to prove it can complete a full CCID request→response exchange through the
  serving kernel, and only then does it call
  `esp_ota_mark_app_valid_cancel_rollback()` (`firmware/esp32-ccid/src/ota_validity.rs:41-43`;
  the fire-once `ValidityGate` kernel lives in
  `crates/ccid-service/src/ota_validity.rs`). Evidence fires at transport
  completion on every serving transport (UART `main.rs:276-278` after
  `wait_tx_done`, CDC `pn7160_ccid.rs:210`, TCP `tcp_server.rs:175`, SPP
  `spp_server.rs:459`, vpcd `vpcd.rs:190`). Diagnostic mains never mark valid.
  **Caveat:** the knob is baked into the COMPILED bootloader — the m5stick
  still runs a pre-knob bootloader, so config is present but unenforced there
  (`tests/hardware/ota/README.md:140-151`; locked by
  `tests/hardware/test_wiring_coherence.py:145-161`).
- **Anti-rollback:** absent and documented ("Secure boot / flash encryption /
  anti-rollback eFuse versioning (`CONFIG_BOOTLOADER_APP_ANTI_ROLLBACK`) -
  not enabled on this bench" — `tests/hardware/ota/README.md:299-300`).
- **Host side:** `tests/hardware/vpcd/ota_push.py` (OTA1/OTA2-aware,
  `push()` :48-91, streams the app image in 4 KiB chunks) and
  `tests/hardware/ota/ota_push.py` (OTA1-only suite library, :36-113). Both
  send **app-only** images. Note: three docs reference `host-tools/ota_push.py`
  which does not exist — the real clients are the two above.

**(4) Secure boot / signed images:** **absent, deliberately** —
`# CONFIG_SECURE_BOOT is not set` in every `.full` (e.g.
`sdkconfig-xtensa.full:301`, `sdkconfig.full:331`); listed under "Not covered"
(`tests/hardware/ota/README.md:299-300`). `docs/SECURITY_AND_BOOTLOADER.md`
is an STM32 research/roadmap doc, not implemented ESP32 practice.

**(5) Bench update path:** serial is canonical via
`tests/hardware/labgrid/esp32_flash.py` (pcscd quiesce under bench flock,
baud caps, descending writes, erases-last, USB latch ladder, FTDI
latency_timer restore, boot proof — "Canonical ESP32 flash path — the
serial-wedge discipline", :1). Wireless: OTA seats `ccid-nucula` /
`ccid-m5stick` at `192.168.13.230/.234:3232` under labgrid acquire
(`tools/hil/labgrid-exporter.yaml:47-69`, workflow in `tools/hil/README.md`).
Emerging third path: flash bundles ("bundle-or-nothing" reprovision sets)
with `ota_push.py --bundle` planned (`docs/FLASH-VIA-LABGRID-DESIGN.md:231`).

### 1.2 bolty-rs

**Chips:** ESP32 classic (m5stick/m5atom) + ESP32-C3 (nucula); compile-only
STM32F469 skeleton. ESP-IDF v5.5.5 via esp-idf-sys.

**(1) Partition layout** — `apps/bolty-esp32/partitions.csv:2-6`:

```
nvs,      data, nvs,     0x9000,  0x6000,
phy_init, data, phy,     0xf000,  0x1000,
otadata,  data, ota,     0x10000, 0x2000,
factory,  app,  factory, 0x20000, 0x1E0000,
ota_0,    app,  ota_0,   0x200000,0x1E0000,
```

**Hybrid factory + single OTA slot** (deliberate: the bench role-switch
scheme, §5). No ota_1 → no A/B within bolty itself. **No coredump partition.**
Table selection is NOT via Kconfig — `sdkconfig.defaults:20-26` documents the
esp-idf-sys cmake-relative-path limitation (esp-rs/esp-idf-sys#395) and the
workaround: the PT is passed to espflash at flash time instead
(`build.sh:184-196`: "The partition table must land WITH the image or the OTA
layout never reaches the device... espflash without it writes the SDK default
layout", bolty #146). Doc rot: `docs/card-safety.md:402-403` claims
"ota_0, ota_1 + factory" — wrong vs the actual CSV.

**(2) Bootloader flash handling:** `build.sh --flash` →
`espflash flash --baud 115200 --port … --partition-table "$PT" "$BIN"`
(`build.sh:194-195`); espflash writes bootloader+PT+app as its standard
bundle, deriving per-chip offsets itself (generated sdkconfigs record
`CONFIG_BOOTLOADER_OFFSET_IN_FLASH=0x1000` xtensa / `=0x0` C3). No repo-owned
offset table; whole-chip merged images (role-switch fallback) flash at 0x0.
`tools/hil/overnight/split_images.py:52-66` can *synthesize* a CRC-valid
otadata entry — notable for §5.

**(3) OTA support** — two mechanisms:

- **HTTP pull + Ed25519 signature** (feature `ota`, the m5stick canonical
  build): plain `EspHttpConnection` GET streamed in 4 KB chunks with inline
  SHA-256, then `verify_ed25519_signature(...)` **before** commit
  (`apps/bolty-esp32/src/ota.rs:57,126-129` — "ota signature VERIFIED —
  committing"; wrong key → update Dropped, device stays on running slot;
  hardware-proven per AGENTS.md:438-472). Pubkey in NVS, provisioned via
  serial; host signer `tools/ota-sign.py`. Trigger: serial console only
  (`commands.rs:49-52`) — the REST OTA route implied by
  `docs/card-safety.md:434` does not exist.
- **TCP push server :3232** (feature `ota-server`, nucula/C3): protocol-
  compatible with ccid's OTA1 client (`ota_server.rs:8,107-108`), "the bench
  recovery door" (:1-4), `esp_ota_end` as commit gate (:139-141). **No token,
  no signature** — integrity relies on image verification inside esp_ota_end.
  Host client is ccid's `ota_push.py` (noted at `ota_server.rs:13`).

**Rollback/mark-valid: ABSENT.** Generated sdkconfigs say
`# CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE is not set`;
`esp_ota_mark_app_valid_cancel_rollback` appears only in
`docs/card-safety.md:410-417` as a described-but-unimplemented scheme; manual
rollback = otadata erase / factory fallback (`role_switch.py:406-414`).
Anti-rollback: none.

**(4) Secure boot / signed images:** secure boot absent (generated sdkconfigs
`# CONFIG_SECURE_BOOT is not set`, no signing in build tooling). Image
authentication is the software Ed25519 scheme — the same *idea* as IDF's
"Signed App Verification Without Hardware Secure Boot" (chain of trust from
the running app; see §4.4) but custom-crypto rather than RSA-PSS
signature-block format.

**(5) Bench update path:** serial espflash via build.sh (115200 — the FT232
link drops bytes above it, `build.sh:45`); nucula push over :3232; and the
**role switch** on the shared m5stick: "the stick is provisioned once with
the bolty dual-slot table (bolty app in factory @0x20000, ccid app in
ota_0 @0x200000); a role switch is then an otadata flip + reboot (~15 s)
instead of a 4 MB reflash (~100 s)" (`role_switch.py:173-178,176`).
Recovery ladders documented (AGENTS.md:623-637 flash-link reliability;
full-chip dump/restore at 0x0, AGENTS.md:534-536; bad-OTA recovery
`docs/card-safety.md:439-444`).

### 1.3 nucula (Amperstrand fork, C wallet firmware)

**Chips:** ESP32-C3 (XIAO default / NUCULAPCB production), ESP32 classic
(Atom / M5Stick variants).

**(1) Partition layout** — `partitions.csv:2-4`: nvs@0x9000/0x26000,
phy@0x2F000, **factory@0x30000/0x1D0000**. Single factory bank; **no
otadata, no OTA slots, no coredump partition**; flash above 0x200000
unallocated. Selected by `sdkconfig.defaults:7-8`; per-board defaults files
carry only the board flag.

**(2) Bootloader flash handling:** no dedicated rig flasher; primary flow is
`idf.py flash` / `esptool write_flash "@flash_args"` (bootloader+PT+app
together — README.md:17-20, AGENTS.md:67-71, with the Hades2001 115200-baud
and explicit-port traps documented). Per-chip-family offsets are implicit in
IDF tooling, not spelled out in-repo. Rev-A verification used **app-only**
writes at 0x30000 ("Application partition only; NVS, partition table and
bootloader preserved" — `docs/rev-a-firmware.json:14,18`), because NVS holds
the wallet seed ("Do not erase flash on a device holding funds" — README:24).

**(3) OTA support: ABSENT in firmware.** Repo-wide search for `esp_ota`,
`esp_https_ota`, `mark_app_valid`, `secure_version` → zero source hits; the
only `app_update`-adjacent dependency is `espressif/cbor`. Every "OTA seat"
mention refers to the **ccid-firmware-rs** image + its push tool on the
shared bench boards (`rig/src/endpoints.rs:106`,
`rig/tests/endpoints.rs:38`), and "OTA relay" in the roadmap means NFC
over-the-air relaying, not firmware update.

**(4) Secure boot / signed images:** absent. Every "signature" hit is Cashu
application crypto (blind signatures, DLEQ).

**(5) Bench update path:** disciplined manual serial with full-chip backup:
`esptool read-flash 0x0 0x400000 …` → stop `nucula-ccid-broker` → flash
wallet fw → run suites → byte-exact restore
(`tests/hil/phone/README.md:36-43`, `docs/HIL-PHONE-DESIGN.md:126-130`).
Recovery: `rig/scripts/recover_console.py` (reset-pulse + banner + healthy
`status`), download-mode escape (dtr=False, pulse rts), fleet-level
`dut_recover.py --from-lab`. Known-good Rev-A pinned by sha256 in
`docs/rev-a-firmware.json:11`.

### 1.4 nutbar

**What it is:** desktop Cashu wallet (Rust daemon + Omarchy plugin); the
`esp32/atom-detector/` subtree is a genuine but **superseded** ESP-IDF C
firmware ("The embedded TollGate client has moved to the Micronuts project.
This firmware is kept as a TollGate v1 protocol reference" —
`esp32/atom-detector/README.md:3-7`).

**(1) Partition layout:** no partition CSV anywhere; the stock single-app
table is selected (`CONFIG_PARTITION_TABLE_SINGLE_APP=y`,
`esp32/atom-detector/sdkconfig.defaults:6`) → nvs+phy+factory, no OTA slots,
no coredump.

**(2) Bootloader flash handling:** manual prose only —
`esp32/atom-detector/README.md:96-102` flashes bootloader+PT+app together at
the classic-ESP32 offsets (0x1000/0x8000/0x10000). One chip family, no offset
matrix. No scripts, no CI firmware job.

**(3) OTA support: ABSENT** — zero `esp_ota`/`esp_https_ota`/OTA matches in
the tree (the daemon's HTTP route table `daemon/src/main.rs:2348-2372` has no
firmware-update route either; "rollbackable" hits are Cashu journal states).

**(4) Secure boot / signed images: ABSENT.** `CHANGELOG.md:18` claims
"Signed firmware verification (SSHSIG manifests with pinned keys)" — grep
finds no implementing code, keys, or manifests anywhere; unbacked claim worth
deleting or substantiating.

**(5) Bench update path:** none (manual full reflash; desktop install.sh is
host-side only).

### 1.5 micronuts

**Chips:** ESP32 classic + S3 lane (wallet), ESP32 classic (mint); the
`firmware/` workspace member is STM32F469 (out of ESP32 scope, noted below).

**(1) Partition layout** — two single-factory tables, no otadata, no
coredump, anywhere:

- wallet `micronuts-esp32-wallet/partitions.csv:2-4`: nvs@0x9000/0x20000,
  phy@0x29000, factory@0x30000/0x3D0000 (selected via sdkconfig.defaults:8-9
  + the esp-idf-sys GLOB env in `.cargo/config.toml`).
- mint `micronuts-esp32-mint/partitions.csv:7-9`: nvs grown to 0x10000 for
  32 KiB state blobs (comment :2-6), factory@0x110000/0x2F0000; the table is
  NOT set via Kconfig — passed to espflash at flash time
  (`micronuts-esp32-mint/README.md:34,59-60`, same esp-idf-sys limitation as
  bolty).

**Documented drift hazard:** `docs/HANDOFF-2026-09-19.md:34` records the
m5stick actually running a 2MB-bounded table (factory@0x20000) that did not
match `partitions.csv` (factory@0x30000) — exactly the class of silent
mismatch that "PT rides with the app" prevents.

**(2) Bootloader flash handling:** no repo script writes
bootloader+PT+app together for the ESP32 targets. espflash is the runner;
app-only writes are sanctioned for CYD ("App-only flash at `0x20000` is
always safe" — AGENTS.md:176-177; PORT-M5STICK.md:47 uses 0x10000 —
inconsistent doc offsets mirroring the drift above). No bundle tooling.
STM32 side (for contrast) has real discipline: `tools/hil/rig.py:247-274`
always `st-flash --connect-under-reset` + mandatory 2 MiB backup
(`backup_stm32` :257-260) and restore.

**(3) OTA support: ABSENT in every sub-project.** Repo-wide grep for
`esp_ota|esp_https_ota|mark_app_valid|OTA_TOKEN|secure_version` → zero
micronuts-owned hits; all ":3232" references point at ccid's wifi-vpcd image
on shared bench boards (`micronuts-satocash/README.md:90-91`). "Rollback"
hits are Cashu payment semantics; `.wave-plan.md:193,199` "rollback image" is
the STM32 backup. No `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`, no
`CONFIG_ESP_COREDUMP_ENABLE_TO_FLASH` in any micronuts sdkconfig.

**(4) Secure boot / signed images:** absent (no hits in any micronuts
source/sdkconfig/script).

**(5) Bench update path:** serial only — espflash or esptool app-only writes
per board (table in §1.0); STM32 via rig.py backup/flash/restore under
labgrid place + BenchLock. CI builds and scans but never flashes. Recovery
lore: CYD NVS dump-before-erase rule (erasing NVS permanently kills
`esp_wifi_start` — RF cal data, AGENTS.md:165-177), known-good rollback
image (`.wave-plan.md:193-199`).

### 1.6 t-a7670g-tollgate

**Chips:** ESP32 classic (T-A7670G PoC, CYD, cyd-probe), ESP32-S3
(T-Display-S3 display test, reseller lane). All ESP-IDF C; no CI.

**(1) Partition layout** — single factory everywhere, **no otadata, no
coredump** (coredump explicitly `CONFIG_ESP_COREDUMP_ENABLE_TO_NONE=y` in
every generated sdkconfig, e.g. `tollgate-poc/sdkconfig:1153`):

- `tollgate-poc-sources/partitions.csv:2-5`: nvs@0x9000/0x40000,
  phy@0x49000, factory@0x50000/0x2A0000, spiffs@0x2F0000/0x110000.
- `cyd-tollgate` / `cyd-probe`: no custom CSV — stock
  `CONFIG_PARTITION_TABLE_SINGLE_APP=y` (`cyd-tollgate/sdkconfig:398-404`).
- `display-test-s3-sources/partitions.csv:2-4`: factory@0x10000/0x300000
  (16 MB flash).
- **Bug-shaped quirk:** `reseller-s3-sources/partitions.csv` exists but its
  `sdkconfig.defaults:1-21` never sets `CONFIG_PARTITION_TABLE_CUSTOM` — the
  CSV is inert; builds would use the stock table.

**(2) Bootloader flash handling:** full triple flash on every update via
`write_flash @flash_args` (e.g. `lab/flash-poc.sh:12-13`,
`scripts/flash-s3.sh:114-116`); `tollgate-poc/build/flasher_args.json:10-17`
shows the correct classic offsets (bootloader 0x1000, PT 0x8000, app
0x50000). Careful per-board reset semantics: the S3 has no auto-reset circuit
→ manual BOOT entry + `--after watchdog-reset` to re-sample strapping pins
(`scripts/flash-s3.sh:2-6,109-111`); the probe uses `--after no_reset` to
preserve single-boot test fixtures (`lab/flash-probe.sh:3-8`).

**(3) OTA support: ABSENT — deliberately dropped.** "The OTA mechanism is
dropped. Single factory partition only." / "No OTA partition. Updates
require UART flashing. Acceptable for a tollgate appliance."
(`docs/TOLLGATE-FEASIBILITY.md:52,127-128`). A future plan exists on paper
(dual OTA, ECDSA-v2 signed images, `esp_ota_mark_app_valid_cancel_rollback`
within one boot, cellular download — `docs/BUILD-TOOLCHAIN-DECISION.md:131-137`)
and is unimplemented.

**(4) Secure boot / signed images:** absent (`# CONFIG_SECURE_BOOT is not
set` in every generated sdkconfig, e.g. `cyd-probe/sdkconfig:331-332`).

**(5) Bench update path:** idf.py build → esptool `@flash_args` per board;
recovery/rollback philosophy is **whole-flash image**: `firmware-backup/`
holds 4 MB dumps with read/write recipes (`firmware-backup/README.md:5,10`,
`AGENTS.md:52-59`). Ports always by-id; "Never flash after a failed build"
(AGENTS.md:290-292).

---

## 2. The canonical Amperstrand bootloader/OTA pattern

Everything in this section already exists in ccid-firmware-rs; this is the
written-down version other projects should converge on. Numbers cite this
repo.

1. **One partition table per board class, dual-bank, no factory:**
   `nvs + phy_init + otadata(0x2000) + ota_0 + ota_1 + coredump`
   (`partitions-ota.csv`). Two equal app slots give power-fail-safe updates:
   the running image is never the write target, and otadata's two-sector
   `ota_seq` design tolerates mid-write power loss (ESP-IDF OTA guide, "OTA
   Data Partition"). No factory slot keeps the fallback story simple ("the
   other slot" + reprovision bundle) and is *required* if anti-rollback is
   ever enabled (ESP-IDF: "Factory and Test partitions are not supported in
   anti rollback scheme"). App slots 0x10000-aligned, last 64 KB reserved for
   coredump.
2. **Bootloader offsets are DERIVED from the chip family, never caller-set:**
   esp32→0x1000, esp32c3/s3/c2→0x0 (`make_bundle.py:51-56`); unknown chip →
   refuse (:67-76). Partition table at 0x8000. Reprovision/deep-flash events
   ship **bootloader + PT + app together** with the IDF version recorded
   (bundle checks 5+6) — mixed-generation bootloader/app pairs are a proven
   incident class here. Day-to-day serial updates may flash PT+app+otadata-
   erase without the bootloader, but ONLY from a tool that knows the offset
   is table-relative (the 0x30000-is-the-factory-app trap,
   `flash_bundle.py:14-19`).
3. **OTA writes go through `esp_ota_*` with `esp_ota_end` as the integrity
   gate**, size-gated against the actual partition before any erase, abort on
   stream failure (`ota.rs:104-146`). Whatever transports projects invent
   (TCP :3232, HTTP pull, SPP…), the commit path must be exactly this.
4. **Authenticate before bytes:** ccid's compile-time 32-byte token (OTA2)
   is the bench-tier floor; armed builds refuse tokenless clients
   (`ota_header.rs:70-94`). For anything that leaves the bench, upgrade to
   signature verification before commit (bolty's Ed25519-before-commit is
   the in-fleet precedent; ESP-IDF's sanctioned form is
   `CONFIG_SECURE_SIGNED_APPS_NO_SECURE_BOOT`, §4.4).
5. **Rollback + a mark-valid that means something:** enable
   `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` in the sdkconfig AND reflash the
   bootloader so it is enforced (the m5stick caveat,
   `tests/hardware/ota/README.md:140-151`). The app must call
   `esp_ota_mark_app_valid_cancel_rollback()` only after a **real self-test**
   — ccid's serve-evidence gate (full CCID exchange through the serving
   kernel, `crates/ccid-service/src/ota_validity.rs`) is the pattern:
   "reached app_main" proves nothing about the product's actual function.
   Diagnostic mains never mark valid.
6. **Core dump to flash + silent panic:** coredump partition +
   `CONFIG_ESP_COREDUMP_ENABLE_TO_FLASH` + SILENT_REBOOT (all seven `.full`s)
   — the dump-and-retrieve doctrine. No coredump partition = crash forensics
   are guesswork (four of six projects today).
7. **Flash discipline (host side):** one esptool invocation, writes in
   DESCENDING offset order, destructive erases only after every write landed,
   mandatory boot proof (FWID or behavior), DIO header guard, by-id ports,
   baud capped to the bridge, pcscd/broker quiesce, full-flash backup before
   touching foreign or money-bearing boards. `esp32_flash.py` is the
   reference implementation.
8. **Anti-rollback (eFuse secure_version): OFF on the bench.** It is one-way
   hardware state, incompatible with factory partitions, caps at 32 version
   steps, and would poison multiboot role-switching (§5). Revisit only with a
   signed production story and a key-rotation plan.

---

## 3. Gap list per project

Effort: S ≤ a day, M ≤ a week, L > a week. Items assume the shared 4 MB
board class; money-bearing NVS layouts must be preserved by construction.

**ccid-firmware-rs** (canonical, gaps are polish):
- S: fix the stale `host-tools/ota_push.py` doc path (3 docs point at a file
  that doesn't exist; the real clients live under `tests/hardware/`).
- S: reflash the m5stick bootloader so the rollback knob is actually
  enforced (README:140-151) — one bundle flash.
- M (field-only): signature verification on the OTA path
  (`CONFIG_SECURE_SIGNED_APPS_NO_SECURE_BOOT` or an Ed25519-before-commit
  scheme à la bolty) + HTTPS transport if any deployment is exposed past the
  bench LAN.

**bolty-rs:**
- S: fix doc divergence — `card-safety.md:402-403` (claims ota_0+ota_1) and
  `:434` (claims REST OTA route).
- S: add a token (or refuse-by-default) to the :3232 push server
  (`ota_server.rs`) — today anyone on the LAN can flash it.
- M: enable `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` (+ bootloader reflash)
  and call `esp_ota_mark_app_valid_cancel_rollback()` after a real self-test
  (e.g. first successful card transaction / API round-trip) — with a single
  ota slot, a bad committed OTA today needs serial recovery.
- S/M: coredump partition + `CONFIG_ESP_COREDUMP_ENABLE_TO_FLASH` (currently
  only an NVS crashlog; heap-exhaustion-class postmortems needed a debug
  build once).
- M (optional): second OTA slot or document factory-as-fallback as THE
  rollback story, so the two doc/behavior pairs stop drifting.

**nucula (wallet firmware):**
- S: coredump partition + config — a money-handling device with no crash
  forensics is the highest-value single line item in this audit. Layout must
  keep nvs@0x9000/0x26000 byte-identical (the seed lives there; Rev-A
  hash-pinned images must keep booting).
- M: dual-bank table (factory → ota_0 + ota_1) so update experiments stop
  requiring full-dump/restore cycles; pair with rollback + mark-valid after
  "wallet prompt alive + status healthy". In-firmware network OTA is
  *optional* here — a wallet with no network stack has a smaller attack
  surface; the dual-bank win is available over serial alone.
- S: adopt chip-derived bootloader offsets + "PT rides with app" in whatever
  flash helper replaces raw `@flash_args`, killing the 0x30000-trap class
  for shared boards.

**micronuts:**
- S: reconcile the documented on-flash drift (factory@0x20000 vs CSV
  0x30000, `HANDOFF-2026-09-19.md:34`; `PORT-M5STICK.md` 0x10000) — one
  reflash with the CSV table ends the ambiguity.
- S: coredump partition for wallet + mint.
- M: dual-bank tables for the ESP32 wallet/mint targets (+ rollback +
  mark-valid after first healthy serving/print cycle), with NVS offsets
  preserved (mint's 32 KiB state blobs constrain layout).
- S: standardize "partition table rides with the app" in the flash helper
  (mint already passes `--partition-table` to espflash; wallet should too).

**t-a7670g-tollgate:**
- S: fix or delete the inert `reseller-s3-sources/partitions.csv`
  (CONFIG_PARTITION_TABLE_CUSTOM never set → silently unused).
- Keep the full-flash backup discipline; adopt "app+PT together, offsets
  derived per chip" if/when flash scripts multiply.
- M (only when a tollgate is actually fielded): implement the documented
  plan (`BUILD-TOOLCHAIN-DECISION.md:131-137`) — dual OTA + signed images +
  mark-valid rollback + cellular download. Until then single-factory UART
  flashing is a defensible appliance stance, as the feasibility doc argues.

**nutbar:**
- No adoption warranted (superseded reference firmware). S: delete or
  substantiate the SSHSIG claim in `CHANGELOG.md:18`.

---

## 4. Best practices (with citations)

### 4.1 ESP-IDF OTA mechanics — the baseline

- **Dual-bank is the safe mode.** "OTA requires configuring the partition
  tables of the device with at least two OTA app slot partitions (ota_0 and
  ota_1) and an OTA Data Partition. The OTA operation functions write a new
  app firmware image to whichever OTA app slot that is currently not
  selected for booting." Bootloader/partition-table/data-partition updates
  are classed "unsafe update mode" — power-loss-vulnerable
  (ESP-IDF Programming Guide v5.5, *Over The Air Updates (OTA)*,
  "OTA Process Overview",
  https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/api-reference/system/ota.html).
  Note v5.5.5 (our pin) exposes the rollback switch as
  `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`; IDF "latest/master" has reworked
  the confirmation-mode Kconfig names — don't mix doc generations when
  configuring.
- **otadata semantics.** Two sectors, independently written, with a counter
  to pick the more recent on disagreement; erased (0xFF) otadata → factory
  app if present, else the first OTA slot (same doc, "OTA Data Partition";
  *Partition Tables*,
  https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/api-guides/partition-tables.html).
  This is why "erase otadata → boots ota_0/factory" works as a recovery
  primitive (ccid AGENTS restore recipe, bolty role_switch).
- **Rollback state machine.** `esp_ota_set_boot_partition()` writes
  `ESP_OTA_IMG_NEW`; bootloader demotes to `PENDING_VERIFY` on first boot;
  app must confirm via `esp_ota_mark_app_valid_cancel_rollback()`, else any
  reset before confirmation → `ABORTED`, previous slot boots. "A new
  application therefore has only one attempt to reach its confirmation point
  successfully." "Disabling rollback is strongly discouraged for production
  devices." "Only OTA partitions can be rolled back. Factory partition is
  not rolled back." Recommendation: "Perform the self-test procedure as
  quickly as possible, to prevent rollback due to power loss" (same OTA
  doc, "App Rollback"/"Rollback Process"). ccid's serve-evidence gate is a
  strict refinement: fast AND function-proving.
- **Anti-rollback.** `CONFIG_BOOTLOADER_APP_ANTI_ROLLBACK` + eFuse
  `secure_version` (popcount of programmed bits, ESP32 EFUSE_BLK3_RDATA4);
  bootloader refuses images with a lower version; hard limits: 32 lifetime
  version steps, no factory/test partitions allowed, requires application-
  controlled confirmation (same doc, "Anti-rollback"). One-way hardware
  state — a bench-liberal default, as §2.8 says.
- **`otatool.py`** ships in IDF (`app_update` component): read/erase
  otadata, switch OTA slot, erase/write/read slots over serial (same doc,
  "OTA Tool otatool.py"). Useful as the *supported* role-switch primitive
  instead of hand-rolled otadata writes.
- **esp_https_ota** is the sanctioned network layer: HTTPS + server-cert
  verification (root cert or cert bundle), optional partial-range downloads
  and resumption for constrained RAM, pre-encrypted image support
  (*ESP HTTPS OTA*,
  https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/api-reference/system/esp_https_ota.html).
  Our custom TCP :3232 trades that for zero TLS footprint on the bench LAN —
  fine for the bench, not for field exposure.

### 4.2 Industry A/B practice on constrained devices

- **MCUboot** (the de-facto 32-bit MCU standard: Zephyr, Mynewt, NuttX):
  test-swap → confirm → revert. The new image boots as a *test*; it must
  mark itself "OK" (image trailer flag) or "MCUboot will perform a 'revert'
  swap during the next boot… Test swaps are supported to provide a rollback
  mechanism to prevent devices from becoming 'bricked' by bad firmware… This
  allows device firmware to make test swaps permanent only after performing
  a self-test routine." Rollback protection uses a monotonic **security
  counter** TLV (MCUboot docs, *Bootloader design* — swap types
  `BOOT_SWAP_TYPE_TEST/PERM/REVERT`, "Image OK" trailer field,
  `IMAGE_TLV_SEC_CNT`, https://docs.mcuboot.com/design.html). ESP-IDF's
  PENDING_VERIFY/VALID/ABORTED machine is the same idea with otadata as the
  trailer.
- **RAUC + barebox/U-Boot** (embedded Linux A/B): explicit **boot attempt
  counters** — barebox bootchooser keeps `remaining_attempts` + `priority`
  per slot and decrements on every unconfirmed boot (default 3 attempts),
  vs ESP-IDF's single attempt; U-Boot integration exposes `BOOT_ORDER` /
  `BOOT_<X>_LEFT` environment variables; barebox can arm the **watchdog
  before booting** (`boot.watchdog_timeout`) so a hung image counts as a
  failed attempt; "mark-good" is a systemd service that runs only after all
  relevant services came up (RAUC docs, *Integration* — "Interfacing with
  the Bootloader"/"Barebox"/"U-Boot"/"Init System",
  https://rauc.readthedocs.io/en/latest/integration.html).
- **The one gap in plain ESP-IDF rollback:** no built-in attempt counter —
  one boot, confirm or revert. If a product needs "3 bad boots then
  fallback", it must implement the counter itself (e.g. an NVS boot-attempt
  counter incremented pre-mark-valid, forced
  `esp_ota_mark_app_invalid_rollback_and_reboot()` at zero) or rely on the
  task watchdog rebooting a hung image into its single attempt. Not worth
  building for bench-tier devices; worth remembering it exists.

### 4.3 Flashing discipline (hard-won, in-fleet)

- Chip-family bootloader offsets (0x1000 vs 0x0) are load-bearing — the
  2026-10-08 m5stick incident (`make_bundle.py:47-50`); derive, never guess.
- Bootloader+PT+app together on reprovision; PT+app for iteration; app-only
  only when the tool knows the running table (the 0x30000 trap).
- Writes-descending / erases-last / boot-proof-every-flash
  (`esp32_flash.py:577-639,786-797`); DIO header guard (#309); by-id ports;
  full-flash backup before touching foreign or money-bearing boards
  (tollgate `firmware-backup/`, nucula HIL-PHONE procedure).
- Updating a partition table does not erase data stored under the OLD table
  (ESP-IDF *Partition Tables*, "Flashing the Partition Table" note) —
  silent-drift hazard that bit micronuts (`HANDOFF-2026-09-19.md:34`).

### 4.4 Secure boot & signing

- **Secure Boot v2** (RSA-PSS signature blocks, public-key digest in eFuse):
  ROM verifies the bootloader, bootloader verifies each app image; once on,
  "any updated bootloader or app will need to be signed with a key matching
  the digest already stored in eFuse", and enabling it disables the USB-OTG
  ROM serial path; recommended paired with flash encryption; keys should be
  generated with quality entropy and can live on a remote signer/HSM
  (ESP-IDF *Secure Boot v2*,
  https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/security/secure-boot-v2.html).
  Note ESP32 classic needs chip rev ≥ v3.0 for SBv2 — our bench classics
  largely can't use it anyway.
- **Middle tier — signed apps without hardware secure boot:**
  `CONFIG_SECURE_SIGNED_APPS_NO_SECURE_BOOT` verifies new OTA images against
  the signature on the *running* app ("the system creates a chain of trust
  from the running app to the newly updated app"; initial image must be
  signed or the device bricks itself against updates deliberately). Same
  doc, "Signed App Verification Without Hardware Secure Boot". This is the
  sanctioned version of what bolty hand-rolled with Ed25519 — worth
  converging on ONE of the two fleet-wide when the first device leaves the
  bench.
- Today's fleet stance (token-only or unsigned, bench LAN) is coherent as
  long as the devices stay bench-bound; every gap item above marks the
  upgrade point.

---

## 5. "Multiboot between projects" — verdict

**The idea:** use otadata as a boot *selector between different projects'
images* (e.g. bolty in one slot, ccid in another, nucula wallet in a third),
so shared bench boards role-switch in seconds instead of 100 s reflashes.

**It already exists, in exactly one direction.** bolty's
`role_switch.py:173-178` provisions the shared m5stick once with the bolty
dual-slot table (bolty in factory@0x20000, ccid in ota_0@0x200000) and flips
otadata (+reboot ≈ 15 s vs ≈ 100 s full reflash); erasing both otadata
sectors falls back to factory (bolty); `split_images.py:52-66` synthesizes
CRC-valid otadata entries from merged images. So the mechanism is
bench-proven — *within a deliberately shared partition table that one
project owns*.

**Real caveats if generalized:**

1. **Partition-layout coupling is the killer.** otadata-slot semantics only
   mean something under ONE table. Today's tables disagree violently: ccid
   otadata@0x30000 / ota_0@0x40000; bolty otadata@0x10000 / ota_0@0x200000;
   wallets' factory@0x30000 (nucula/micronuts) or 0x50000 (tollgate). A
   role-switch on the wrong table boots garbage or clobbers what another
   table calls the factory app — the literal 0x30000-is-the-factory-app
   trap ccid's `flash_bundle.py` exists to refuse. Generalized multiboot
   REQUIRES one fleet table per board class, adopted by every project, or it
   is a brick generator.
2. **NVS schema drift.** Projects share the nvs partition (all at 0x9000,
   various sizes). NVS namespaces mostly isolate projects from each other,
   but any project that erases/reformats NVS, or a size shrink, destroys the
   others' state — and some of that state is unbootstrappable (wallet seed,
   RF calibration data — the micronuts CYD `esp_wifi_start` death trap class).
3. **App-descriptor compatibility is a hazard, not a guard.** The bootloader
   checks magic/checksum/chip_id/revision — *not* project identity
   (`esp_image_header_t` chip_id; `esp_app_desc_t` at a fixed offset carries
   `project_name`/`version`/`idf_ver`/`secure_version` — App Image Format
   docs, https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/api-reference/system/app_image_format.html).
   Any same-chip image boots. Fine on a bench, terrifying as an implicit
   contract: a mis-flashed wallet image will happily boot as a reader.
   Cheap mitigation: identity probes read `esp_app_desc.project_name`
   (ccid's FWID line is this pattern).
4. **Rollback interplay.** Rollback states live in otadata. Under a
   rollback-enabled bootloader, a role-switched image that never calls
   mark-valid gets exactly one boot then ABORTED — the next flip appears
   "dead". Role-switch participants must either all confirm on boot or that
   bench board keeps rollback off (i.e. the m5stick status quo, for the
   opposite reason). And if anti-rollback eFuses were ever burned, older
   secure_version images of OTHER projects would become unbootable — another
   reason eFuses stay off on bench boards.
5. **IDF-version skew.** A bootloader from one generation booting another
   project's app is the mixed-generation incident class ccid's bundle
   `--esp-idf-version` check exists to prevent; multiboot multiplies the
   surface (every project × the one installed bootloader).

**Verdict: not worth productizing — keep it as a bench pattern.** A
generalized cross-project boot manager buys ~85 s per role switch on a
handful of shared bench boards, at the cost of a fleet-wide partition-table
contract, NVS co-tenancy rules, per-image confirmation policy, and a new
class of silent-brick failure modes. That is scope creep with a bench-sized
payoff. The genuinely valuable cross-project move is cheaper:

**Minimal safe version, if ever built:**
1. **Adopt ONE fleet partition table per board class** (ccid's
   `partitions-ota.csv` is the natural canonical 4 MB table: nvs@0x9000,
   otadata@0x30000, ota_0@0x40000, ota_1@0x1E0000, coredump@0x3F0000) —
   provisioned once per board via the bundle path. This alone makes every
   project's app image slot-compatible and kills the offset-trap class;
   it is 90% of the multiboot value with none of the manager.
2. **Role switch = otadata flip only**, via IDF's `otatool.py
   switch_ota_partition` (or a host tool writing the CRC-valid entry —
   `split_images.py` proves the format). No new firmware surface.
3. **Every participant image carries and confirms its identity:** FWID banner
   / `esp_app_desc.project_name` probe, and either marks valid quickly or
   the board's bootloader keeps rollback off — pick one policy and document
   it on the board, not per project.
4. **Never burn secure_version eFuses on shared bench boards**; treat NVS as
   co-tenanted (namespaces only, no erases, no shrinks).
5. Stop at two roles per board (A/B). Three-way "boot menu" thinking is
   where scope creep starts.

---

## Appendix: source index

In-repo (primary evidence): cited inline as `path:line`, per project.

Web (accessed 2026-10-28):
- ESP-IDF v5.5 Programming Guide — *Over The Air Updates (OTA)*:
  https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/api-reference/system/ota.html
- ESP-IDF v5.5 — *Partition Tables*:
  https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/api-guides/partition-tables.html
- ESP-IDF v5.5 — *App Image Format*:
  https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/api-reference/system/app_image_format.html
- ESP-IDF v5.5 — *ESP HTTPS OTA*:
  https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/api-reference/system/esp_https_ota.html
- ESP-IDF v5.5 — *Secure Boot v2*:
  https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/security/secure-boot-v2.html
- MCUboot — *Bootloader design* (swap types, image trailer, security
  counter): https://docs.mcuboot.com/design.html
- RAUC — *Integration* (barebox bootchooser attempts/priority, U-Boot
  BOOT_ORDER/BOOT_x_LEFT, watchdog, mark-good service):
  https://rauc.readthedocs.io/en/latest/integration.html
