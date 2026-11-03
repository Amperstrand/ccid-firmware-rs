# The Amperstrand NFC lab — canonical description and workflow

This is the authoritative description of the NFC laboratory: what
exists, where it lives, how it fits together, and the intended
workflow. Point-in-time physical state lives in generated snapshots
(`fixture_selfcheck.py`, SEATS.md) — this file describes the LAB, not
today's card placement.

Related: `docs/NFC-LAB-STATE.md` (2026-11-03 session snapshot),
`docs/LIVE-CARD-RELAY-DESIGN.md` (relay internals), issue #529
(evidence trail), AGENTS.md (doctrine).

## Inventory

Two bench hosts, one exclusive-ownership coordinator:

| Host | Role | NFC-relevant hardware |
|---|---|---|
| ai-legion (.208) | coordinator, build host | M5Stick+MFRC522 reader (USB `/dev/ttyUSB2`), CardMan 3121 (contact reference), the labgrid coordinator + exporters |
| ai-legion-small (.221) | RF bench | ACR1252 (reference oracle, system pcscd), nucula ESP32-C3+PN7160 (USB-JTAG + wifi-vpcd), Samsung S20 phone (adb `RF8NB01ZYAH`) |

Phones: exactly TWO exist. The S20 (SM-G780F) is the proven HCE
endpoint. Phone B (moto g, `ZY326DPC7R`) is intermittently connected —
its NFC capability is UNVERIFIED until it is on USB.

