# CCID Reader — Spec Alignment & Verification Checklist

This document summarizes alignment with the CCID specification, libccid, and osmo-ccid-firmware, and provides a checklist before and during hardware testing.

## References

| Reference | Purpose |
|-----------|---------|
| [USB CCID Rev 1.1](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf) | Message formats, descriptor, bStatus/bError, response types |
| **libccid** (CCID/ in workspace) | Host behaviour: XfrBlock layout, bSeq, GetParameters/SetParameters |
| **osmo-ccid-firmware** (ccid_common/) | Reader behaviour: 14-command dispatch, gen_err_resp, slot FSM, error codes |

---

## 1. Spec alignment (code vs CCID Rev 1.1)

### 1.1 Descriptor

| Field | Required / Typical | Our value | Notes |
|-------|--------------------|-----------|--------|
| bcdCCID | 1.10 | 0x0110 | ✓ |
| bMaxSlotIndex | 0 (single slot) | 0 | ✓ |
| bVoltageSupport | 5V+3V+1.8V | 0x07 | ✓ |
| dwProtocols | T=0 and T=1 | 0x00000003 | ✓ |
| dwFeatures | Short APDU + 0x02 + auto + clock stop + NAD + auto IFSD | 0x000207B2 | ✓ |
| dwMaxCCIDMessageLength | 261+10 | 271 | ✓ |
| bNumEndpoints | 3 (Bulk IN, Bulk OUT, Int IN) | 3 | ✓ (all three in config descriptor) |

### 1.2 Message layouts

- **PC_to_RDR_XfrBlock**  
  Host (libccid) sends: `[0]=0x6F, [1..4]=dwLength(LE), [5]=slot, [6]=bSeq, [7]=bBWI, [8..9]=wLevelParameter(LE), [10..]=abData (APDU)`.  
  We use `CCID_HEADER_SIZE = 10` and APDU at `rx_buffer[10..10+data_len]` → **aligned**.

- **RDR_to_PC_DataBlock**  
  We send: `[0]=0x80, [1..4]=dwLength, [5]=slot, [6]=bSeq, [7]=bStatus, [8]=bError, [9]=bChainParameter, [10..]=abData`.  
  Matches spec and osmo `ccid_rdr_to_pc_data_block` (header_in + bChainParameter + abData). ✓

- **RDR_to_PC_SlotStatus**  
  Header + bStatus + bError + bClockStatus (10 bytes). We send 10-byte SlotStatus. ✓

- **RDR_to_PC_Parameters**  
  Spec: header (9) + bProtocolNum (1) + abProtocolData (5 or 7). We send 10-byte header including byte 9 = bProtocolNum, then 5/7 bytes. ✓

- **bStatus**  
  `(bmCommandStatus << 6) | bmICCStatus`. We use `build_status(cmd_status, icc_status)`. ✓

- **Error codes**  
  We use the full CCID error set (0x00, 0xE0, 0xEF–0xFF) per osmo `ccid_proto.h`. ✓

### 1.3 Command/response type mapping

Per osmo `gen_err_resp()` and spec:

- DataBlock (0x80): IccPowerOn, XfrBlock, Secure ✓  
- SlotStatus (0x81): IccPowerOff, GetSlotStatus, IccClock, T0APDU, Mechanical, Abort, SetDataRateAndClockFrequency ✓  
- Parameters (0x82): GetParameters, SetParameters, ResetParameters ✓  
- Escape (0x83): Escape ✓  

All 14 commands are handled with the correct response type. ✓

---

## 2. libccid alignment

- **XfrBlock send**  
  libccid builds: `cmd[0..9]` as above, `memcpy(cmd+10, tx_buffer, tx_length)`. We read APDU from offset 10. ✓

- **bSeq**  
  We echo `seq` from the request in every response. ✓

- **GetParameters / SetParameters**  
  Host sends SetParameters with `cmd[7] = protocol`; we read `rx_buffer[7]` as requested protocol and respond with Parameters (real values from AtrParams). ✓

---

## 3. T=1 block format (ISO 7816-3)

- **I-block PCB**  
  `0x00 | (N(S)<<6) | (M?0x20:0)`. We use `PCB_I_BLOCK | (ns << 6) | if m { I_M_CHAIN }`. ✓  

