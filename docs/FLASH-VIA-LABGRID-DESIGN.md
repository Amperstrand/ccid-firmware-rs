# Flash-via-labgrid — design for the Amperstrand MCU bench

> **Provenance** (kanban `t_f62142b1`): promoted 2026-10-21 from the bench
> research notes of 2026-10-20 into the repo — substance unchanged. The
> research was READ-ONLY-derived (installed labgrid 26.0 package sources,
> exporter configs, bench discipline files; no hardware touched). Companion
> bench artifacts, not tracked here: `ROOT-REFLASH-RUNBOOK-20261020.md`,
> `TAKEOVER-NOTE-20261020.md` (ai-legion `/root/src/worktrees/`). The M0
> inventory seed this design calls for lives at `docs/device-inventory.json`.

Status: proposal (research 2026-10-20, bench host ai-legion/.208)
Scope: ESP32 (esptool) + STM32 (st-flash/DFU/OpenOCD) flash, boot-verify,
console capture, and image-bundle management, coordinated through the
installed labgrid 26.0. Everything here is READ-ONLY-derived: installed
package sources, exporter configs, and the existing bench discipline
files — no hardware was touched.

Trigger: the 2026-10-20 mixed-family incident (old prebuilt bootloader
v5.5.1 on flash + freshly-built v5.5.5 images + two partition-table
layouts mixed across sessions → silent boot loops), plus the
ROOT-REFLASH-RUNBOOK archaeology that followed
(`/root/src/worktrees/ROOT-REFLASH-RUNBOOK-20261020.md`).

---

## 1. Current-state map

### 1.1 Installed labgrid

- `pip show labgrid` → **labgrid 26.0**, installed at
  `/usr/local/lib/python3.12/dist-packages/labgrid` for **python3.12**.
  ⚠️ System `python3` is 3.14.7 and **cannot import labgrid** — anything
  driving labgrid as a library must run `python3.12` explicitly
  (verified: `python3.12 -c 'import labgrid; print(labgrid.__version__)'`
  → `26.0`; plain `python3 -c` → ModuleNotFoundError).
