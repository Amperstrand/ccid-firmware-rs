# HIL labgrid enrollment — ccid bench OTA seats

Labgrid environment for the two ESP32 DUTs of the ccid-firmware bench
(`ai-legion`, 192.168.13.208): the **nucula** (ESP32-C3 + PN7160) and the
**m5stick** (ESP32 + MFRC522), plus the bench's ACR1252 reference reader.
Follows the bolty-rs `tools/hil/` pattern (the lab's canonical labgrid
deployment — see `bolty-rs docs/labgrid-bench-sharing.md`); the repo's
serial-DUT places keep living in `tests/hardware/labgrid/` (owned by the
test suite, not touched here).

Verified end-to-end 2026-10-13: transcript in `results/enroll-transcript.txt`
(both places acquired + released; `lsof` proves the console ports stayed
closed the whole time).

## 1. What runs where

| Piece | Where | Notes |
|---|---|---|
| labgrid coordinator | `ai-legion`, system unit `labgrid-coordinator.service`, `0.0.0.0:20408` | pre-existing, bench-wide; persists places via `StateDirectory=labgrid` (`/var/lib/labgrid/places.yaml`) — coordinator restarts do NOT wipe places here |
| exporter `ai-legion-ccid` | `ai-legion`, systemd **--user** unit `labgrid-exporter-ccid.service` (root), config `tools/hil/labgrid-exporter.yaml` | this repo's enrollment: OTA NetworkServices + console USBSerialPort tokens |
| exporter `ai-legion-nfc` | `ai-legion`, system unit, config `tests/hardware/labgrid/exporter-ai-legion-nfc.yaml` → `/etc/labgrid/exporter-nfc.yaml` | pre-existing: serial tokens (`nucula-c3`, `m5stick-usb` — plain `SerialPort` class), STM32 DUTs, reference-reader USB identities. Not touched here |
| places `ccid-nucula`, `ccid-m5stick` | coordinator | network-only matches, labels `board=` / `transport=network` |

Resources exported by `ai-legion-ccid` (as seen by `labgrid-client resources`):

| Resource | Class (client-visible) | Purpose |
|---|---|---|
| `ai-legion-ccid/nucula/NetworkService` | NetworkService | OTA endpoint `192.168.13.230:3232` — acquire `ccid-nucula` to own it |
| `ai-legion-ccid/m5stick/NetworkService` | NetworkService | OTA endpoint `192.168.13.234:3232` — acquire `ccid-m5stick` to own it |
| `ai-legion-ccid/nucula/USBSerialPort` | NetworkSerialPort | console `/dev/ttyACM0` — **matched by no place, see §3** |
| `ai-legion-ccid/m5stick/USBSerialPort` | NetworkSerialPort | console `/dev/ttyUSB0` — **matched by no place, see §3** |

## 2. Start / stop / re-create

```bash
systemctl --user status labgrid-exporter-ccid        # exporter health
journalctl --user-unit labgrid-exporter-ccid -f      # its log
systemctl --user restart labgrid-exporter-ccid       # after editing the yaml
tools/hil/labgrid-place.sh                           # idempotent place (re)creation
```

The unit lives at `~/.config/systemd/user/labgrid-exporter-ccid.service`
(install instructions in `tools/hil/labgrid-exporter.service`). It points
at this worktree's yaml — when the checkout moves to a stable path, update
the `ExecStart` line and re-`daemon-reload`. User units start with root's
user manager; for boot persistence without a login session run
`loginctl enable-linger root` (deliberately NOT enabled here).

Never run `labgrid-exporter` interactively — it holds connections and
blocks the shell (bolty-rs lesson).

## 3. The DTR hazard — why the places match NetworkService only

labgrid 26.0 serves `USBSerialPort` through ser2net **on acquire**
(`SerialPortExport._start`): acquiring a place that matches one spawns a
ser2net listener that OPENS the device. Opening the nucula console
(`/dev/ttyACM0`) toggles DTR and chip-resets the board
(`docs/VPCD_WIFI_RUNBOOK.md`: "USB serial is only needed for recovery").
So:

- `ccid-nucula` / `ccid-m5stick` match **only** their NetworkService.
- The USBSerialPort entries in the exporter yaml are enrollment records
  for future console places: add the match only when a console session is
  intended AND the bench is free, under the bench flock.
