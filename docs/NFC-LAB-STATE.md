# NFC lab state — 2026-11-03 kickoff session

Status vocabulary: `PASS` (demonstrated on named fixture), `BLOCKED`,
`SKIP`, `UNVERIFIED`, `UNSUPPORTED`. Every row below is either probed
this session (evidence linked) or cites its bench source. This file is
a session snapshot — the live truth for cross-project state is
`/root/bench/SEATS.md` + the labgrid coordinator, not this document.

## Phones (exactly two exist — never assume more)

| Identity | Model | Host / transport | NFC | Role | State |
|---|---|---|---|---|---|
| `RF8NB01ZYAH` ("S20", SM-G780F / r8s) | Samsung | ai-legion-small, `adb -s RF8NB01ZYAH` over ssh; labgrid place `phone-s20` | ON; HCE with `com.amperstrand.benchrelay` + both boltcard HCE apps installed | Phone A (HCE virtual card) | adb-authorized, screen ON_UNLOCKED; **physically coupled to the nucula coil on small** (PASS, evidence below) |
| `ZY326DPC7R` (moto g, Android 15) | Motorola | labgrid place `numo-phone` (claims ai-legion USB 18d1:4ee7) | UNVERIFIED | Phone B (ReaderMode backend — M3) | **NOT on either host's USB** (adb empty on .208 and small; no udev events since 2026-11-01). Location needs owner hands. BLOCKED |

The 2026-10-31 "S20 USB enable-loop" note in AGENTS STATE 2026-11-02 is
stale: the S20 enumerates and adb-answers on ai-legion-small.

## Readers / DUTs

| Device | Physical location | Access path | Card in field | State |
|---|---|---|---|---|
| nucula C3 + PN7160 (`90:DA:72:9A:50:18`) | ai-legion-small | wifi-vpcd image → `Nucula VPCD wifi67 00 00/01` on small's pcscd; USB-JTAG by-id for esptool; labgrid `nucula-c3` + OTA seat `ccid-nucula` | S20 HCE (coupling PASS) | **vpcd presence gap**: slots report "no card" to pcscd despite coupling, also after hard-reset reboot (see findings) |
| m5stick + MFRC522 (Hades2001 `49D6163EBE`) | **ai-legion (.208)** — the "on small since 11-01" note is stale | local `/dev/ttyUSB2` (lane pcscd holds it); lane socket `/run/pcscd-lanes/m5stick.sock` | a T=1 tag (ATR `3B858131FE458073C8211084` per 11-01 note; identity UNVERIFIED this session) | pcscd connect = the tracked protocol-mismatch bug; raw GemPC path works per 11-01 |
| ACR1252 (ref oracle) | ai-legion-small | small's pcscd (`ACS ACR1252 ... PICC/SAM`); labgrid `ref-acr1252-small` | **empty** (probed: no card on coil) | the J3R180/NTAG424 are NOT here — their physical location is unknown, owner hands |
| CardMan 3121 | ai-legion (.208) | system pcscd (USB-only freeze) | — | untouched this session |

## Card fixtures

| Fixture | Where it actually is | Identity confidence |
|---|---|---|
| J3R180 JavaCard (P71D321 class per CPLC history) | **UNVERIFIED** — not on the ACR coil; #127 ledger cards were last sighted on nucula/m5stick/ACR coils 10-20..11-01; needs physical search | medium (bench history), none re-probed today |
| NTAG424 DNA | UNVERIFIED — no location data at all this session | none |

Default-deny APDU allowlist stands (AGENTS card-handling doctrine); no
card was powered this session except read-only presence probes that
returned absent.

## Session findings (all evidence: `.omo/evidence/529-relay/2026-11-03/`)

1. **PASS — phone↔host relay link**: brain (`host-tools/hce_relay.py`)
   connects through `adb forward tcp:4871`; phone hello received
   (`link.jsonl`).
2. **PASS — reader→phone→host dispatch**: a reader-originated boltcard
   SELECT (`00A4040007D276000085010100`, sha8 `a07a456f`) arrived at the
   brain over real RF with zero pcscd action from us (`m1.jsonl` first
   session, 14:00:09 local). One-shot; the originating reader was the
   nucula (its post-power-on activation, see next).
3. **PASS — S20↔nucula RF coupling**: with the brain attached, the
   phone's `onDeactivated` fired 4.3 s after an esptool hard-reset of
   the nucula — the PN7160 field came up, activated the S20's HCE, then
   the link dropped (`m1.jsonl` `deactivated` @ 1793712304.8).
4. **BLOCKED — pcscd through nucula wifi-vpcd**: both vpcd slots report
   "no card" (NoCardException ×4 with 15 s activation windows, plus
   60 s post-reboot poll) while the vpicc TCP link is ESTAB and the
   coupling is proven. Firmware-side presence reporting gap
   (#125/#551 class). NOT re-flashed this session.
5. **stale-bench-registry sweep**: m5stick physically back on .208;
   `/dev/m5stick-remote` bridge + `m5stick-serial-relay` on small dead;
   labgrid `phone-s20` USBDevice resource still matches a .208 udev
   identity that no longer exists (coordinator shows avail=True) —
   re-export or annotate at the next coordinator maintenance.

## RF geometry (as probed)

- One coupled pair live: S20 ↔ nucula coil (small). Keep one active
  reader field per pair; ACR1252 ~same bench, empty field.
- m5stick (.208) has its own coil + tag — independent RF zone, untouched.

## What unblocks what

| Blocked item | Needs |
|---|---|
| M1 full PASS (SELECT through pcscd → 9000 back) | nucula wifi-vpcd presence fix (firmware issue) **or** moving the S20 to the ACR1252 coil (hands) |
| M2 real-card relay (J3R180 → ACR → brain → S20 HCE) | J3R180 located + placed on ACR coil (hands) |
| M3 phone-as-reader (Phone B ReaderMode) | moto g located + USB + adb authorize (hands); the app-side work is designed in `LIVE-CARD-RELAY-DESIGN.md` |