Cards (see the #127 ledger — CPLC serials, key state, budgets):

| Card | Identity | Role |
|---|---|---|
| Real NTAG 424 DNA | UID `04c474fa967380` (provisioned boltcard, SDM off) | primary read-only Type-4 target |
| JCOP/P71 "keyed" | CPLC `50942720` | the working-AUTH-triple card — owner-gated, never used autonomously |
| JCOP/P71 "locked" | CPLC `50947320` | ISD permanently locked (incident #144) — applet fixture only |
| JCOP/P71 | CPLC `50979692` | third ledger card, keys unknown |
| Fake-NTAG424 applet card | JCOP + NDEF applet mimicking NTAG424 | zero-risk conformance target |

## Reader endpoints (the map that was paid for)

| Reader | Host | pcscd access | Notes |
|---|---|---|---|
| M5Stick GemPC | .208 | `PCSCLITE_CSOCK_NAME=/run/pcscd-lane-m5stick.comm` (pcscd-lane@ unit, USB-filtered) | serial GemPC framing; raw path via repo `T1Session` when pcscd must be quiesced |
| ACR1252 PICC | small | system pcscd (ssh `pyscard` snippets) | the oracle; needle `ACR1252 Dual Reader PICC` (SAM is a separate reader — needles must be unambiguous) |
| Nucula VPCD | small | `PCSCLITE_CSOCK_NAME=/run/pcscd-lane-nucula.comm` | wifi-vpcd image; port 35963 is baked at BUILD time (#555) and must match the lane conf |
| (stale) "Nucula VPCD wifi67" | small | system pcscd | DEAD entry from pre-lane days on ports 35967/68 — do not probe it; removal pending an owner restart window |

Hard-won rules encoded here so they are never relearned:
- A lane pcscd with live TCP listeners can still be API-dead (the #554
  zombie) — health = a reader list + connect through THE SOCKET, never
  `ss` alone.
- Kill lane daemons by PID from `ss`, never `pkill -f` from a wrapper
  whose own cmdline matches.
- The S20 answers unregistered-AID SELECTs itself with 6999; the
  m5stick/marginal-card "protocol mismatch" is card-class-specific.

## The relay (the lab's centerpiece)

```
test client (pcscd anywhere)
   → reader DUT (m5stick / nucula / ACR)
   → ISO-DEP RF
   → S20 HCE (com.amperstrand.benchrelay — RF modem)
   → adb forward tcp:4871
   → host brain (host-tools/hce_relay.py)
   → card backend: scripted table | static SW | PC/SC real card
```

The brain enforces the session contract (one socket ⇄ one backend
session, explicit errors, no re-pairing, JSONL evidence journal with
redaction-by-default) and — since the safety hardening — a default-deny
APDU policy at the physical-card boundary (`--pcsc-policy` +
`--expect-id`): NTAG424 by verified UID, JCOP by verified CPLC ledger
serial. Forbidden commands die before transmission.

Proven (hardware, journaled; see #529): full-chain reads through the
real NTAG424 at 32–94 ms reader-visible latency; a forbidden D6 write
denied at the boundary with the card never seeing it.

### Capability taxonomy (keep verdicts separate — they are DIFFERENT claims)

| Capability | What it means | Status on this bench |
|---|---|---|
| Transparent APDU relay | bytes in = bytes out across the chain, no semantics touched | PASS for non-SELECT APDUs after HCE bind (single-AID tunnel — #558) |
| AID-routed application relay | the phone dispatches per-SELECT by AID; only registered+dispatched AIDs bind | PASS for `D2760000850101` (NDEF) on the S20; other AIDs get phone-6999 (#558) |
| Protocol-aware translation | re-framing/re-encoding between layers (e.g., card-side applet pre-selection to work around AID tunneling) | DESIGNED (backend pre-select), not yet a tested feature |
| Synthetic card simulation | HCE applet answers without any real card (script backend) | PASS (static/script backends, RF-proven) |

A successful NDEF relay therefore does NOT prove transparent J3R180
applet compatibility — applet SELECTs cannot traverse the S20 (see
#558); use backend-side pre-selection or a non-Samsung HCE endpoint.

## Intended workflow

1. **Trust the bench by probing it, not by memory.** Run
   `python3 tests/hardware/nfc/fixture_selfcheck.py` (report mode;
   `--gate` for unattended gating). It diffs reality against the
   declared standing rig and reports drift (it has caught stale
   labgrid holds and the #554 fix within minutes of each).
2. **Own fixtures before touching them.** Acquire the labgrid places
   for every device in your RF path; a two-device fixture is ONE
   exclusivity domain. Flocks for pcscd/lane operations per AGENTS.
3. **Standing rig** (default layout): S20 on the m5stick coil (HCE
   frontend), primary card under test on the ACR (backend), nucula as
   the third reader. Cards not under test stay parked.
4. **Card carousel** (the only physical moves): one card under test at
   a time — ACR first (oracle baseline), then the m5stick coil (DUT
   side, S20 temporarily parked). Always on the coordinator's signal.
5. **Safety is a boundary, not a discipline.** Real-card backends run
   behind `--pcsc-policy`/`--expect-id`; expanding an allowlist is a
   reviewed code change with tests, never a runtime flag. When in
   doubt: synthetic backends.
6. **Evidence by default.** Every relay run carries `--journal` with
   `--meta` provenance; artifacts land in `.omo/evidence/…` and claims
   on issues link them. Status is generated (fixture_selfcheck,
   STATUS.md), never hand-written.
7. **Bring-up / teardown** for the phone leg: the RUNBOOK in
   `host-tools/hce-relay-app/RUNBOOK.md` (payment-default pivot,
   field-cycle after routing changes, teardown restores the baseline).

## What the lab is FOR (the program)

- **Relay reliability & differentials** (#87-class): same card, same
  battery, through every reader — bytes where comparable, semantics
  where activation differs, latency separately.
- **Conformance substrate**: the relay + fake-NTAG424 applet card +
  spec-locked vectors (greatspectations quote gate in CI; nero's
  NDEF/CCID test matrices as reference) — see
  `docs/research/2026-11-03-*.md`.
- **DUT hardening**: #444 sustained-session wedge, #472 time-extension
  behavior under delayed relay responses (the `--delay-ms` instrument),
  fuzz/differential vs the ACR oracle.
- **Second phone leg** (when Phone B exists): ReaderMode companion +
  android backend per `docs/LIVE-CARD-RELAY-DESIGN.md`.
