# CCID Reader — Project Report

This document is the consolidated report for the ccid-reader firmware: purpose, architecture, standards compliance, relation to other projects, and what “done” and “will it work” mean.

---

## 1. Purpose and scope

The **ccid-reader** project provides firmware that turns an STM32F469 board with a smartcard slot into a **USB CCID** smartcard reader usable by **pcscd** and **libccid** on the host.

- **Scope:** Single physical slot, **Short APDU** exchange only (no extended APDU), **T=0** and **T=1** protocols. The firmware performs ATR handling, PPS, IFSD negotiation, and T=0/T=1 protocol so the host sends and receives raw APDUs via XfrBlock.
- **Goal:** Behave like a “boring” generic reader: enumerate as CCID, respond correctly to PowerOn, GetSlotStatus, GetParameters/SetParameters/ResetParameters, XfrBlock, PowerOff, and NotifySlotChange, so that standard PC/SC tools and libraries work without reader-specific quirks.

---

## 2. Architecture

High-level data path:

- **Host** (e.g. `pcsc_scan`, `test_ccid_apdu.py`) talks to **pcscd**, which uses **libccid** to send CCID commands over USB.
- **USB device** (this firmware) exposes one CCID interface with Bulk IN, Bulk OUT, and Interrupt IN (NotifySlotChange).
- **CcidClass** (in `ccid.rs`) parses 10-byte headers, dispatches the 14 CCID commands, maintains slot state (Absent / PresentInactive / PresentActive) and **cmd_busy**, and sends responses in 64-byte chunks with ZLP when needed.
- **SmartcardDriver** trait abstracts the card side: **SmartcardWrapper** in `main.rs` adapts **SmartcardUart** to the CCID layer.
- **SmartcardUart** (`smartcard.rs`) drives USART2 in smartcard mode: power on/off, ATR read, PPS, IFSD negotiation, and **transmit_apdu** which dispatches to T=0 or T=1.
- **t1_engine** implements the T=1 block protocol (I/R/S blocks, LRC, chaining, WTX) and is used by `smartcard.rs` for T=1 cards.

```mermaid
flowchart LR
  Host[Host]
  pcscd[pcscd]
  libccid[libccid]
  USB[USB_Bulk_Int]
  CcidClass[CcidClass]
  Driver[SmartcardDriver]
  Uart[SmartcardUart]
  T1[t1_engine]
  Card[Card]
  Host --> pcscd
  pcscd --> libccid
  libccid --> USB
  USB --> CcidClass
  CcidClass --> Driver
  Driver --> Uart
  Uart --> T1
  Uart --> Card
  T1 --> Card
```

- **Single slot:** One physical slot; slot index is always 0.
- **Synchronous handling:** One CCID command at a time; **cmd_busy** is set when a command is accepted and cleared only after the full response (including ZLP) has been sent.
- **No heap:** `no_std`, fixed buffers (e.g. 271 bytes for CCID messages).

---

## 3. Standards compliance

The firmware is aligned with **USB CCID Rev 1.1** and **ISO 7816-3** (T=0 and T=1) for the implemented subset. Evidence and field-by-field checks are in [ALIGNMENT.md](ALIGNMENT.md) and [VERIFICATION.md](VERIFICATION.md).

- **CCID:** Class descriptor (e.g. dwFeatures 0x000200B2 including required 0x02 for APDU level), 10-byte message header, correct response type per command (DataBlock / SlotStatus / Parameters / Escape), bSeq echo, NotifySlotChange on the interrupt endpoint.
- **libccid:** XfrBlock APDU at offset 10, GetParameters/SetParameters layout and bSeq matching.
- **ISO 7816-3:** ATR parsing, PPS, T=0 procedure bytes (NULL, INS, ~INS, 0x61 XX, 0x6C XX), T=1 I/R/S blocks, LRC, chaining, S-block WTX (0xC3/0xCB), IFSD negotiation.

**Known limitations (see VERIFICATION.md §6):**

