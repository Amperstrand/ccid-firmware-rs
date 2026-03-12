# CCID Reader — Consolidated research and findings

Single entry point for: **current situation**, **what we know**, **what we found**, and **what is still open**. No external research in this file; it consolidates existing project docs and run outcomes. For external research questions and how to report back, see [EXTERNAL_RESEARCH_REQUEST.md](EXTERNAL_RESEARCH_REQUEST.md).

---

## 1. Current situation (hardware and firmware)

- **Card in STM32 reader:** Seedkeeper (SatoChip) in our STM32F469 CCID reader; both USBs (device + debug) connected to remote host (e.g. 192.168.13.246).
- **Firmware state:** CLK asserted before RST (`set_clock(true)` and log); NACK off during ATR; USART error clearing before/during ATR; CR2 init **0x3C00** (CLKEN=1). Delays: 50 ms power-on, 20 ms post-RST; ATR first-byte timeout 400 ms, per-byte 1000 ms.
- **Defmt observation:** `PowerOn: card_present=true`, `CR2=0x3C00`, `ATR timeout SR=0x01C0` (FE=0 PE=0 ORE=0 NE=0) → **zero bytes** from card (no TS).
- **Host result:** Seedkeeper test fails with "Card is unpowered (0x80100067)".
- **Gemalto control:** On the same or another host, with pcscd running, a card in the Gemalto PC Twin Reader returns a full ATR (see [LEARNINGS.md](LEARNINGS.md) OMNIKEY control test). The card that was read was a different one (JTaxCoreV1/Fiji), not the Seedkeeper; Seedkeeper-in-Gemalto result is pending user run.

---

## 2. What we know (from specs and code)

- **CCID:** Descriptor and message layout; GetParameters/SetParameters format (bProtocolNum at byte 9, abProtocolData at 10+; dwLength = length of abProtocolData only). Protocol selection: PowerOn → ATR → ATR_GetDefaultProtocol (TD(i), TA2, default T=0) → PPS (if not auto-PPS) → SetParameters → XfrBlock.
- **NACK:** Only for T=0; must be off during ATR (ISO 7816-3; osmo-ccid-firmware pattern). ORE cleared by reading DR; no pin conflict between smartcard (PA2, PA4, PG10, PC2, PC5) and debug/USB.
- **Voltage:** Advertised in descriptor (bVoltageSupport) but not switched in firmware; hardware supplies slot VCC. Wrong voltage can damage card or prevent response.
- **NotifySlotChange:** Format and usage per CCID 6.3.1; we use interrupt endpoint.
- **SCARD_E_PROTO_MISMATCH / protocol:** Causes documented in PCSC winscard.c; our SetParameters layout matches CCID spec. Truncated or missing ATR leads to wrong protocol detection.
- **Pinout:** PA4 = USART2_CK (card CLK), PA2 = I/O, PG10 = RST, PC5 = PWR, PC2 = card detect; SWD on PA13/PA14; RTT/defmt uses RAM mailbox, no extra pins.
- **External research** ([EXTERNAL_RESEARCH_REQUEST.md](EXTERNAL_RESEARCH_REQUEST.md) §1, §6): PA4 (CLK) must be AF7 push-pull, PA2 (I/O) AF7 open-drain; SatoChip/Seedkeeper are 3 V only—verify slot VCC before use.

Sources: [RESEARCH_AGENDA.md](RESEARCH_AGENDA.md), [OPEN_RESEARCH_QUESTIONS.md](OPEN_RESEARCH_QUESTIONS.md), [DETAILED_RESEARCH_REPORT.md](DETAILED_RESEARCH_REPORT.md).

---

## 3. What we found (from runs and fixes)

- **Defmt showed CR2=0x3800 at PowerOn** → CLKEN (bit 11) was not set. **Fix:** Init CR2 to **0x3C00** and add log. After reflash, defmt confirms CR2=0x3C00 but **still no TS** (ATR timeout, SR=0x01C0, no FE/PE/ORE/NE). Conclusion: software CLK enable is correct; next suspect is hardware (PA4 not reaching card C3, or VCC/RST/I/O).
- **NACK disabled during ATR** and **clear_usart_errors()** added earlier; ATR still times out with zero bytes.
- **Activation checklist (C1–C7)** and **OMNIKEY control test** described in [DEFMT_DIAGNOSIS.md](DEFMT_DIAGNOSIS.md) and [LEARNINGS.md](LEARNINGS.md). Gemalto run (with pcscd running): reader present, card inserted, full ATR for the card that was in the reader (not Seedkeeper).
- **Card damage / overvoltage:** Possible if STM32 slot supplies 5 V and Seedkeeper is 3 V only; "card doesn't light up" in Gemalto could be pcscd not running or reader issue, or actual damage. Added to research as [RESEARCH_AGENDA.md](RESEARCH_AGENDA.md) §5.7.

---

## 4. What is still open (before external research)

- **Full ATR on device:** We never see more than zero bytes from the card in our reader. Need to confirm whether CLK is actually present on card C3 (PA4 routing, schematic or scope).
- **Voltage at slot:** Unknown; depends on board. Must confirm before using 3 V–only cards.
- **Gemalto control with Seedkeeper:** Pending: insert Seedkeeper in Gemalto, run `pcsc_scan`, record whether Seedkeeper ATR appears or not (to distinguish "pcscd/reader" vs "card damaged").
- **Seedkeeper in Gemalto first:** We must get unlock + read secret working in the Gemalto reader before relying on STM32. Current run: card in Gemalto has SeedKeeper applet (SELECT OK) but VERIFY_PIN returns **SW=9C20**; meaning of 9C20 and card identity (ATR "JTaxCoreV1") need research — see [RESEARCH_AGENDA.md](RESEARCH_AGENDA.md) §5.8 and [EXTERNAL_RESEARCH_REQUEST.md](EXTERNAL_RESEARCH_REQUEST.md) “Seedkeeper applet verification”.
- **Board match:** Whether our board matches a known schematic (e.g. Specter-DIY / F469-DISCO smartcard footprint) and pinout C1–C8.

---

## 5. References

| Document | Role |
|---------|------|
| [RESEARCH_AGENDA.md](RESEARCH_AGENDA.md) | Research items, answered/open, hypotheses, next steps |
| [OPEN_RESEARCH_QUESTIONS.md](OPEN_RESEARCH_QUESTIONS.md) | Original questions and answers (NACK, protocol, voltage, pins) |
| [DETAILED_RESEARCH_REPORT.md](DETAILED_RESEARCH_REPORT.md) | Pin analysis, ATR root cause, SetParameters, implementation guidance |
| [DEFMT_DIAGNOSIS.md](DEFMT_DIAGNOSIS.md) | Activation checklist (C1–C7), defmt capture summary, CLK-first run |
| [LEARNINGS.md](LEARNINGS.md) | M0 state, OMNIKEY control test result, M1/M2/M3 milestones |
| [EXTERNAL_RESEARCH_REQUEST.md](EXTERNAL_RESEARCH_REQUEST.md) | Questions for external research and how to report back |

This consolidated doc is the entry point; the others remain the detailed sources.
