<!-- ECOSYSTEM_SCAN.md — verified 2026-10-21 by three parallel research
     agents (liveness checked via GitHub/Codeberg/crates.io/kernel.org APIs
     on that date). Per-item activity claims are as-of stamps; re-verify
     before committing to a dependency. -->

# Ecosystem scan — fork, join, or learn

What exists around us, what's worth joining, forking, adopting, or just
studying — and the confirmed gap we occupy. Companion to
`docs/TRANSPORT_HOST_DRIVERS.md` (host-transport layer) and
`docs/SHARED-CRATES.md` (our own crate graph).

## The headline

1. **No Rust smart-card READER firmware exists besides ours.** The
   trussed-dev stack (Nitrokey 3 / Solo 2 / RS-Key) is *token* firmware
   (card-emulating keys), not readers. Our ccid-core/transport-serial
   platform is a genuine differentiation — and the trussed ecosystem is
   the natural alliance for the USB layer.
2. **Ledger's `lib_ccid` is the fork prize**: Apache-2.0, actively
   maintained, device-side CCID in C on STM32, with a clean
   cmd/transport split. Zero license friction to adapt.
3. **The #261 software card has a three-tier answer** (below), each
   tier with an existing verified building block.

## Join (co-maintain — these upstreams are fragile and worth owning)