- **R-block PCB**  
  `0x80 | (N(R)<<4) | (00=ACK, 01=EDC error, 10=other)`. We send R-block ACK for chained I-blocks. When the card sends R-block with bits 1-0 ≠ 00 (retransmit request), we resend the last I-block (up to 3 retries). ✓  

- **LRC**  
  XOR of NAD, PCB, LEN, INF. We compute over the same bytes. ✓  

- **S-block WTX**  
  We handle S(IFS request/response) and WTX (ISO 7816-3 type 3: request 0xC3, response 0xCB) in `t1_engine.rs`. ✓  

---

## 4. Pre-test checklist (no hardware)

- [x] `cargo build --release --target thumbv7em-none-eabihf` succeeds  
- [x] All 14 CCID commands have a handler (6 full, 8 CMD_NOT_SUPPORTED with correct type)  
- [x] Slot FSM: get_icc_status() uses slot_state; PowerOn/PowerOff/poll update it  
- [x] cmd_busy set at start of handle_message, cleared only after full TX (including ZLP)  
- [x] TX: 64-byte chunks + ZLP when `tx_len % 64 == 0`  
- [x] Interrupt endpoint in configuration descriptor  
- [x] XfrBlock: APDU at offset 10, reject dwLength > 261  
- [x] T=0: GET RESPONSE (61 XX), wrong Le (6C XX), procedure bytes (NULL, INS, ~INS)  
- [x] T=1: I/R/S blocks, LRC, chaining, IFSC from IFS negotiation  
- [x] PPS attempted when has_ta1 && ta1 != 0x11  
- [x] GetParameters/SetParameters use AtrParams (T=0: 5 bytes, T=1: 7 bytes + bProtocolNum)  

---

## 5. Hardware test steps

Run on the host that has the reader (e.g. after `probe-rs run` from the plan).

1. **Enumeration**  
   `lsusb -v -d 08E6:3437`  
   - bNumEndpoints = 3  
   - CCID descriptor: dwProtocols 0x03, dwFeatures 0x000207B2  

2. **pcscd**  
   `pcsc_scan` (card inserted)  
   - Reader name includes **Gemalto PC Twin Reader** / **IDBridge CT30** alias depending on host stack.  
   - ATR starts with `3B` (no leading 0x00)  

3. **APDU**  
   Run: `python3 test_ccid_apdu.py` or `python3 test_ccid_apdu.py "IDBridge"`.  
   Script connects, gets ATR, sends one short APDU (SELECT), checks SW.  

4. **Hot-removal**  
   Remove card, re-insert; confirm NotifySlotChange (no "falling back to polling" in pcscd logs if possible).  

5. **pysatochip** (if available)  
   SELECT SatoChip AID, GET_STATUS; confirms end-to-end with pcscd.  

---

## 6. Known limitations / follow-ups

- **ATR truncation (resolved)**: Previously ATR could be truncated to one byte (TS only) because NACK was enabled during ATR reception. Firmware now disables NACK before ATR, clears USART ORE before each byte, and re-enables NACK only for T=0 after protocol detection. See [RESEARCH_AGENDA.md](RESEARCH_AGENDA.md) §7.1 and [OPEN_RESEARCH_QUESTIONS.md](OPEN_RESEARCH_QUESTIONS.md).
- **Extended APDU**: Rejected (dwLength > 261); acceptable for many cards and libccid Short APDU path.  
- **SetDataRateAndClockFrequency**: Implemented; sets USART BRR from dwDataRate, returns RDR_to_PC_DataRateAndClockFreq with actual values. ✓  
- **Voltage/clock**: We do not change voltage or clock per host request; descriptor advertises support only.  
- **static_mut_refs**: `USB_EP_MEMORY` in main.rs triggers a rust_2024 lint; API expects `&mut`; leave as-is unless HAL accepts raw pointer.  

---

## 7. Optional: unit / host-side tests

- **Descriptor dump**: Parse our `CCID_CLASS_DESCRIPTOR_DATA` and assert dwProtocols, dwFeatures, sizes.  
- **Mock transport**: Implement `SmartcardDriver` and `T1Transport` with fixed ATR/APDU responses to exercise CCID state machine and T=0/T=1 without hardware.  