- The same physical ports are ALSO exported as pure tokens by the system
  exporter `ai-legion-nfc` (groups `nucula-c3` / `m5stick-usb`, plain
  `SerialPort` class — no ser2net on acquire, used by the
  `tests/hardware/labgrid/` suite). Two token families for one port do
  NOT exclude each other through labgrid; the layer that does is the
  bench flock (§4).

## 4. How tests SHOULD acquire (the flock convention)

Same three-layer protocol as `tests/hardware/labgrid/bench.py` and
bolty-rs, order never reversed (AB-BA):

1. **Bench flock first** — `flock /tmp/amperstrand-bench.lock` via
   `bench.bench_lock()` (already implemented in
   `tests/hardware/labgrid/bench.py`). The only layer that excludes
   same-user sessions across projects/harnesses and survives coordinator
   restarts. Mandatory for anything that opens a serial port or touches
   pcscd.
2. **labgrid place second** — `bench.acquired_place("ccid-nucula")` or
   plain `labgrid-client -p ccid-nucula acquire` / `release`. Busy →
   skip with the holder from `labgrid-client who`, never force.
3. Identity probes last (readers by USB serial / Escape 0x02, never
   enumeration order).

## 5. OTA flash path for tests

```bash
tools/hil/labgrid-place.sh                                  # ensure places
labgrid-client -p ccid-nucula acquire                       # own the OTA seat
python3 host-tools/ota_push.py 192.168.13.230 \
    ~/.cargo-target/esp32-ccid-wifi-vpcd.bin                # OTA :3232
labgrid-client -p ccid-nucula release
```

- nucula: `192.168.13.230:3232` — m5stick: `192.168.13.234:3232`
  (params visible in `labgrid-client -p ccid-nucula show`).
- The full procedure (build with `NUCULA_VPCD_HOST`, `NUCULA_VPCD_PORT`
  per board, ufw, pcscd restart order, rollback semantics) is
  `docs/VPCD_WIFI_RUNBOOK.md` — this file only owns the acquisition
  wrapper.

## 6. vpcd is outbound — deliberately not a labgrid resource

The vpcd link runs **board → host**: the firmware connects out to the
host's `libifdvpcd.so` listeners (`:35963` = nucula slot "00 00",
`:35964` = m5stick slot "00 01"). An exporter-side NetworkService for it
would be redundant (nothing to acquire inbound) — the OTA NetworkService
above is the only board-side service tests need to own. Netlog UDP
`:4567` likewise. See the runbook for the ufw rules.

## 7. ACR1252 reference reader — host-side, already enrolled

The ACR1252 is a pcscd USB reader: no labgrid driver is needed or useful
for it. Tests reach it through pcscd **by name** (never enumeration
order). For exclusivity it is already enrolled on the coordinator by the
system exporter: place `ref-acr1252` matching
`ai-legion-nfc/ref-acr1252/USBDevice` (udev identity
`072f:223b` pinned to its physical USB port). Nothing duplicated here —
acquire that place if a session needs to own the reader.

## 8. Future e2e integration (tests/hardware/vpcd/) — intended, NOT implemented

The vpcd e2e suite should later grow a labgrid conftest. Integration
points, deliberately left as documentation only:

- Env var pointing at this env file, e.g.
  `export LG_ENV=$REPO/tools/hil/labgrid-env.yaml` (plus
  `LG_COORDINATOR=127.0.0.1:20408` when not on the bench host) — the
  file defines one target per board (`nucula`, `m5stick`), each a
  `RemotePlace` for the ccid-* places.
- With the official plugin: `pytest --lg-env tools/hil/labgrid-env.yaml`.
- The conftest should reuse `tests/hardware/labgrid/bench.py`
  (`bench_lock` → `acquired_place`) rather than re-implementing the
  flock/ledger conventions, and skip with the holder's name when busy.
- Flashing stays `host-tools/ota_push.py` against the NetworkService
  address (§5) — never the serial ports, unless the flock is held and a
  recovery flash is intended (§3).

## 9. Files

| File | Role |
|---|---|
| `labgrid-exporter.yaml` | exporter config (deployed by the --user unit) |
| `labgrid-place.sh` | idempotent place creation + labels |
| `labgrid-env.yaml` | client env (targets/places; `-c` or `--lg-env`) |
| `labgrid-exporter.service` | systemd --user unit template (installed copy points here) |
| `results/` | verification transcripts (gitignored) |
