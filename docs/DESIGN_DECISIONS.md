# Design Decisions

This document records intentional deviations from the CCID specification, C reference
implementation (osmo-ccid-firmware), and libccid driver behavior that are **NOT bugs**
but deliberate choices based on hardware constraints or design trade-offs.

---

## DD-1: Inverse Convention (TS=0x3F) Not Supported

**Affects**: ATR parsing, ISO 7816-3 compliance
**References**: `src/smartcard.rs`, `C_REFERENCE_AUDIT_REPORT.md` §2.3

The STM32F469 USART2 in smartcard mode does not provide hardware-assisted byte inversion.
The C reference (SAMD54) uses the ARM `RBIT` instruction. On STM32, implementing software
byte inversion on every received character would require reconfiguring the USART to
single-byte mode with manual DMA, significantly impacting throughput and code complexity.

Since virtually all modern ISO 7816 cards use direct convention (TS=0x3B), this is an
acceptable hardware limitation.

---

## DD-2: Voltage Support Limited to 5V Only (bVoltageSupport=0x01)

**Affects**: CT30/K30 device profiles, `bVoltageSupport` descriptor field
**References**: `src/device_profile.rs`, `LIBCCID_DRIVER_AUDIT_REPORT.md` §3.2

The STM32F469-DISCO board with Specter DIY Shield Lite has a fixed 5V SIM slot voltage
regulator. The real CT30/K30 devices report 0x07 (5V/3V/1.8V) because they have
programmable voltage regulators (NCN8025 or similar).

Changing the descriptor to 0x07 without hardware support would be misleading — the
firmware correctly reports what it can actually do. If a different board with
programmable voltage is used, this should be updated in the device profile.

---

## DD-3: Abort Command Is a Stub (Always Returns Success)

**Affects**: `PC_to_RDR_Abort` (0x72) handling
**References**: `src/ccid.rs`, `CCID_SPEC_AUDIT_REPORT.md` §5.10

Single-slot synchronous reader with no concurrent command execution. The `cmd_busy`
flag prevents overlapping commands. There are no async operations to abort.

This matches the practical behavior of the reference implementation (which has a
broken Abort that always returns `CMD_NOT_SUPPORTED` due to a hardcoded `0` condition).

---

## DD-4: Direct ATR-Derived Parameters Instead of proposed_pars Pattern

**Affects**: `PC_to_RDR_SetParameters` (0x61), parameter negotiation flow
**References**: `src/ccid.rs`, `C_REFERENCE_AUDIT_REPORT.md` §3

The osmo-ccid-firmware uses a deferred-commit pattern (`proposed_pars`) where parameters
are only committed after successful PPS. Our approach derives parameters directly from
ATR and responds immediately, trading strict spec compliance for robustness — PPS
failure gracefully degrades to defaults rather than deactivating the card.

This is more resilient with real-world cards that have quirky PPS behavior.

---

## DD-5: Graceful PPS Failure (Degradation Instead of Deactivation)

**Affects**: PPS negotiation, card activation
**References**: `src/pps_fsm.rs`, `C_REFERENCE_AUDIT_REPORT.md` §2.4

The C reference treats PPS failure as fatal (card deactivation). We treat it as
non-fatal (use default Fi/Di). Many real-world SIM/smart cards have imperfect PPS
implementations but work fine at default baud rates. Graceful degradation provides
better user experience.

The CCID spec does not mandate either behavior — both approaches are valid.

---

## DD-6: Clock Frequency Parameter Ignored in SetDataRateAndClockFrequency

**Affects**: `PC_to_RDR_SetDataRateAndClockFrequency` (0x73)
**References**: `src/ccid.rs`, `src/smartcard.rs`, `C_REFERENCE_AUDIT_REPORT.md` §5

The USART clock source is APB1, which is set by the STM32 hardware configuration
(not programmable via CCID). The response returns the actual hardware clock value
so the host can compute correct baud rates. The data rate parameter IS applied
(BRR register adjustment).

---

## DD-7: Escape/T0APDU/Mechanical Return CMD_NOT_SUPPORTED

**Affects**: `PC_to_RDR_Escape` (0x6B), `PC_to_RDR_T0APDU` (0x6A), `PC_to_RDR_Mechanical` (0x71)
**References**: `src/ccid.rs`, `CCID_SPEC_AUDIT_REPORT.md` §5.12-5.14

- **Escape** is vendor-specific with no standard behavior. Gemalto profiles handle
  escape 0x6A (firmware features query) specifically; all other escape codes return
  `CMD_NOT_SUPPORTED`.
- **T0APDU** is redundant with `XfrBlock` at Short APDU level.
- **Mechanical** requires hardware not present.

All three match osmo-ccid-firmware behavior.

---

## DD-8: HardwareError Interrupt (RDR_to_PC_HardwareError 0x51) Not Implemented

**Affects**: Interrupt IN endpoint messages
**References**: `src/ccid.rs`, `CCID_SPEC_AUDIT_REPORT.md` §9.2

No hardware fault detection sensors on the current board. Would require additional
monitoring circuitry (e.g., voltage monitoring, temperature sensing, short-circuit
detection). This is an N/A condition for the current hardware platform.
