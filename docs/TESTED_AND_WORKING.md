<!-- TESTED_AND_WORKING.md — curated verification digest.
     DERIVED page (issue #183 doctrine: status is generated, never
     hand-written): every row below cites its evidence source — the
     generated docs/STATUS.md on branch `bench/status` (nightly bench
     inventory + HIL suites), a merged PR's hardware comment, or a
     dated bench session. When a row here disagrees with the generated
     STATUS.md, the generated page wins and this page must be fixed.
     Last full review: 2026-10-21. -->

# What works, what doesn't — tested combinations & known issues

This is the one-page answer to "what is the state of this firmware".
For the live per-night generated bench matrix (wiring, HIL suites,
drift), see **[docs/STATUS.md on the `bench/status` branch](
../blob/bench/status/docs/STATUS.md)** — that page is authoritative and
regenerated nightly; this page is the curated, human-navigable digest
of what that evidence means.

Legend: **PASS** = verified by test on hardware; **PASS (host)** =
verified by host-side unit/integration tests only; **SKIP** = hardware
absent/busy at verification time; **NOT TESTED** = never exercised on
that axis; **FAIL** = verified broken.

## Firmware × hardware × transport matrix

| Firmware target | Frontend | Transport to host | Card I/O verified | Status @ commit (date) | Evidence |
|---|---|---|---|---|---|
| STM32F469-DISCO (Cherry ST-2xxx profile) | contact slot, USART2 smartcard mode | USB CCID (TPDU level) | T=1 contact card (ComSign eID): ATR, APDU relay, T=0/T=1 | **PASS** @ e9173657 (2026-10-20, three-device stamp closing #308); labgrid HIL 7/7 @ 67bcfbc (2026-10-12) | #308 stamp; `pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil` |
| STM32F746-DISCO (Cherry profile, bit-bang) | contact slot, GPIO bit-bang | USB CCID | bench-distinct iSerial `ST2XXX-F746` landed; release build cached; **USB plug never re-attached since bring-up prep** | **NOT TESTED** (hardware off bench USB — see "Unavailable hardware") | 0af0fb0 prep; #302 deferred legs |
| ESP32 M5StickC + MFRC522 (serial CCID, `build.sh m5stick`) | NFC (ISO 14443-4 / ISO-DEP) | GemPC Twin serial over FTDI, 115200 8N2, pcscd via libccidtwin | J3R180 (P71D321) contactless: ATR, T=1 endpoint (S-blocks, IFS, WTX, chaining), full GP lifecycle incl. applet data ops; reconnect stability | **PASS** @ **428e834d (current main)** — full lifecycle GREEN incl. sustained LOAD (install + cleanup RC 0 on the main image, 2026-10-21 evening). The #372/#378 SW 6400 saga is closed: root cause was the WTX-grant encoding (the deployed J3R180 population REJECTS the spec-correct S(WTX response) 0xF3 byte; the fork sends the 0xF2 request-form echo — #386, wire-proven via netlog block trace) | #376/#386 merge evidence; #378 closure capture; this file's session log below |
| ESP32 M5StickC + MFRC522, SPP variant | NFC | Bluetooth Classic SPP → `/dev/rfcomm0`, pcscd | card-level verified, 30/30 soak, p50 48 ms/APDU | **PASS** @ m5stick-spp bench conf (2026-10-11) | firmware/esp32-ccid/README.md recipe |
| ESP32 M5StickC, wifi-vpcd variant | NFC | WiFi VPCD (libifdvpcd) | card-present + ATR + APDU round-trips; **EXTERNAL AUTHENTICATE 9000** (first ever on this path, 2026-10-21 — the #380 probe-churn fix landed in #386: mid-session probes answer from the cached ATR instead of reactivating the card). Remaining: GET STATUS 6982 post-auth (#380, vpcd-path-specific; serial path fully green with identical pins) | **PARTIAL** @ 428e834d | #380 trail; #386 wire trace |
| ESP32 M5Atom + MFRC522 (default backend) | NFC | serial | builds green in CI; on-device bring-up pending | **NOT TESTED** on hardware | `esp32-build-secondary` |
| ESP32 + PN532 (SPI) | NFC | serial | builds green; hardware verification pending | **NOT TESTED** on hardware | #249 bench card placements |
| ESP32-C3 nucula + PN7160 (`build.sh c3-ccid` + PTY broker) | NFC | USB-CDC → `nucula-ccid-broker` PTY → pcscd | J3R180: ATR, APDU relay, **full GP lifecycle green** (install/SELECT/data ops/uninstall/baseline) | **PASS** @ **f2efdb97 (current main)** — **FULL GP lifecycle green** (install 107 s → SELECT/VERIFY/GET DATA 9000 → uninstall → baseline restore, exit 0, 2026-10-21) + 3/3 broker sessions; wire-silence/coredump evidence at e9173657-era | #308/#327 evidence; `tests/hardware/nucula/` |
| ESP32-C3 nucula bring-up image (`build.sh c3`) | PN7160 ladder | console | NCI init ladder + PN7160 alive, FWID self-stamp | **PASS** @ 0b8e33e (2026-10-20) | #312 (closed) |

Direct pcscd/libccidtwin enumeration of the nucula CDC is impossible on
C3 silicon (USB-Serial/JTAG chip-resets on port-open, no disable bit —
#117): the PTY broker is the supported pcscd path for that board.

## pcscd-facing feature status (per capability, best-verified target)

| Capability | Status | Notes |
|---|---|---|
| USB enumeration (CCID descriptors, Cherry/Gemalto profiles) | **PASS** (F469) | reference-locked descriptors; profiles advertise TPDU exchange level |
| pcscd reader enumeration | **PASS** (F469 USB; m5stick serial; nucula broker) | all three transports simultaneously bench-proven 2026-10-12 |
| PowerOn / PowerOff / GetSlotStatus | **PASS** | contact + contactless |
| T=0 (contact) | **PASS** (F469) | procedure-byte handling on-device |
| T=1 transparent relay (reader role, USB) | **PASS** (F469) | #132 hardening: BWT-derived waits, honest-error contract; fidelity hammer 0 anomalies |
| T=1 endpoint (GemPC serial emulation role) | **PASS** (m5stick) | S-blocks answered locally, I-block reassembly, IFS negotiation, WTX dance, retry-idempotence (#101), session reset on reconnect (#345) |
| APDU exchange & response chaining | **PASS** (m5stick, nucula) | chained responses drained via R(ACK) in the wire harness |
| IFS negotiation | **PASS** | S(IFS request/response) with load-bearing EDC |
| WTX handling | **PASS** | S(WTX) grant dance; fuzz `t1_fuzz_wtx_dance_never_wedges` |
| Reader reconnection | **PASS** (m5stick, 5/5 cycles @ dd948466) | the #330/#345 class is fixed and regression-locked |
| NFC card activation (ISO 14443-4) | **PASS** | MFRC522 (reqa/rats retry, 20/20) + PN7160 (auto-activation model #88) |
| Diagnostics (Escape 0xD0 counters / 0xD1 coredump) | **PASS** (m5stick, nucula, F469) | F746 has no `diagnostics` override yet — fabricated zeros (#344, PR pending) |
| PIN pad | **PASS (host)** (Cherry profile descriptor + unit tests) | on-device PIN-pad use not exercised |
| OpenPGP applet interop (SmartPGP on J3R180) | **PASS** (nucula full, m5stick full post-#345/#341, ACR1252 full) | install/SELECT/GET DATA/VERIFY/uninstall/baseline-restore |
| GP secure channel through reader (SCP02) | **PASS as transparent relay** (F469 T=1 relay; nucula/m5stick via host gp) | reader never terminates the SC in reader role (doctrine) |

## Performance (measured, with stamps)

| Path | Metric | Value | When |
|---|---|---|---|
| m5stick serial GemPC ↔ J3R180 | APDU round-trip (empty SELECT, n=200, one connection) | **p50 42.0 ms / p95 47.0 ms / p99 47.1 ms**, connect 130.7 ms — measured @ dd948466 | 2026-10-21, `tests/hardware/perf/benchmark_readers.py -n 200` |
| Nucula broker ↔ J3R180 | APDU round-trip (n=200) | **p50 400.0 ms / p99 400.5 ms, connect 1196.6 ms** — the C3 USB-CDC per-command latency floor class (#117); optimization lane |
| ACR1252 reference oracle ↔ J3R180 | same workload | p50 8.84 ms / p95 8.96 ms, connect 38.9 ms (the ~5× serial-path gap is the known FTDI+firmware budget) | 2026-10-21, same run |
| m5stick serial | decomposition | ~16 ms class = ≈5–6 ms wire @115200 8N2 + ≈10 ms firmware gap (MFRC522 I2C @100 kHz + card I/O) | issue #51 decomposition, 2026-08-30 |
| m5stick SPP | p50 per APDU | 48 ms, 30/30 soak | 2026-10-11 |
| STM32F469 USB CCID | contact-card exchange p50 | 42.75 ms (n=60, post-DRY-phase-2) | 2026-10-11 |
| nucula broker | GSS RTT overhead | 0.72 ms p50 over PTY (direct CDC floor 0.20 ms); ~300 ms/command CDC latency floor for raw probes | #117, 2026-10-11 |
| Host tweak | FTDI `latency_timer` | 1 (not 4) removes up to ~4 ms batching | #51; bench scripts set it |

Baud >115200 on the serial path is host-blocked (libccidtwin hardcodes
B115200; #54 tracks the upstream knob).

## Unavailable / untested hardware (honest gaps)

- **STM32F469-DISCO and STM32F746-DISCO are currently physically off
  the bench USB bus** (both the Cherry-emulation device and the
  ST-LINK are absent — nightly `bench/status` FAIL rows since
  2026-10-21, `lsusb` confirms). All STM32 rows above carry their last
  on-bench stamps; nothing STM32 could be re-verified in the
  2026-10-21 session. Re-attach → one `hil-bench` nightly refreshes
  the generated page.
- M5Atom, PN532 (SPI), F746 bit-bang on-USB: build-green only (see matrix).
- No software ISO-DEP card simulator for the nucula RF legs (#261).
- CardMan 3121 / ACR1252 reference readers: present and enumerated
  (nightly PASS rows); used as oracles, not DUTs.

## Known issues (open, ranked by user impact)

1. **wifi-vpcd APDU pairing** (#326, PARTIAL): firmware half merged
   (#360 — stack overflow + per-probe power_on churn + ATR-cache
   correctness); host libifdvpcd patch riding #359; remaining wire
   wrinkle: a wire-correct APDU is answered with the staged ATR in
   ~3 ms (debug-build discrimination pending).
2. **F746 Escape 0xD0 fabricated zeros** (#344): no `diagnostics`
   override on the bit-bang wrapper; PR in flight; hardware verify
   blocked on board re-attach.
3. **Flash discipline** (#322): three partition-layout families + two
   bootloader generations in circulation; bundle-spec design merged
   via #351 (offsets per chip family, mandatory bootloader, registry
   authority); implementation tracked as task lab-flash-via-labgrid.
   Interim: `esp32_flash.py` only, full-4MB backup before any flash.
4. **Nucula pcscd retirement under direct CDC opens** (C3 silicon
   constraint, #117/#125): broker path is the workaround; POWER_OFF
   shim still active.
5. **BLE C3 advertising 0x0c rejection** (#66): GATT chain completes,
   advertising blocked by controller; NimBLE evaluation pending.
6. Review-debt backlog burning down: #332 umbrella (most P1s fixed in
   #349/#350/#352/#361 wave), #335/#336/#337/#339/#340/#343 — FIXED by
   PRs #361/#362 (merged 2026-10-21); #344 F746 diagnostics override
   merged (#364) pending bench verification on re-attach.

## Process state (2026-10-21)

- **CI policy (#366/#367, owner decision)**: PR CI is OPT-IN via the
  `ci-run` label; every merge verifies on the push-to-main leg;
  schedule/dispatch unchanged. Branch protection no longer requires
  per-PR ci-ok.
- **Merge gate (owner amendment 2026-10-21)**: the Codex-bot head
  review remains the norm; when the bot is silent after reasonable
  latency, a substantive SELF-review (diff read + tests run) satisfies
  the gate. Unaddressed bot findings still block.

## Card & bench safety (always current)

See AGENTS.md "Card-Handling Doctrine" — never SC-class commands in
loops; the J3R180 CPLC identity ledger is REQUIRED before any
fixed-keyset GP auth (enforced by
`tests/hardware/nfc/j3r180_gpg_lifecycle.py`, PR #341); retry budgets
are tracked on #127.

## Session verification log (2026-10-21, main @ dd948466)

- m5stick serial image built from main@dd948466
  (sha256 `200c9fe1…8e70eec`), flashed via `esp32_flash.py`
  (PT@0x8000 + app@0x40000 + otadata erase), `--boot-check wire`
  **PASSED** (GetSlotStatus OK). Full-4MB pre-flash backup taken
  (sha256 `7fb8b178…91728b037`).
- `gempc_smoke --uptime --poweron`: GetSlotStatus OK; IccPowerOn ATR
  `3B 85 81 31 FE 45 80 73 C8 21 10 84` (J3R180 on coil); uptime
  advancing; card_present=True.
- `gempc_t1 --probe` (PR #347 harness): GetSlotStatus → IccPowerOn →
  SetParameters(T=1) accepted → S(IFS) IFSC=254 → SELECT `6A82`
  (correct: applet absent at baseline) → GET DATA CPLC `9000` +
  42-byte 9F7F. All LRC/EDC verified.
- pcscd reconnect battery (libccidtwin): **5/5 cycles** each
  connect → ATR + SELECT ISD `9000` + CPLC `9000` (#345 class).
- `j3r180_gpg_lifecycle --status-only` with the CPLC ledger:
  preflight PASS (ledger `✓`, org-keyset single auth), status-only
  read-only contracts (no VERIFY, no CAP), correct nonzero exit on
  the applet-absent finding.
- Conformance battery (framing fuzz / GetSlotStatus soak / pcscd
  restart resilience): see the generated STATUS.md for the latest
  stamp; results from this session's run are recorded on PR #347.

## Session verification log (2026-10-21, second pass — current main @ f2efdb97)

- **m5stick serial CCID**: image built from main@f2efdb97 (sha256
  `3261e0c8…9fe6ec7`), flashed (PT@0x8000 + app@0x40000 + otadata
  erase), `--boot-check wire` PASSED; gempc_smoke green (GetSlotStatus
  bmICCStatus=1, ATR `3B 85 81 31 FE 45…`, uptime advancing, card
  coupled); T=1 wire session green (SetParameters → S(IFS) IFSC=254 →
  SELECT `6A82` correct-at-baseline → CPLC `9000` + 42-byte 9F7F);
  pcscd 5/5 reconnect cycles each ATR + ISD SELECT `9000` + CPLC `9000`.
- **nucula-c3 c3-ccid**: image built from main@f2efdb97 (sha256
  `6afce78d…5715e5`), flashed with broker quiesce (RTS ladder, boot
  check PASSED, broker restarted); broker reader `Nucula CCID via
  broker 01 00` serving — 3/3 sessions ATR + ISD SELECT `9000` + CPLC
  `9000` (card 5086…1320 on coil).
- **Cross-frontend differential at current main**: both boards return
  the byte-identical ATR `3B 85 81 31 FE 45 80 73 C8 21 10 84` for
  their J3R180s (MFRC522 vs PN7160) and clean CPLC values.
- **Bench end state**: both boards run current-main product images
  (m5stick serial GemPC reader + nucula c3-ccid broker reader in
  pcscd). The wifi-vpcd experimental image remains cached
  (sha256 `48936445…04ace739`) + full-4MB m5stick backup
  (sha256 `7fb8b178…91728b037`) for the #326 lane.
- Post-merge Codex sweep: 0 late findings across all 20 merged PRs.

## Session verification log (2026-10-21, third pass — lifecycles + perf)

- **Nucula broker FULL GP LIFECYCLE @ f2efdb97**: EXIT 0 — install
  (107 s, --bs 200, CAP sha 9213b1d6…), SELECT 9000, VERIFY PW1 9000,
  GET DATA 5E 9000, uninstall, baseline restore (5 pkgs). gpg SCD leg
  DEGRADED (known host scdaemon-wedge class, not a device defect).
- **m5stick regression #372**: same command at current main →
  `LOAD failed: 0x6400` ×3; PRE-re-pin (e9173657-era) image on the same
  board/card → install GREEN. A/B isolated to the #348 dep window;
  single APDUs unaffected. Pre-re-pin image cached on the bench as the
  known-good for CAP-install work until the bisect lands.
- **Perf (n=200, both readers live)**: m5stick p50 42.0 ms; nucula
  broker p50 400.0 ms / connect 1196.6 ms (C3 CDC floor class); ACR1252
  oracle p50 8.8 ms.