These are not required for a first pass; hardware tests above are the main validation.

---

## 8. PC/SC (pcscd) integration

### 8.1 Reader recognition

- **Firmware VID:PID**: `0x08E6:0x3437` (see `src/usb_identity.rs`). The firmware emulates an IDBridge CT30-compatible identity.  

### 8.2 udev (Linux)

- **Generic CCID rule**: In `CCID/src/92_pcscd_ccid.rules`, the line `ENV{ID_USB_INTERFACES}=="*:0b0000:*", GROUP="pcscd"` applies to **any** USB device with interface class 0x0B (CCID).  
- **Action**: Install the standard libccid udev rules (e.g. `sudo meson install` from the CCID build). Ensure the user running pcscd is in the `pcscd` group. If you see "access denied" or "could not claim interface", run `groups` and add the user to `pcscd`; then unplug/replug the reader or restart pcscd.  

### 8.3 pcscd restart and hotplug

- After **editing Info.plist** or **changing supported_readers.txt and reinstalling the driver**: unplug all CCID readers so the driver is unloaded, then replug; or **restart pcscd** (e.g. `sudo systemctl restart pcscd` or `sudo killall pcscd` then start pcscd again).  
- After **changing udev rules**: run `sudo udevadm control --reload-rules` and optionally `sudo udevadm trigger`; then unplug/replug the reader.  

### 8.4 Correlating reader with USB device

- **lsusb**: `lsusb | grep 08E6` shows our reader as `ID 08e6:3437`.  
- **pcsc_scan -r**: Lists readers by name; usually `Gemalto PC Twin Reader` or compatible alias.  

### 8.5 Firmware vs libccid (Short APDU path)

- We advertise **Short APDU only** (dwFeatures bit 17). libccid uses the same bulk message path for Short APDU readers: PowerOn → (optional GetParameters/SetParameters) → XfrBlock for each APDU. Our firmware handles bSeq echo, 10-byte header, and ZLP for exact multiples of 64 bytes. For verbose CCID traffic: `LIBCCID_ifdLogLevel=0x0F pcscd --foreground --debug` (then run test_ccid_apdu.py in another terminal).
- **Timeouts**: If the card is slow, libccid may retry; we clear `cmd_busy` only after the full response (including ZLP) is sent, so bSeq stays consistent.

---

## 9. Cross-platform verification (Windows, Linux, macOS)

The firmware emulates an IDBridge CT30-compatible CCID reader (`VID:PID 0x08E6:0x3437`).

- **Windows**: The generic Microsoft driver **usbccid.sys** loads automatically for any USB device with interface class 0x0B (CCID). Plug in the reader; it appears in Device Manager under "Smart card readers". Verify with `certutil -scinfo` or the Windows Smart Card Manager. No driver install or VID:PID registration needed.

- **Linux**: The VID/PID is already in libccid supported lists on common distributions, so standard pcscd/libccid installs work out of the box. Run `pcsc_scan` to see the reader; run `python3 test_ccid_apdu.py "IDBridge"` for an APDU test.

- **macOS**: The system CCID stack supports class-compliant readers. Install `pcsc-tools` (e.g. via Homebrew) and run `pcsc_scan` to verify enumeration and card presence.

---

## 10. Pin usage and debug (ST-Link)

**There is no pin conflict between the smartcard slot and the debug interface.**

- **ST-Link (SWD)** on STM32F469-DISCO uses:
  - **PA13** = SWDIO  
  - **PA14** = SWCLK  
  These are dedicated debug pins and are not used by the firmware for the smartcard.

- **Smartcard slot** (see `main.rs` and §5) uses:
  - **PA2** = IO (USART2_TX, smartcard half-duplex data)  
  - **PA4** = CLK (USART2_CK)  
  - **PG10** = RST (reset, active LOW)  
  - **PC2** = PRES (card presence, HIGH = present)  
  - **PC5** = PWR (power control, LOW = power on)  

So the debugger does **not** drive or share PA2, PA4, PG10, PC2, or PC5. Flashing with `st-flash` or leaving the ST-Link connected after reset does not interfere with the smartcard pins.