| Project | What | Why join |
|---|---|---|
| [trussed-dev/usbd-ccid](https://github.com/trussed-dev/usbd-ccid) (+ `iso7816`, `apdu-dispatch`) | THE Rust usb-device CCID class; powers Nitrokey 3 & Solo 2; v0.5.0 2026-08, Apache-2.0/MIT | 2-star fragility under production firmware; our STM32 USB-CCID descriptor conformance lessons belong upstream. Consume + become co-maintainer. |
| [scll](https://github.com/ievkh/scll-rust) | Rust GlobalPlatform lifecycle lib: SCP02/03, CAP load/install, verified against real JCOP P71; pluggable Transport trait; v1.0.5 2026-09, Apache-2.0 | The only real Rust `gp`-tool alternative. Contribute a reader-transport adapter so scll drives OUR readers; candidate to back our Python lifecycle harness with a Rust one. Bus-factor: single maintainer. |
| [pc-usbip-runner](https://github.com/trussed-dev/pc-usbip-runner) | CCID-over-USB/IP CI runner (README: "CCID often unstable") | Owning that stability fix = our firmware-in-CI story + upstream goodwill. Apache-2.0. |
| [Nitrokey/vpicc-rs](https://github.com/Nitrokey/vpicc-rs) | Rust vpicc (vsmartcard card side), MIT, stale-but-tiny | Fork to add scripted table cards for our vpcd CI lane. |

## Fork / adapt (permissive, verified alive)

| Project | What | Adaptation |
|---|---|---|
| [Ledger lib_ccid](https://github.com/ledgerhq/ledger-secure-sdk) (in ledger-secure-sdk) | Device-side CCID in C on STM32, Apache-2.0, lib_ccid last touched 2025-09 (crash fix) — active | Cross-check/port semantics into ccid-core; its cmd/transport split is the multi-transport endpoint pattern; descriptor choices vs our Cherry ST-2xxx profile. |
| [okanatas/NFCCardEmulator](https://github.com/okanatas/NFCCardEmulator) | MIT Android HCE test app (2024-dormant, small) | Fork as the S20 fixed-APDU-table responder — #261 tier (b). |
| [NXPNFCProject libnfc-nci](https://github.com/NXPNFCProject/NFC_NCIHAL_libnfc-nci) | NXP's NCI stack with working listen-mode CE paths, Apache-2.0 | Mine the NCI listen + CE callback path into our pn7160-nci for #261 tier (c); blueprint = NXP AN13861 Scenario 1 (DH-NFCEE: host IS the card, chip does RF). |
| [libnfc nfc-emulate-forum-tag4](https://github.com/nfc-tools/libnfc) | Minimal complete ISO-DEP T=CL responder (LGPL-3.0) | Reference for the PICC responder state machine (RATS/ATS, S/D/I blocks, WTX). |

## Adopt as infrastructure (not code — test rigs & oracles)

| Thing | Role |
|---|---|
| [vsmartcard](https://github.com/frankmorgner/vsmartcard) USB CCID Emulator (GadgetFS) + PC/SC Relay | CI conformance rig: descriptor + endpoint behavior without our firmware; scripted card simulation with vpcd. Active (2026-10). |
| [Rousseau's CCID driver](https://github.com/LudovicRousseau/CCID) verbose logs | Free nightly oracle: strictest real descriptor parser + per-exchange T=1 logging. Parse its logs as assertions. |
| [GlobalPlatformPro](https://github.com/martinpaljak/GlobalPlatformPro) | Already our `gp` tool — treat its traces as the expected-reply corpus for mock cards. |
| [jcardsim](https://github.com/licel/jcardsim) behind vpcd | Zero-hardware virtual J3R180 (GP semantics, Apache-2.0); OpenSC wiki holds the bridge recipe. |
| [Chameleon Ultra](https://github.com/RfidResearchGroup/ChameleonUltra) (`nfc_14a_4.c`) | Programmable "perfect card" bench instrument for WTX/IFS/chaining edge cases. Hardware purchase — owner decision. |
| Gnuk / YubiKey 5 (OpenPGP) | Fixed cast of hostile real cards for the conformance matrix. |

## Learn only (architecture & behavior references)

- **Flipper Zero `lib/nfc`**: poller/listener/scanner per-protocol plugin
  architecture; `iso14443_4_layer.c` shows the testable T=CL boundary.
  (No project "loxjie" exists — their stack is in-house.)
- **Linux `net/nfc`**: cleanest NCI core state machine +
  `digital_dep.c` as an independent ISO-DEP initiator to diff against.
- **AOSP NFC mainline module**: NCI core + the de-facto HCE semantics.
- **Proxmark3 (Iceman)**: ISO14443 timing/anticollision depth; Lua
  hardware-test-script pattern; APDU/SW decode tables. GPL-3.
- **Nitrokey/se05x**: one of two real Rust T=1s — reference only
  (LGPL-3.0, incompatible with static firmware linking).
- **embassy-usb CCID host class** (merged 2026-08): a Rust CCID *host*
  for cross-checking our device side without pcscd.

## Corrections to earlier beliefs (verified)

- `card-filesystem` crate: does not exist. "Chameleon32": does not
  exist (family: ChameleonMini → Ultra/SE; Mini is dead/custom-licensed).
- GlobalPlatform publishes **no public TADP/mock-SE** — compliance
  suites stay certification-gated. Their OSS org hosts reference
  implementations only.
- AOSP native NFC lives in `packages/modules/Nfc` (moved Dec 2024);
  `external/libnfc-nci` and `system/nfc` are frozen.

## #261 decision (software ISO-DEP card) — three tiers, all grounded

1. **Now, zero hardware (GP/APDU level)**: scripted card over vpcd —
   jcardsim mock-GP applet behind vpcd (OpenSC wiki recipe), or a
   plain responder speaking the documented vpcd wire protocol. Covers
   everything except RF specifics.
2. **RF level, cheap (S20)**: fork NFCCardEmulator into a fixed-table
   AID service (~100 lines); use NFCGate to capture golden J3R180
   traces for the table.
3. **RF level, our silicon (endgame)**: PN7160 listen-mode CE on a
   second nucula per NXP AN13861 Scenario 1, mining NXP's Apache-2.0
   NCI HAL for the listen/CE callbacks. ESPHome's PN7160 component
   proves ESP32+PN7160 CE works (NDEF-shaped; extend for raw APDUs).

Recommended sequence: tier 1 lands in CI first (biggest test-coverage
gain per effort), tier 3 rides the #380 vpcd work, tier 2 only if
tier 3 stalls.

## What this changes for us

- Our Rust CCID kernel stays ours — and we ally with trussed on USB.
- The iso14443 fork continues its patch-queue → upstream trajectory
  (crates.io-published, active).
- #261 has a concrete, evidence-backed plan instead of an open design
  question.
- Licensing guardrails recorded: GPL-3 items are learn-only
  (Proxmark3, Flipper, Chameleon, vsmartcard code); Apache/MIT items
  are fork/adapt-safe (Ledger lib_ccid, scll, NFCCardEmulator, NXP
  NCI HAL, jcardsim); LGPL items are read-only (se05x, libnfc,
  Rousseau CCID).