- **Extended APDU** is rejected (payload &gt; 261 bytes); acceptable for the Short APDU path.
- **SetDataRateAndClockFrequency** and some other optional commands return CMD_NOT_SUPPORTED; acceptable for a simple reader.
- **Voltage/clock:** Descriptor advertises support; we do not change voltage or clock dynamically per host request.
- **SetParameters / ResetParameters:** We respond with current ATR-derived parameters but do **not** apply host-supplied parameters to the next exchange (we use ATR-derived params only). See [COMPARISON.md](COMPARISON.md) for the contrast with osmo.

---

## 4. Relation to other projects

- **osmo-ccid-firmware:** Reader-side reference. We reuse message types, error codes, command→response-type rules (gen_err_resp), and 3-state slot semantics. We differ: single slot, synchronous XfrBlock, and we do not apply host SetParameters/ResetParameters to runtime. See [COMPARISON.md](COMPARISON.md) for full comparison.
- **libccid (CCID/):** Host driver. We implement the device side of the protocol it expects (descriptor, message layout, bSeq). No code shared; used as specification for host behaviour.
- **canokey-stm32:** Token (embedded “card”); CCID is one USB interface. We are a reader with a real slot and physical T=0/T=1; no shared behaviour beyond “CCID interface on USB.”
- **GoKey:** Token in Go; CCID delegates to internal ICC. We are a reader with real ISO 7816; comparison is architectural only.

**See [COMPARISON.md](COMPARISON.md) for the full comparison table and per-project notes.**

---

## 5. What we learned from others

- **osmo-ccid-firmware:** CCID message type and error code constants; the rule that each command must get the correct response type on error (DataBlock vs SlotStatus vs Parameters vs Escape); 3-state slot (NO_ICC / PRES_INACT / PRES_ACT) and cmd_busy semantics; parameter encode/decode patterns for Get/SetParameters (T=0 vs T=1).
- **libccid:** XfrBlock layout (APDU at offset 10); bSeq handling; SetParameters request layout (bProtocolNum at byte 7, protocol data at 10+).
- **Specs and references:** USB CCID Rev 1.1 (descriptor, message formats); ISO 7816-3 (T=0 procedure bytes, T=1 block format, WTX S-block); blog.apdu.fr / ccid.apdu.fr (dwFeatures, NotifySlotChange).

We intentionally do **not** apply host SetParameters or ResetParameters to runtime (we use ATR-derived params only); ResetParameters returns current params. See COMPARISON.md “Learnings and differences” for the contrast with osmo’s proposed_pars/default_pars.

---

## 6. Will it work?

**Definition of “done”:** Spec-aligned implementation plus successful hardware validation (enumeration, pcscd recognition, ATR, at least one APDU, hot-removal).

**Steps to validate:**

1. **Build** the firmware: `cargo build --release --target thumbv7em-none-eabihf`.
2. **Flash** to the STM32F469 (e.g. `probe-rs run` or `st-flash`).
3. **Host:** Use standard class-compliant CCID stack behavior with the emulated `08E6:3437` reader identity already present in common libccid lists.
4. **Run the hardware test steps** in [VERIFICATION.md](VERIFICATION.md) §5:
   - `lsusb -v -d 08E6:3437` (descriptor, three endpoints).
   - `pcsc_scan` with card inserted (reader name `Gemalto PC Twin Reader`/IDBridge-compatible alias, ATR starts with `3B`).
   - `python3 test_ccid_apdu.py` (or with reader name) for connect, ATR, one APDU, SW check.
   - Card hot-removal and re-insert; confirm NotifySlotChange behaviour if possible.

**Conclusion:** The implementation is compliant with the specs we claim; “will it work” is confirmed only by running these steps on the target host with the real device.

---

## 7. Possible future improvements

- **README:** Done; see [README.md](README.md).
- **Optional tests:** Descriptor dump test; mock `SmartcardDriver` and `T1Transport` to exercise CCID and T=0/T=1 without hardware (see VERIFICATION.md §7).
- **Parameters:** Optionally apply host SetParameters (e.g. Fi/Di, WI, IFSC) to the next exchange, or implement “reset” as power-cycle and re-read ATR, to align more closely with osmo semantics if needed.
- **Extended APDU:** Only if required later; would need TPDU or extended-APDU handling and possibly host-side support.
- **Debug:** Reduce or gate verbose defmt in hot paths for production builds.