- Coordinator `127.0.0.1:20408` (`LG_COORDINATOR`; labgrid 26.0 has **no
  client config file** — env var or `-x` only, per
  `tests/hardware/labgrid/README.md` #164 note). Coordinator persists
  places in `/var/lib/labgrid/places.yaml` across restarts.
- Exporters in `/etc/labgrid/`: `exporter-nfc.yaml` (ai-legion-nfc: serial
  tokens, STM32 DUTs, reference readers), `exporter-ccid.yaml`
  (ai-legion-ccid: OTA NetworkServices + console enrollment records),
  plus cyd/t7670/s3/venue/vlab/gs1900/numo exporters (10 files in
  `/etc/labgrid/`) — 39 places on the coordinator (live
  `labgrid-client places`, 2026-10-20), shared by ccid, bolty,
  micronuts, tollgate, PRTA, conwrt.
- ser2net **4.6.0** installed and active; esptool **v5.3.1**; st-flash
  (stlink **v1.8.0**); openocd **0.12.0**; dfu-util present.
  **uhubctl is NOT installed** and no switchable USB hub exists on the
  bench (all DUTs sit on root hub bus 001).

### 1.2 What labgrid 26.0 ships that matters here (exact classes + paths)

Package root below = `/usr/local/lib/python3.12/dist-packages/labgrid`.

| Capability | Class | File:line | Notes |
|---|---|---|---|
| Generic flash script | `FlashScriptDriver` | `driver/flashscriptdriver.py:13` | binds `USBFlashableDevice`/`NetworkUSBFlashableDevice`; `.flash(script, args)`; ships the script to the exporter host via `ManagedFile.sync_to_resource()` and runs it there with `{device}`/`{file}` arg templating. This is labgrid's intended extension point for "any flasher" — see §6. |
| DFU flashing | `DFUDriver` | `driver/dfudriver.py:12` | `download(alt, file)` / `detach(alt)` / `list()` via `dfu-util`; binds `DFUDevice` (`resource/udev.py:360`, udev match on `:fe0102:` DFU interface) or `NetworkDFUDevice` (`resource/remote.py:139`). Client: `labgrid-client dfu`. |
| OpenOCD/SWD | `OpenOCDDriver` | `driver/openocddriver.py:14` | implements `BootstrapProtocol`; binds `USBDebugger` (`resource/udev.py:798` — its VID:PID allowlist includes our ST-LINK `0483:374b`) or `NetworkUSBDebugger` (`resource/remote.py:319`). |
| Serial console | `SerialDriver` | `driver/serialdriver.py:16` | binds `SerialPort`/`NetworkSerialPort`; speaks `rfc2217://…?ign_set_control` (line 46) — labgrid's own console driver **never toggles DTR/RTS**. |
| Console via subprocess | `ExternalConsoleDriver` | `driver/externalconsoledriver.py:18` | runs any `cmd:` (e.g. `microcom`, `telnet`) as the console — useful for custom ser2net endpoints. |
| USB power (replug) | `USBPowerDriver` | `driver/powerdriver.py:356` | **uhubctl-based**: `_switch()` runs `uhubctl -l <hub.path> -p <hub.index> -r 100 -a on|off`; `on/off/cycle/get`; binds `USBPowerPort` (`resource/udev.py:638`) or `NetworkUSBPowerPort` (`resource/remote.py:277`). |
| USB power (dedicated) | `YKUSHPowerDriver` | `driver/powerdriver.py:273` | Yepkit YKUSH3, `ykushcmd`; binds `YKUSHPowerPort` (`resource/ykushpowerport.py`). No uhubctl dependency. |
| Other power | `ExternalPowerDriver` `powerdriver.py:116`, `NetworkPowerDriver` `:151`, `DigitalOutputPowerDriver` `:238`, `PDUDaemonDriver` `:421`, plus 30+ PDU backends in `driver/power/` | | `PowerResetMixin` (`:20`) makes every power driver a `ResetProtocol` — `labgrid-client -p P power cycle` is a first-class reset. |
| Serial resources | `RawSerialPort`/`NetworkSerialPort` | `resource/serialport.py:10/22` | `NetworkSerialPort.protocol` = `raw` or `rfc2217`. |
| USB serial by identity | `USBSerialPort` | `resource/udev.py:226` | udev-matched (by-id), `port` attr auto-filled from `device_node`. |
| Network service | `NetworkService` | `resource/networkservice.py:9` | how the OTA seats are owned today. |
| Strategies | `ShellStrategy` `strategy/shellstrategy.py:18`, `GraphStrategy` `strategy/graphstrategy.py:33` | | `GraphStrategy` builds a state machine from `state_*` methods + `@depends()` — the labgrid-native shape for a flash→verify→console cycle. |
| pytest plugin | fixtures `env`/`target`/`strategy` | `pytestplugin/fixtures.py:54/114/125` | `pytest --lg-env <file>` drives the full driver graph. |

Client side (`labgrid-client`, `remote/client.py`):

- `power` **auto-instantiates** `USBPowerDriver` (client.py:955), `YKUSHPowerDriver` (:963) etc. when the acquired place matches the corresponding resource — **no env file needed** for `labgrid-client -p <place> power on|off|cycle`.
- `console` (client.py:1029) finds the place's `NetworkSerialPort` and execs `microcom -s <speed> -t host:port` (telnet fallback). Requires the port to be ser2net-served, i.e. acquired.
- `flashscript`, `dfu`, `fastboot`, `bootstrap` subcommands exist for the drivers above.
- `env` (client.py:866) generates an environment YAML for an acquired place from its matched resources.
- Reservation queue: `reserve`/`wait`/`cancel-reservation`/`reservations` (used by the bench today via `--reserve` in the HIL conftest).

### 1.3 What labgrid 26.0 does NOT ship

- **No ESPLoader/esptool driver or resource.** `grep -ril esp` over the
  package returns nothing relevant (only `deserial…`-style false
  positives). Upstream's MCU bootstrap coverage is i.MX/RK/MXS/BDI
  (`driver/usbloader.py`), fastboot, openocd, flashrom, dediprog, LAA —
  no Espressif, no STM32 SPIMEM. ESP32/STM32 flashing on this bench
  stays **our tooling** by design (see §3.3 and §6).
- No client config file (env-var coordinator discovery only).

### 1.4 The DTR hazard, precisely (why consoles are "never auto-match")

- Exporter behavior: acquiring a place that matches a
  `USBSerialPort`/`RawSerialPort` triggers `SerialPortExport`
  (`remote/exporter.py:189`, exports map :304-305) which spawns ser2net
  with connector `serialdev(nouucplock=true),<path>,<speed>n81,local`
  (line 259). Opening the tty asserts DTR → the Espressif USB-JTAG
  bridge chip-resets the board. Bench doctrine (verified live
  2026-10-13, documented in `/etc/labgrid/exporter-ccid.yaml` header and
  `tools/hil/README.md` §3): **console ports are exported as
  enrollment records only; the OTA places match `NetworkService` only.**
- labgrid's `SerialDriver` opens rfc2217 **with `ign_set_control`**
  (serialdriver.py:46), so once a console session is intentionally
  opened, labgrid itself will not flap the control lines. The hazard is
  the *open* (by ser2net's serialdev connector), not labgrid traffic.
- Per-adapter reset straps (bench evidence — runbook §3 and
  `esp32_flash.py::_boot_capture`):

| Bridge | DTR | RTS | Safe open |
|---|---|---|---|
| Hades2001/FT2232 (m5stick) | inert | **EN** (reset pulse) | `dtr=False,rts=False`, pulse RTS to reset |
| Espressif USB JTAG/serial (nucula-C3, T-Display-S3) | esptool auto-reset pair with RTS | pair | open with both low; pulse RTS only |
| ST-LINK V2.1 VCP (F469/F746) | no reset strap | — | open-safe |
| FT232/CP210x/CH340 (tollgate atoms, CYD) | auto-reset circuit on dev boards | pair | treat as DTR-hazard |

- ser2net 4.6.0 carries modem-control settings **DTRHI/DTRLO/RTSHI/RTSLO**
  (present in the installed binary's strings) — a bench-owned ser2net
  config can pin DTR/RTS deasserted at open. labgrid's exporter cannot
  (its connector string is hardcoded), so this is only available for a
  self-managed ser2net instance (§3.4 Pattern C).

### 1.5 Existing discipline to keep (assets, not liabilities)

| Asset | File | What it encodes |
|---|---|---|
| ESP32 flash spine | `tests/hardware/labgrid/esp32_flash.py` (`flash_esp32`, 522 lines) | pcscd socket+service quiesce under a **held** flock (`/run/lock/bench-pcscd.lock`); baud capped to exporter-configured speed (`baud_from_exporter`); **writes first, descending offsets (PT lands last), destructive erases only after all writes**; USBDEVFS_RESET → ftdi_sio rebind retry ladder; FTDI latency_timer=1 restore; boot proof (`boot_check_no_conflict` — `Calling app_main`, banner-only evidence rejected); CLI for build.sh. |
| STM32 flash spine | `tests/hardware/labgrid/stm32_flash.py` | `st-flash --connect-under-reset --serial <pinned>` write **then** reset; ST-LINK USBDEVFS_RESET latch recovery; ST-LINK serial resolved from the exporter config (board-crossing guard, 2026-10-14 F469-image-on-F746 incident). |
| Bench exclusivity | `tests/hardware/labgrid/bench.py` | flock `/tmp/amperstrand-bench.lock` **before** place acquire (AB-BA order, cross-project); `acquired_place`; run ledger `results/history.jsonl`. |
| Inventory probe | `tests/hardware/labgrid/bench_inventory.py` | parses in-repo exporter configs as source of truth; acquire/release probes with held-place skip; `--json` for the nightly. |
| Builders | `firmware/esp32-ccid/build.sh`, `flash_and_test.sh` | per-variant chip/baud/offset tables; sdkconfig+IDF-version cache stamping; `elf2image --flash-mode dio` + header-byte guard (#309); PT rides along with app; otadata erase at hardcoded `0x30000`. |
| tollgate-lab (in-house) | `/root/src/tollgate-lab/tollgate_lab/` | `drivers/esp_flash.py::EspFlashDriver` (registered labgrid Driver, esptool via a `CommandProtocol` shell, reset-method aware), `hardware/port.py` (by-serial port resolution, per-serial `FlashPolicy` baud caps), `hardware/bench_lock.py`. The org's shared-harness home. |

### 1.6 The actual root cause (why bundles)

`build.sh` flashes `0x8000=<OTA PT>` + `0x40000=<app>` and erases
`0x30000` ("otadata") — but under the micronuts factory table the runbook
proved the board ran, **0x30000 IS the factory app**, and the OTA app
slot is elsewhere. Meanwhile no bootloader (0x0) is ever written, so an
old prebuilt v5.5.1 bootloader survived under v5.5.5 images. Three
independent provenances (bootloader on flash, PT+app from build A,
erases tuned for table B) with **no machine-checkable statement of what
a consistent set is** and **no record of what's on the device**. Both
gaps are fixable with data, not hardware.

---

## 2. Target architecture — principles

1. **One gate**: every flash goes through the bundle-aware tool under
   the bench flock + place. No ad-hoc esptool/st-flash against bench
   ports (the runbook's own step-2 is the last sanctioned exception).
2. **Bundle-or-nothing**: the flashable unit is a *bundle directory*
   with a manifest; mixed-family sets are refused structurally, not by
   operator care.
3. **Inventory is written by the tool**: what's on each device is
   recorded on every successful flash and reconciled before the next.
4. **labgrid coordinates, scripts operate**: labgrid owns identity,
   exclusivity, power, and console *enrollment*; the flash logic stays
   in the repo Python (portable, testable, already disciplined).
5. **Never auto-match consoles** stays the default; console access
   becomes deliberate and typed (§3.4).

---

## 3. Target architecture — per device class

### 3.1 ESP32 serial DUTs (nucula-C3, m5stick, T-Display-S3, CYD, atoms)

**Resources per DUT (exporter config, `/etc/labgrid/` + in-repo copy):**

```yaml
# ai-legion-nfc exporter (existing pattern extended)
nucula-c3:                      # acquisition token — plain SerialPort, no ser2net on acquire
  SerialPort:
    port: /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_90:DA:72:9A:50:18-if00
    speed: 115200

nucula-console:                 # NEW group: deliberate console (see §3.4 Pattern B)
  USBSerialPort:
    port: /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_90:DA:72:9A:50:18-if00
    speed: 115200

nucula-pwr:                     # NEW group once a switchable hub exists (§5)
  USBPowerPort:
    match: 'ID_SERIAL_SHORT=<hub-serial> ID_PATH=pci-0000:00:14.0-usb-0:X'
    index: 2                    # hub downstream port the DUT occupies
```

**Places** (coordinator):

| Place | Matches | Purpose |
|---|---|---|
| `nucula-c3` (exists) | `SerialPort` token | flash/test exclusivity token — acquire, run tool, release |
| `nucula-console` (new) | `nucula-console/USBSerialPort` | deliberate interactive console; acquire = accepted reset |
| `nucula-pwr` (new) | `nucula-pwr/USBPowerPort` | `labgrid-client -p nucula-pwr power cycle` = remote replug |

Two places over one physical port still don't exclude each other in
labgrid — that remains the bench flock's job (unchanged doctrine), but
each *intent* (token / console / power) is now a named, visible object
instead of a yaml comment.

**Flash cycle** (tool = bundle-aware `esp32_flash.py`, §4):

```
bench_lock (/tmp/amperstrand-bench.lock)          # 1. cross-project
  → labgrid-client -p nucula-c3 acquire           # 2. per-place
    → quiesce pcscd (held pcscd flock)            # 3. port ownership
    → verify bundle + reconcile inventory         # 4. §4 policy
    → esptool write-flash (descending offsets)    # 5. existing spine
    → erase otadata (offset FROM THE BUNDLE PT)   #    never hardcoded
    → boot capture (safe-open, RTS pulse) + signature from bundle
    → on success: update inventory + place tags + ledger
  → release
→ unlock
```

Failure ladder stays exactly as today (retry → USBDEVFS_RESET → ftdi
rebind → retry), with one upgrade: when the DUT has a `*-pwr` place,
the "PHYSICAL REPLUG" verdict becomes `labgrid-client -p <dut>-pwr
power cycle` (§5).

### 3.2 ESP32 OTA seats (ccid-nucula, ccid-m5stick)

Unchanged topology (NetworkService-only, `tests/hardware/vpcd/ota_push.py`),
with two additions:

- `ota_push.py` gains `--bundle`: reads the manifest, pushes the app
  image, and records bundle_id/family in the same inventory (OTA flash
  = app-only reflash; the PT/bootloader on the device must already be
  the same family — checked against inventory, refused on mismatch or
  unknown).
- The rollback-hardened OTA service already guards bad images; the
  inventory entry notes `slot=ota_1 via OTA` so a later serial flash
  reconciles correctly (reflash of PT/bootloader is required anyway →
  full-bundle rule).

### 3.3 STM32 (F469-DISCO, F746-DISCO)

- Keep `st-flash` under place lock (`stm32_flash.py` spine). The
  ST-LINK stays pinned by serial from the exporter config
  (`resolve_stlink_serial`); the Cherry iSerial (`ST2XXX-001` /
  `ST2XXX-F746`) doubles as the **boot-verify signal**: after reset,
  the DUT must enumerate in pcscd/lsusb with the expected iSerial —
  same "boot proven ≠ flash verified" doctrine as the ESP32 side.
- STM32 bundles (§4) carry a single `app` target at `0x08000000` +
  expected iSerial + git rev; mixed-project hazards (bolty gm65 vs
  ccid Cherry vs micronuts wallet on one board) become inventory
  checks: the tool refuses to put a `ccid-cherry` bundle on a device
  whose inventory says `bolty-gm65` unless `--force-family` + free-text
  reason (restores are legitimate; silent crossovers are not).
- **DFU (`DFUDriver`)**: only relevant if we ever wire DFU boot wiring
  (DISCO boards expose ST-LINK, not DFU). Enroll `DFUDevice`
  (`resource/udev.py:360` auto-matches `:fe0102:`) only when such a
  target exists; today: no.
- **OpenOCD (`OpenOCDDriver` + `USBDebugger`)**: optional replacement
  for st-flash if we want flashing through the labgrid driver graph
  (e.g. from remote hosts via `NetworkUSBDebugger`). Not needed while
  flashing happens on .208 itself.

### 3.4 Console safety patterns (the DTR hazard, answered)

**Pattern A — enrollment-only + safe-open capture (default, current).**
Console groups exported but matched by no place; boot-verify capture is
`esp32_flash._boot_capture`: pyserial open with `dtr=False, rts=False`
then an explicit RTS pulse. Zero surprise resets. Keep for all
automated flows.

**Pattern B — explicit console places (recovery/debug).**
`<dut>-console` places matching the `USBSerialPort` group. Acquiring is
a *deliberate, visible act* that resets the DUT (often desired
post-flash). Interactive use: `labgrid-client -p m5stick-console
console` (microcom → rfc2217). Rules: acquire only under the bench
flock (extend `bench.acquired_place` usage in the recovery runbooks);
the place comment carries the strap table from §1.4 so the operator
knows what a connection does.

**Pattern C — reset-safe persistent console (upgrade, needs one
empirical check).** A bench-owned ser2net instance (systemd unit, not
the labgrid exporter) serving the console ports with `DTRLO,RTSLO`
modem-control settings (ser2net 4.6.0 supports them — tokens verified
in the installed binary) plus a TCP port per DUT; exporter declares
`NetworkSerialPort` (protocol `rfc2217` or `raw`) pointing at it.
Opening that console does **not** change DTR/RTS → no chip reset;
reset becomes an explicit rfc2217 control-line op (or the DUT's
power place). This is the pattern that lets a `nfc-watch.sh`-style
always-on monitor live *inside* labgrid coordination instead of a
root tmux session. Migration gate: verify on the Hades2001 and
Espressif bridges that `DTRLO,RTSLO` at open really leaves the DUT
alone (expectation: yes — the reset comes from DTR assertion on open,
and labgrid's `SerialDriver` already opens with `ign_set_control`).

Boot-log capture for tests can then be `SerialDriver`-based
(`ConsoleExpectMixin`), or stay on `ExternalConsoleDriver(cmd:
"microcom …")` — both ship in 26.0.

### 3.5 Flash as a driver operation vs script-under-place-lock — decision

**Script-under-place-lock is the spine.** Reasons:

- The discipline (pcscd quiesce, baud caps, ordering, ladder, boot
  proof) is already implemented and unit-tested in repo Python; a
  labgrid driver graph would have to re-express it inside per-role env
  files that the multi-project bench does not share.
- Cross-project exclusivity (bolty/micronuts/ccid) is the bench flock,
  which lives *outside* labgrid; the tool must hold flock → place →
  pcscd lock in that order regardless.
- labgrid's value here is identity + visibility + power + console
  enrollment — none of which require the flasher to be a Driver.

**Driver wrappers as an adapter, not the spine**: register thin labgrid
drivers in `tollgate_lab.drivers` (precedent: `EspFlashDriver`) that
call the same `flash_esp32`/`flash_f469` functions, so pytest flows
with `--lg-env` (fixtures `env`/`target`/`strategy`) and a
`GraphStrategy` (`state_flashed` → `state_verified` → `state_console`)
can drive the cycle where that shape pays off. `FlashScriptDriver` +
`ManagedFile` is only needed when flashing must run *from a remote
host through the exporter* — explicitly out of scope for now.

---

## 4. Flash bundle format (spec)

### 4.1 Producer

Every builder (ccid `build.sh --bundle-out`, `flash_and_test.sh`,
micronuts `build-nfc.sh`, tollgate-s3 `scripts/hil`, bolty/micronuts
builders later) emits, next to the app image:

```
<bundles-root>/<board>/<bundle_id>/
  manifest.json
  bootloader.bin        # if the family flashes one
  partition-table.bin
  app.bin
```

`bundles-root` default: `$CARGO_TARGET_DIR/bundles/` (bench-local,
same lifecycle as images today).

### 4.2 manifest.json

```json
{
  "schema": "amperstrand.flash-bundle/1",
  "bundle_id": "sha256 of the canonical manifest (see 4.3)",
  "git_rev": "5a6948e…",
  "git_dirty": false,
  "built_at": "2026-10-20T14:03:11+02:00",
  "project": "micronuts-nfc",
  "board": "m5stick",                  // build.sh variant name
  "chip": "esp32",
  "family": {
    "esp_idf_version": "v5.5.5",       // bootloader/PT/app must all come from this
    "sdkconfig_id": "sha256(sdkconfig file)",
    "sdkconfig_name": "sdkconfig-xtensa.full",
    "toolchain": "esp/1.98.1.0"
  },
  "layout": {                          // PARSED from the partition CSV at build time
    "source": "partitions-ota.csv",
    "id": "ota:v1",                    // stable human name; canonical = parsed offsets below
    "partitions": {
      "nvs":       {"offset": "0x9000",  "size": "0x20000"},
      "phy_init":  {"offset": "0x29000", "size": "0x1000"},
      "otadata":   {"offset": "0x30000", "size": "0x2000"},
      "ota_0":     {"offset": "0x40000", "size": "0x3F0000"}
    },
    "app_slot": "ota_0"
  },
  "targets": {                         // what gets written, in ONE esptool invocation
    "bootloader":      {"offset": "0x0",    "path": "bootloader.bin", "sha256": "…", "bytes": 26560},
    "partition_table": {"offset": "0x8000", "path": "partition-table.bin", "sha256": "…", "bytes": 3072},
    "app":             {"offset": "0x40000","path": "app.bin", "sha256": "…", "bytes": 3367392}
  },
  "otadata_policy": "erase-if-family-or-layout-change",   // see 4.5
  "boot_signature": "nucula>",          // per-variant console proof (string or list)
  "boot_signature_reject": ["i2c: CONFLICT", "CONFLICT! driver_ng is not allowed"],
  "baud_cap": 115200,                   // per-board proven ceiling (Hades2001 = 115200)
  "expected_identity": {                // STM32 side uses iSerial; ESP32 uses boot_signature
    "usb_iserial": null
  }
}
```

STM32 bundle: same schema with `targets: {"app": {"offset": "0x08000000", …}}`,
`layout: null`, `expected_identity.usb_iserial: "ST2XXX-001"`,
`boot_signature: null` (boot proof = USB enumeration).

### 4.3 bundle_id

`sha256` over the canonical JSON of the manifest **with `bundle_id`
removed**, keys sorted, no whitespace. Files referenced by the manifest
are content-hashed into `targets[].sha256`, so a rebuilt-dirty-tree
bundle never aliases a clean one (`git_dirty` is also recorded).

### 4.4 Self-consistency checks (producer AND flasher re-verify)

1. Every `targets.*.path` exists next to the manifest; `sha256` and
   `bytes` match the file.
2. `targets.app.offset` == `layout.partitions[layout.app_slot].offset`
   — the app lands in a slot the flashed table actually defines.
3. `targets.partition_table` present ⇒ `layout` present and parsed
   from the same CSV that produced the table (producers embed the parse;
   the flasher re-parses the CSV when shipped alongside, else trusts
   the producer's recorded parse).
4. ESP32 app image header byte 2 == `0x02` (DIO, issue #309) — the
   existing build.sh guard, moved into the bundle check.
5. `family.esp_idf_version` present whenever `bootloader` is a target.

### 4.5 Refusal matrix (the flash tool's policy)

| Request | Inventory says | Verdict |
|---|---|---|
| full bundle (bl+PT+app) | anything / unknown | **flash** (this is the only path that may change family/layout) |
| app-only, same `bundle_id` | same id | **flash** (idempotent reflash; otadata preserved) |
| app-only, same family+layout, newer rev | family+layout match | **flash**; otadata per policy |
| app-only | family OR layout mismatch | **REFUSE** — “device carries <id/family/layout>; full bundle or `--reprovision`” |
| any flash | inventory unknown/stale (>1 flash-less family change) | **REFUSE** unless `--allow-unknown-inventory` (first adoption uses `--seed-inventory`) |
| erase otadata at offset X | X ∉ bundle layout `otadata` | **REFUSE** — this is the 0x30000-is-the-factory-app trap, structurally impossible now |
| PT-only or bootloader-only | anything | **REFUSE** — never split the structural pair from the app |

`--reprovision` = shorthand that re-issues as full bundle (same
manifest must contain bl+PT+app; producers that don't build a
bootloader declare `family.esp_idf_version` of the on-device one and
the tool still requires PT+app together).

### 4.6 Device inventory — where it lives

Two layers, one writer (the flash tool, under the bench flock):

1. **Source of truth — bench-local generated file**:
   `/var/lib/amperstrand-bench/device-inventory.json` (host-level,
   survives repo/worktree churn, shared by all projects on the bench —
   same reasoning as the flock path; created by the tool, format
   versioned like the manifest):

   ```json
   {
     "schema": "amperstrand.device-inventory/1",
     "devices": {
       "m5stick": {
         "port": "/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0",
         "bundle_id": "…", "git_rev": "5a6948e…", "project": "micronuts-nfc",
         "family": {"esp_idf_version": "v5.5.1", "sdkconfig_id": "…"},
         "layout_id": "factory:v1",
         "flashed_at": "2026-10-18T18:09:…", "flashed_by": "root@ai-legion",
         "last_boot_proof": "2026-10-20T…", "note": null
       }
     }
   }
   ```

   Keyed by the stable device identity (the by-id serial stem), not by
   port path. Appends also go to the existing
   `results/history.jsonl` ledger (`bench.ledger`), which stays the
   append-only audit trail.

2. **Broadcast view — labgrid place tags**: after each successful
   flash the tool runs `labgrid-client -p <place> set-tags
   bundle=<bundle_id[:12]> family=esp-idf-v5.5.5 layout=ota:v1
   rev=5a6948e`. Tags are visible in `labgrid-client places`/`show`
   (e.g. `nucula-c3` already carries `board=… class=… transport=…`),
   so *any* session on any host sees what's on the device without
   reading the file. Tags are advisory (coordinator state); the file is
   authoritative — a mismatch is itself a red flag the tool reports.

Seed data exists: the runbook already established the m5stick's
contents by forensics (`mn-app4.bin` sha256, factory:v1 layout,
micronuts-nfc @5a6948e) — first migration step turns that archaeology
into an inventory row.

---

## 5. Power / USB reset integration (the replug killer)

### 5.1 Mechanism (all shipped in 26.0)

- Export a `USBPowerPort` resource per switchable hub port the DUT
  hangs off (`resource/udev.py:638`, match the hub by serial/path,
  `index:` = downstream port).
- Match it into the DUT's power place →
  `labgrid-client -p <dut>-pwr power off|on|cycle|get` just works:
  the client auto-instantiates `USBPowerDriver`
  (`remote/client.py:955`), which shells `uhubctl -l <path> -p <index>`
  (`driver/powerdriver.py:370-378`). No env file, no code.
- `cycle()` sleeps `delay` (default 2 s) between off/on — for FTDI
  wedges use ~5 s so the host actually sees the detach/attach.

### 5.2 Hardware to buy

1. **Yepkit YKUSH3** (~€30, 3 switchable ports) — zero-dependency path:
   labgrid's native `YKUSHPowerDriver`/`YKUSHPowerPort`
   (`driver/powerdriver.py:273`, `resource/ykushpowerport.py`), no
   uhubctl build needed. Right size for 2–3 DUT seats.
2. **A uhubctl-compatible smart hub** (per the mvp/uhubctl tested-hub
   list — pick one verified there; generic hubs without per-port PPC
   control are useless here, and ganged switching ports switch
   together). Needs uhubctl built/installed on .208 (not packaged
   everywhere; trivial make install) + `uhubctl` on PATH for the
   exporter user. Prefer a model where EACH downstream port switches
   independently; one hub per 4–7 DUT seats.

Both give the same labgrid surface; YKUSH3 is the fast start, smart
hubs scale.

### 5.3 What a port power-cycle actually fixes (and what it doesn't)

- ✅ **FTDI/FT2232 bridge wedges** (the "enumerates, zero data,
  replug-class" of 2026-10-20's TAKEOVER note, and esp32_flash's
  physical-replug verdict): the bridge is USB-powered; VBUS cycle
  re-enumerates it exactly like a replug.
- ✅ **ST-LINK latch** (`.208` class) — same mechanism as its
  USBDEVFS_RESET ladder step, but stronger.
- ✅ Chronic staller DUTs (the ws3915i class) when the DUT draws USB
  power through the switched port.
- ⚠️ **m5stick specifically**: the StickC carries a battery — a VBUS
  cycle resets the *bridge* (which is the fix for the wedge) but not
  necessarily the MCU. Full DUT cold boot there still needs EN (RTS)
  or battery disconnect; say so in the place comment.
- ⚠️ A hub-port cycle also drops any USB device sharing that port —
  one DUT per switched port, period.

Ladder integration: `esp32_flash.py`'s final "PHYSICAL REPLUG required"
verdict becomes: if the place has a `*-pwr` sibling →
`labgrid-client -p <dut>-pwr power cycle` + one more retry; only then
ask for hands.

---

## 6. How other labgrid users handle this (package design + precedent)

No offline issue tracker in the installed package; reasoning from the
package's own design plus in-house precedent:

- Upstream labgrid has **no Espressif support at all** (§1.3) — every
  external project wraps its own flasher. The sanctioned hook is
  `FlashScriptDriver` (transfer-and-run *any* script on the exporter
  host, templated with the device) or a custom registered `Driver`
  exactly like in-house `tollgate_lab.drivers.esp_flash.EspFlashDriver`
  and `tollgate_lab.drivers.serial_console`. We are not fighting the
  framework; we're using its intended extension seam.
- The resource/driver/protocol split (resources = *identity*,
  drivers = *operations*, strategies = *lifecycle*) is deliberately
  host-agnostic: every udev resource has a `Network*` twin
  (`resource/remote.py`) so the same place config works when flashing
  moves off the bench host later.
- Upstream's model for "one bench, many projects" is **one coordinator,
  many exporters, per-place matching**. What upstream does NOT
  provide is cross-place exclusivity for shared ports (two matches on
  one device don't conflict) — the bench flock is the correct local
  answer, already canon (`conwrt-bench docs/labgrid-coordination.md`,
  bolty-rs `docs/labgrid-bench-sharing.md`) — the bench runs exactly
  this shape today (10 exporters, 39 places).
- The pytest plugin (`env`/`target`/`strategy` fixtures) plus
  `GraphStrategy` is how driver-graph users express flash→boot→shell
  lifecycles; our script spine can grow into that via §3.3's adapter
  without changing the spine.
- Closest in-house full pattern: **tollgate-s3-hil** (place comment:
  "flash + boot-log gates, esptool image backup/restore") and
  **tollgate-lab** (BenchLock, per-serial FlashPolicy baud caps) — the
  bundle layer is the missing piece across all of them, which is why
  it belongs in `tollgate_lab` once proven in ccid (§7 M6).

---

## 7. Migration steps (smallest first)

**M0 — inventory seed (no behavior change).**
Create `/var/lib/amperstrand-bench/device-inventory.json` from the
runbook's forensic facts (m5stick = micronuts-nfc @5a6948e,
factory:v1, mn-app4 sha256; nucula/F469/F746 = current known state).
Tag the places once. Acceptance: `labgrid-client show m5stick` displays
family/layout; inventory file committed to the bench state dir with
0600.

**M1 — manifest producer.**
`build.sh`/`flash_and_test.sh` emit `manifest.json` + bundle dir
(bundler = a small python helper `tools/hil/make_bundle.py` shared by
all projects; parsing partitions CSV is 20 lines with `csv` + int(x,0)).
Keep existing flashing working unchanged (manifest produced, not yet
required). Acceptance: two consecutive builds of the same rev produce
identical `bundle_id`; a dirty tree changes it.

**M2 — bundle-gated ESP32 flash.**
`esp32_flash.py` gains `--bundle <dir>` (+ `--targets app` for the
app-only path) implementing §4.4–4.5: sha256 verify, refusal matrix,
otadata offset from the bundle layout (delete the hardcoded
`0x30000`), boot signature from the manifest, inventory+tags+ledger
write on success. `build.sh --flash` switches to `--bundle`.
Acceptance: (a) app-only flash with mismatched family refused;
(b) otadata erase targeting a factory-layout device refused;
(c) successful full flash updates `show` tags within seconds.

**M3 — explicit console + docs.**
Create `<dut>-console` places matching the enrollment USBSerialPort
groups; move the §1.4 strap table into place comments + the recovery
runbooks; extend `bench.py` helpers with `console_place()` (flock →
acquire, loud warning). Acceptance: acquiring a console place resets
the DUT *and says so in its comment*; automated flows still touch no
console place.

**M4 — power.**
Buy YKUSH3 (or a listed smart hub), install uhubctl if going the hub
route, enroll `USBPowerPort` groups + `*-pwr` places, wire the ladder
upgrade into `esp32_flash.py`/`stm32_flash.py`. Acceptance: a wedged
FTDI bridge recovers unattended via `power cycle` + retry; the
replug-verdict log names the power place when one exists.

**M5 — STM32 bundle.**
Same bundler for F469/F746 (`app@0x08000000`, expected iSerial);
`stm32_flash.py` reconciles inventory (ccid-cherry vs bolty-gm65 vs
micronuts-wallet on the shared board) and writes the same records.
Acceptance: cross-project overwrite attempt is refused with the
current owning project named.

**M6 — org-wide lift.**
Move bundler + inventory client into `tollgate_lab` (bundles,
inventory, policies); adopt in tollgate-s3-rs and micronuts
`build-nfc.sh`; optional labgrid driver adapter + `GraphStrategy` env
for pytest-driven flash/verify cycles.

**M7 (optional) — reset-safe persistent console.**
Pattern C ser2net unit with `DTRLO,RTSLO`, `NetworkSerialPort`
resources, migrate `nfc-watch.sh` from root-tmux into a coordinated
monitor. Gated on the §3.4 empirical check.

---

## 8. Open questions

1. **ser2net `DTRLO/RTSLO` on-open behavior** on the Hades2001 and
   Espressif bridges — verify before M7 (expectation: deasserted-at-open
   removes the chip reset; the `local` flag in labgrid's own connector
   is evidently not enough per bench doctrine).
2. **Hub choice**: YKUSH3 (native driver, 3 ports) vs uhubctl-listed
   smart hub (scalable, extra dep) — decide at M4 purchase; also
   whether the m5stick seat needs a battery-disconnect mod for true
   cold boots.
3. **Inventory write rights**: the flash tool (often run via sudo for
   pcscd control) must write `/var/lib/amperstrand-bench/` and call
   `set-tags` on the coordinator — pick the owning user/group now to
   avoid a root-only inventory.
4. **Bootloader policy for c3**: bundles that build their own
   bootloader (esp-idf-sys does) vs boards carrying Espressif
   prebuilts — should full bundles always write 0x0 (safest, ends the
   v5.5.1-under-v5.5.5 class) even though it lengthens flash time?
   Proposal: yes, always.
5. **Foreign-project adoption order**: bolty-rs/micronuts currently
   flash ad-hoc; until they adopt the bundler, their flashes will
   make inventory stale — the tool treats “inventory older than the
   last flash window on the bench flock” as unknown (§4.5 row 4).
   Is `--allow-unknown-inventory` enough, or should the flock ledger
   (`history.jsonl`) grow a foreign-flash event type?
6. **CYD / tollgate atoms**: they live on other exporters but share
   the bench flock — fold their baud policies (`FlashPolicy` in
   tollgate-lab) into `baud_cap` in manifests, or keep two sources?
7. **Does `labgrid-client power` need `uhubctl` on PATH for the
   exporter's user** (system service) — confirm at M4 with the actual
   hub (YKUSH path avoids the question entirely).
