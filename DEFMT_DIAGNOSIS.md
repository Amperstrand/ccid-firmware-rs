# Remote defmt diagnosis — Seedkeeper PowerOn

**Date:** 2025-03-08  
**Setup:** Card in STM32 reader, both USBs to remote (192.168.13.246); probe-rs attach for defmt, Seedkeeper test to trigger PowerOn.

## Resolved / Success (2026-03-08)

After activation-layer and protocol-layer fixes, the STM32 reader now achieves **full ATR** and **full Seedkeeper flow**. Key fixes: USART (CR2/CR3, no SBK, CLKEN, RST sequence), ATR reception (tight busy-wait, no mid-ATR logging/delays), T=1 (1 stop bit, GT=1, IFSD negotiation), and **T=1 sequence number persistence** plus **I-block PCB detection** (`(pcb & 0x80) == 0`). defmt shows correct T=1 exchanges (alternating N(S) 0x00/0x40, LRC OK, XfrBlock OK). See [LEARNINGS.md](LEARNINGS.md) “STM32 CCID reader: full success” for details.

---

## Defmt capture summary

- **Card present:** `PowerOn: card_present=true` on all PowerOn attempts.
- **First attempt:** Pre-ATR drain discarded 1 stale byte (0x00). Then **ATR timeout** (no ATR bytes received within 1000 ms) → ATR failed → CCID PowerOn failed.
- **Second and third attempts:** No drain logged; again **ATR timeout** → ATR failed → CCID PowerOn failed.

No ATR bytes were received (not even TS). So the failure is **ATR timeout (zero bytes)**, not truncated ATR (len=1).

## Root cause (inferred)

The firmware reaches `read_atr()` and waits up to `SC_ATR_TIMEOUT_MS` (1000 ms) for the first character (TS). The card never sends it in time. Possible causes:

1. **Power-on / RST timing:** Seedkeeper may need longer than current `SC_POWER_ON_DELAY_MS` (20 ms) and/or `SC_ATR_POST_RST_DELAY_MS` (5 ms) before it starts sending the ATR.
2. **Baud rate:** Current 11290 baud (Fi/Di default) might not match what the card expects (less likely if the card never sends any byte).
3. **Hardware:** Pinout, voltage, or reader–card compatibility (STM32 vs Seedkeeper) could prevent the card from responding.

## Recommendation (firmware)

- **Try longer delays:** Increase `SC_POWER_ON_DELAY_MS` (e.g. to 50–100 ms) and/or `SC_ATR_POST_RST_DELAY_MS` (e.g. to 20–50 ms) so the card has more time to stabilize after RST before the reader expects the first ATR byte.
- **Optional:** Add a defmt log line for “waiting for first ATR byte” with the timeout value to confirm in future captures that we are in this path.

## Full Seedkeeper test result (after capture)

- **Command:** `python3 /tmp/test_seedkeeper.py "CardMan" 1234`
- **Result:** Fail — “Card is unpowered. (0x80100067)” (Connect failed: Unable to connect with protocol: T0 or T1. Card is unpowered.)
- **USB:** 076b:3021 (OmniKey AG CardMan 3021/3121) present; pcscd restarted before test.

## Deliverables

- Defmt capture: `/tmp/defmt_capture.log` (retrieved from remote).
- This diagnosis and the recommendation above.
- Full test re-run (Step 6): `python3 /tmp/test_seedkeeper.py "CardMan" 1234` — **Result:** Fail (same: "Card is unpowered. (0x80100067)"). No firmware change applied yet; applying the recommended delay increase and re-flashing may resolve it.

---

## Latest run (plan execution)

- **Defmt capture:** `ccid-reader/defmt_capture.log` (from remote `/tmp/defmt_capture.log`).
- **Card present:** `PowerOn: card_present=true` on all PowerOn attempts.
- **ATR:** **ATR timeout** — no bytes received (not even TS) within 1000 ms. No Pre-ATR drain logged this run. NACK is already disabled before ATR and `clear_usart_errors()` is in place; failure is still zero bytes from card.
- **Conclusion:** Root cause remains **power-on/RST timing**: Seedkeeper does not send the first ATR byte before our timeout. **Recommendation:** Increase `SC_POWER_ON_DELAY_MS` to 50–100 ms and `SC_ATR_POST_RST_DELAY_MS` to 20–50 ms in `smartcard.rs`, then re-flash and re-test.
- **Full test (Step 6) again:** `python3 /tmp/test_seedkeeper.py "CardMan" 1234` — **Result:** Fail (same: "Card is unpowered. (0x80100067)").

---

## Activation checklist (physical-layer verification)

When ATR times out with **zero bytes** (no TS), the failure is at the activation layer. Verify with scope or logic analyzer:

| Contact | Pin (STM32) | Check |
|---------|-------------|--------|
| **C1 (VCC)** | PWR (PC5): low = power on | Is the card powered at the expected class voltage (3 V or 5 V) after PWR low? |
| **C2 (RST)** | PG10 (active low: low = reset asserted) | Does RST hold low then transition cleanly high? |
| **C3 (CLK)** | PA4 (USART2_CK) | Is there a **stable clock** on the card CLK pin after power-on? |
| **C7 (I/O)** | PA2 (USART2_TX, open-drain) | Does I/O idle high, and is there **any** transition after reset? If I/O never moves → hardware/activation. If I/O moves but firmware sees nothing → UART config (parity, inversion, framing). |

**OMNIKEY control test:** Run `pcsc_scan` with the **same** Seedkeeper in the **real** OMNIKEY 3021 (or another reference reader). If ATR is **present** there, STM32 activation/CLK is the prime suspect. If the card is also **unpowered** in the OMNIKEY, suspect card insertion, contact, or card health first.

---

## CLK-first run (after plan implementation)

- **Firmware changes:** Explicit `set_clock(true)` and "CLK enabled before RST" log before RST; "ATR: waiting for first byte (timeout N ms)" log; on first-byte timeout, SR telemetry `ATR timeout SR=0xXXXX (FE= PE= ORE= NE=)`; optional "USART cleared ORE" at debug; delays 50 ms power-on, 20 ms post-RST.
- **Defmt capture:** `defmt_capture.log` shows "CLK enabled before RST", "ATR: waiting for first byte (timeout 400 ms)", then **ATR timeout SR=0x01C0 (FE=0 PE=0 ORE=0 NE=0)**. No framing/parity/overrun/noise errors → **line probably never toggled** (no clock at card or no card drive on I/O). Next step: scope C3 (CLK) and C7 (I/O) per activation checklist.
- **Seedkeeper test:** Still fails with "Card is unpowered (0x80100067)".