**If the card works in another reader but not in this one:** the cause is unlikely to be the ST-Link. More likely:
- **Protocol/timing**: pcsc_scan showed "Card inserted" and "ATR: 3B", so the slot and initial power-on/ATR path work; "Card is unresponsive" when connecting with T=0/T=1 can be due to the first APDU or parameter exchange (timing, protocol handling), not pins.
- **Hardware layout**: If you use a custom carrier or shield, confirm the physical smartcard connector is wired to PA2, PA4, PG10, PC2, PC5 as above. On the stock STM32F469-DISCO, the extension headers expose these pins; the actual slot may be on an add-on board (e.g. Specter shield) whose schematic defines the mapping.

---

## 11. Voltage, reset and debugging (replicating Gemalto behaviour)

### 11.1 Voltage

**The firmware does not switch or select card voltage.** The USB descriptor advertises `bVoltageSupport = 0x07` (5.0 V, 3.0 V, 1.8 V) for compatibility, but the **hardware** (board or add-on) must supply the correct voltage to the slot. Most modern cards expect **3 V**; 5 V is legacy. **Slot VCC must be 3 V for SatoChip/Seedkeeper** (see [EXTERNAL_RESEARCH_REQUEST.md](EXTERNAL_RESEARCH_REQUEST.md) §6). If the card works in a Gemalto reader (typically 3 V or 5 V depending on model), ensure your STM32 carrier/shield supplies the same voltage to the smartcard contacts. There is no software-controlled voltage switch in this firmware.

### 11.2 Reset and power-on (Gemalto-style)

To better match commercial readers (e.g. Gemalto), the firmware now:

- **Full power cycle on PowerOn:** When the host sends PC_to_RDR_IccPowerOn, the reader performs a full power cycle (PWR off → 50 ms → then full power-on sequence) if the card was already powered. This gives a clean cold reset every time the application connects.
- **Longer delays:** Power-on delay 20 ms (was 10 ms), reset release delay 25 ms (was 15 ms), post-RST delay 5 ms (was 2 ms). These are more conservative and help slower or marginal cards.

If the card still does not respond after ATR, check firmware logs (see §11.4) for procedure-byte timeouts or protocol errors.

### 11.3 Protocol (T=0 vs T=1)

Protocol is **detected from the ATR** (TD1 byte). The same card that works in a Gemalto in T=0 (or T=1) will be driven in the same way here. The firmware logs `protocol=T=0` or `protocol=T=1` after power-on. If the host reports "Card is unresponsive", capture defmt logs to see whether the first XfrBlock reaches the card and whether a procedure byte (T=0) or block (T=1) is received.

### 11.4 Firmware logging (defmt over RTT)

With the device connected via **probe-rs** (ST-Link), you can stream defmt logs to see:

- Power-on sequence and full **ATR hex**
- **APDU** (first 12 bytes) for each XfrBlock
- **T=0 procedure bytes** (0x60 NULL, then INS / ~INS / 0x61 / 0x6C etc.)
- **Rx timeout** and **XfrBlock failed** messages

Example (run on host with device attached via ST-Link):

```bash
DEFMT_LOG=info probe-rs attach --chip STM32F469NIHx
# Or: cargo run (runner is probe-rs run) and use a second terminal with probe-rs attach + defmt-print
```

Alternatively build with `DEFMT_LOG=debug` in `.cargo/config.toml` for more verbose output. Logs are sent over RTT; use `probe-rs` or a J-Link/OpenOCD RTT viewer to capture them.

### 11.5 Host-side debugging (libccid / pcscd)

To see what the **host** is sending and why it might report "Card is unresponsive":

- **Linux (libccid + pcscd):** Run pcscd in foreground with debug and maximum libccid logging:
  ```bash
  LIBCCID_ifdLogLevel=0x0F pcscd --foreground --debug
  ```
  Then in another terminal run your test (e.g. `python3 test_ccid_apdu.py "IDBridge"`). Check pcscd output for PowerOn, GetParameters, SetParameters, XfrBlock and any timeouts or errors.

- **Reader name:** Ensure you are connecting to the correct reader (`Gemalto PC Twin Reader` / `IDBridge CT30` alias). If multiple readers are present, the application may be connecting to another reader.
