# ST reference material (quote-only) — provenance

Vendored under the class-1 quote-only policy (AGENTS.md → "Vendoring policy
(three classes)"): never compiled, never linked; tracked so every ST-derived
wire/register decision can be verified against the upstream text it came from.

## stm32f4xx-hal-driver (issue #135)

| Item | Value |
|---|---|
| Upstream | https://github.com/STMicroelectronics/stm32f4xx-hal-driver |
| Pin (commit) | `1f6451c3e07728b4c830744de380e56bf5bc0026` (master, 2026-06-05) |
| License | Apache-2.0 (ST BSD-3-Clause→Apache-2.0 relicensed tree; file headers kept) |
| Vendored files | `stm32f4xx-hal-driver/Inc/stm32f4xx_hal_smartcard.h` (full, 755 lines) · `stm32f4xx-hal-driver/Src/stm32f4xx_hal_smartcard.c` (TRIMMED: init docs + `HAL_SMARTCARD_Init` + IRQ error flags + `SMARTCARD_SetConfig`) |

What we quote from it (specquotes markers `ST_HAL` / `ST_HAL_C`):

- `IS_SMARTCARD_STOPBITS` — smartcard mode supports **only 0.5 or 1.5 stop
  bits**; the founding evidence of the #135 T=1 STOP=1.0 violation fix.
- `SMARTCARD_STOPBITS_0_5` / `SMARTCARD_STOPBITS_1_5` — the CR2 STOP[13:12]
  encodings (0.5 = `01`, 1.5 = `11`).
- `SMARTCARD_NACK_ENABLE` / `SMARTCARD_NACK_DISABLE` + `IS_SMARTCARD_NACK_STATE`
  — the NACKEN contract (T=0 error signalling vs T=1 transparent relay).
- `SMARTCARD_InitTypeDef.GuardTime` doc: "Guard Time value in terms of number
  of baud clocks", programmed `GuardTime << 8` into `USART_GTPR` (prescaler
  in the low byte) by `HAL_SMARTCARD_Init`.
- The "Recommended SmartCard interface configuration to get the Answer to
  Reset from the Card" block (9 bits, 1.5 stop, even parity) and the
  0.5-for-receive caveat ("recommended to use 1.5 stop bits for both
  transmitting and receiving to avoid switching between the two
  configurations").
- `IS_SMARTCARD_BAUDRATE` + `SMARTCARD_BRR` — the BRR mantissa/fraction math
  and the 10.5 MBaud ceiling for the ETU audit (issue #122 checklist item 1).
- The IRQ handler's PE/FE/NE/ORE classification — the RX error contract the
  #136 error-map mirrors on the CCID surface.

Not vendored (checked, not needed): `Inc/stm32f4xx_hal_usart.h` carries no
smartcard-mode content beyond what `stm32f4xx_hal_smartcard.h` already pins.

## RM0090 §26.3.9 — NOT YET VENDORED (owner-gated)

The issue also asks for verbatim RM0090 §26.3.9 (smartcard mode) quotes in
`docs/quote-sources/rm0090-smartcard.md`. st.com is unreachable from this
working environment (2026-10-28) and archive.org holds no snapshot of the
PDF, so the verbatim transcription could not be produced in this lane.
Same flow as the UM11495 precedent (owner supplies the PDF; transcription is
committed, the PDF is not). Until then the HAL header/c file above — which
the issue itself lists as sources and which explicitly point at RM0090 —
carry the citations, and the one number the RM formula must confirm (the
GTPR GT value that expresses ISO guard time N) is locked by unit test with
the conservative choice documented in `smartcard_common.rs`.

## stm32-mw-usb-device (issue #138)

| Item | Value |
|---|---|
| Upstream | https://github.com/STMicroelectronics/stm32-mw-usb-device |
| Pin (commit) | see `stm32-mw-usb-device/PROVENANCE` block below |
| License | see file headers |
| Vendored files | `Class/CCID/Inc/*.h` (trimmed) |

What we quote from it (specquotes marker `ST_CCID`): the USB Device Library
CCID class template — descriptor field values, bulk endpoint/ZLP handling
and the T=0/T=1 smartcard-interface template state machine that the #138
audit table compares our implementation against.
