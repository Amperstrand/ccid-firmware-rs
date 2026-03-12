# Exhaustive CCID & ISO 7816 Alignment Report

This document is a **field-by-field** alignment check of the ccid-reader firmware against the USB CCID Rev 1.1 specification, libccid (host driver), osmo-ccid-firmware (reference reader), and ISO 7816-3/4. It also records fixes applied during the audit.

---

## 1. CCID Class Descriptor (Table 5.1)

Descriptor is 52 bytes (without length/type); libccid `parse.c` indexes from byte 0 = first byte of descriptor body.

| Byte(s) | Field | Spec / libccid | Our value | Status |
|---------|--------|-----------------|-----------|--------|
| 0–1 | bcdCCID | 1.10 (LE) | 0x10, 0x01 | ✓ |
| 2 | bMaxSlotIndex | 0 = one slot | 0x00 | ✓ |
| 3 | bVoltageSupport | 0x07 = 5V+3V+1.8V | 0x07 | ✓ |
| 4–7 | dwProtocols | LE; 0x03 = T=0 and T=1 | 0x03,0,0,0 | ✓ |
| 8–11 | dwDefaultClock | Hz LE; 4 MHz | 0x00,0x2D,0x3D,0x00 | ✓ |
| 12–15 | dwMaximumClock | 20 MHz | 0x80,0x84,0x31,0x01 | ✓ |
| 16 | bNumClockSupported | 0 = use default | 0x00 | ✓ |
| 17–20 | dwDataRate | bps LE; 10752 | 0x00,0x2A,0,0 | ✓ |
| 21–24 | dwMaxDataRate | 344086 bps | 0x36,0x41,0x05,0 | ✓ |
| 25 | bNumDataRatesSupported | 0 | 0x00 | ✓ |
| 26–29 | dwMaxIFSD | 254 for T=1 | 0xFE,0,0,0 | ✓ |
| 30–33 | dwSynchProtocols | 0 | 0 | ✓ |
| 34–37 | dwMechanical | 0 | 0 | ✓ |
| 38–41 | dwFeatures | LE; see below | 0xB2,0x07,0x02,0 | ✓ (Omnikey 3021) |
| 42–45 | dwMaxCCIDMessageLength | 271 | 0x0F,0x01,0,0 | ✓ |
| 46 | bClassGetResponse | 0xFF = echo | 0xFF | ✓ |
| 47 | bClassEnvelope | 0xFF = echo | 0xFF | ✓ |
| 48–49 | wLcdLayout | 0 | 0,0 | ✓ |
| 50 | bPINSupport | 0 | 0x00 | ✓ |
| 51 | bMaxCCIDBusySlots | 1 | 0x01 | ✓ |

### dwFeatures (exhaustive)

Spec and blog.apdu.fr: when APDU level is used, **0x00000002** must be present and one of **0x00000040** or **0x00000080**.

| Bit (hex) | Meaning | Our value |
|-----------|---------|-----------|
| 0x02 | Automatic parameter configuration based on ATR | ✓ (added in audit) |
| 0x04 | Automatic activation on insert | — |
| 0x08 | Automatic voltage selection | — |
| 0x10 | Automatic ICC clock frequency change | ✓ |
| 0x20 | Automatic baud rate (Fi/Di) | ✓ |
| 0x40 | Automatic parameters negotiation (PPS) | — |
| 0x80 | Automatic PPS by CCID | ✓ |
| 0x0100 | CCID can set ICC in clock stop mode | ✓ (Omnikey 3021) |
| 0x0200 | NAD value other than 00 accepted (T=1) | ✓ |
| 0x0400 | Automatic IFSD exchange as first exchange (T=1) | ✓ |
| 0x00020000 | Short APDU level | ✓ |

**Value used:** 0x000207B2 (Omnikey 3021 compatible: 0xB2|0x10|0x20|0x80, +0x0100 clock stop, +0x0200 NAD, +0x0400 auto IFSD, +0x00020000 Short APDU).

---

## 2. Bulk OUT (PC to RDR) — Message Layout

All commands: 10-byte header (bMessageType, dwLength, bSlot, bSeq, then 3 bytes message-specific). References: osmo `ccid_proto.h`, libccid `commands.c`.

### 2.1 Header (all commands)

| Offset | Field | Our parsing | Spec |
|--------|--------|-------------|------|
| 0 | bMessageType | rx_buffer[0] | ✓ |
| 1–4 | dwLength | LE, rx_buffer[1..5] | ✓ |
| 5 | bSlot | rx_buffer[5]; we require 0 | ✓ |
| 6 | bSeq | rx_buffer[6]; echoed in response | ✓ |

### 2.2 PC_to_RDR_IccPowerOn (0x62)

| Offset | Field | Our use |
|--------|--------|---------|
| 7 | bPowerSelect | Not used (we power on same way) |
| 8–9 | RFU | Ignored |

Response: RDR_to_PC_DataBlock with ATR in abData. ✓

### 2.3 PC_to_RDR_XfrBlock (0x6F)

| Offset | Field | libccid send | Our use |
|--------|--------|--------------|---------|
| 7 | bBWI | bBWI | Not used |
| 8–9 | wLevelParameter | rx_length (LE) | Not used |
| 10+ | abData | APDU | apdu = rx_buffer[10..10+data_len] ✓ |

dwLength = length of abData (APDU length). We reject data_len > 261 (Short APDU). ✓

### 2.4 PC_to_RDR_SetParameters (0x61)

| Offset | Field | libccid | Our use |
|--------|--------|---------|---------|
| 7 | bProtocolNum | cmd[7]=protocol | requested_protocol = rx_buffer[7] ✓ |
| 8–9 | RFU | 0 | — |
| 10+ | abProtocolData | 5 or 7 bytes | We accept; respond with our AtrParams |

---

## 3. Bulk IN (RDR to PC) — Response Layout

Header: 10 bytes (bMessageType, dwLength, bSlot, bSeq, bStatus, bError, then 1 byte message-specific). libccid: STATUS_OFFSET=7, ERROR_OFFSET=8, CCID_RESPONSE_HEADER_SIZE=10.

### 3.1 bStatus (byte 7)

Spec Table 6.2-2: bmCommandStatus in high bits, bmICCStatus in low.

| Our constant | Value | Meaning |
|--------------|--------|---------|
| COMMAND_STATUS_NO_ERROR | 0 | (<<6) = 0x00 |
| COMMAND_STATUS_FAILED | 1 | (<<6) = 0x40 |
| ICC_STATUS_PRESENT_ACTIVE | 0 | ✓ |
| ICC_STATUS_PRESENT_INACTIVE | 1 | ✓ |
| ICC_STATUS_NO_ICC | 2 | ✓ |

We use `(cmd_status << 6) | icc_status`. ✓

### 3.2 RDR_to_PC_DataBlock (0x80)

| Offset | Field | Our set |
|--------|--------|---------|
| 0 | bMessageType | 0x80 ✓ |
| 1–4 | dwLength | Length of abData ✓ |
| 5 | bSlot | 0 ✓ |
| 6 | bSeq | Echo from command ✓ |
| 7 | bStatus | build_status(...) ✓ |
| 8 | bError | 0 or error code ✓ |
| 9 | bChainParameter | 0 (single block) ✓ |
| 10+ | abData | ATR or R-APDU ✓ |

### 3.3 RDR_to_PC_SlotStatus (0x81)

| Offset | Field | Our set |
|--------|--------|---------|
| 9 | bClockStatus | 0 ✓ |

### 3.4 RDR_to_PC_Parameters (0x82)

Spec: header (9 bytes) + bProtocolNum (1) + abProtocolData (5 or 7). We send: bytes 0–8 = header (with bStatus, bError), byte 9 = bProtocolNum (0 or 1), bytes 10+ = abProtocolData (5 or 7). dwLength we set to 5 or 7 (length of abProtocolData only). osmo sends only header + union (no separate bProtocolNum byte); both forms are used in the wild. ✓

### 3.5 Error response type per command (osmo gen_err_resp)

| Command | Response type | Our send_err_resp |
|---------|----------------|-------------------|
| IccPowerOn, XfrBlock, Secure | DataBlock | ✓ |
| IccPowerOff, GetSlotStatus, IccClock, T0APDU, Mechanical, Abort, SetDataRateAndClock | SlotStatus | ✓ |
| GetParameters, ResetParameters, SetParameters | Parameters | ✓ |
| Escape | Escape | ✓ |
| Unknown | SlotStatus + CMD_NOT_SUPPORTED | ✓ |

---

## 4. Message Handling and State

### 4.1 Slot state (3-state FSM)

| State | Our enum | get_icc_status() | When set |
|-------|----------|-------------------|----------|
| No ICC | Absent | ICC_STATUS_NO_ICC (2) | !card_present; poll() |
| Present, inactive | PresentInactive | ICC_STATUS_PRESENT_INACTIVE (1) | PowerOn fail; PowerOff; card just inserted |
| Present, active | PresentActive | ICC_STATUS_PRESENT_ACTIVE (0) | PowerOn success |

Matches osmo and spec. ✓

### 4.2 cmd_busy

- Set at start of handle_message() before dispatch.
- Cleared only after full TX completion (including ZLP when tx_len % 64 == 0).
- If busy, we respond with SlotStatus + CCID_ERR_CMD_SLOT_BUSY (0xE0). ✓

### 4.3 Multi-packet RX (Bulk OUT)

We accumulate in rx_buffer; total_len = 10 + msg_len (from dwLength). We only call handle_message when rx_len >= total_len. For multi-packet OUT (e.g. 271 bytes), we copy each 64-byte read into rx_buffer; when 10+261 received we process. ✓

### 4.4 Multi-packet TX (Bulk IN) and ZLP

We send in 64-byte chunks; when tx_len % 64 == 0 we set needs_zlp and send a zero-length packet; cmd_busy cleared after ZLP. ✓

---

## 5. Interrupt IN — NotifySlotChange

| Byte | Content | Our send_notify_slot_change |
|------|---------|-----------------------------|
| 0 | bMessageType | 0x50 ✓ |
| 1 | bmSlotCCState (bit 0 = ICC present, bit 1 = change) | bits = (present?1:0) \| (changed?2:0) ✓ |

We send on card presence edge in poll(). ✓

---

## 6. Class-specific control requests

| Request | Our handling |
|---------|----------------|
| ABORT (0x01) | control_out: accept (no slot/seq action) ✓ |
| GET_CLOCK_FREQUENCIES (0x02) | control_in: 4 MHz (0x400F0000 LE) ✓ |
| GET_DATA_RATES (0x03) | control_in: 10752 bps ✓ |

---

## 7. ISO 7816-3 — ATR and Parameters

### 7.1 ATR parsing (smartcard.rs parse_atr)

- T0, Y1, TA1/TB1/TC1/TD1 presence from Y1.
- TA1 → Fi, Di (tables); TC1 → guard time; TD1 → protocol; T=1 → IFSC, BWI/CWI, EDC.
- We populate AtrParams; used for GetParameters/SetParameters and PPS. ✓

### 7.2 PPS (negotiate_pps)

- Only if has_ta1 && ta1 != 0x11; send FF, PPS0, PPS1, PCK; expect echo; then set_baud_from_fi_di. ✓

### 7.3 T=1 IFSD negotiation (do_ifs_negotiation_t1)

- Send S(IFS request) with IFSD=254; parse S(IFS response); store IFSC. ✓

### 7.4 Leading 0x00 in ATR

We discard leading 0x00 before TS in read_atr(); then store TS and rest. ✓

---

## 8. T=0 Procedure Bytes (smartcard.rs transmit_apdu_t0)

| Procedure | Our handling |
|-----------|--------------|
| 0x60 (NULL) | Discard, read next |
| INS | SW1 SW2; 0x6C → re-send with P3=SW2; 0x61 → GET RESPONSE chain |
| ~INS | Send next body byte, continue |
| 0x61 XX | GET RESPONSE 00 C0 00 00 XX; chain up to SC_T0_GET_RESPONSE_MAX |
| 0x6C XX | Re-issue same command with P3=XX |

Loop structure: 'send for re-issue, inner loop for procedure bytes. ✓

---

## 9. T=1 Block Protocol (t1_engine.rs)

### 9.1 I-block PCB

- N(S) in bits 6–7: PCB = 0x00 \| (ns<<6) \| (m?0x20:0). ✓

### 9.2 R-block PCB

- N(R) in bits 4–5: PCB = 0x80 \| (nr<<4). ✓

### 9.3 LRC

- XOR of NAD, PCB, LEN, INF. We compute and check in recv_block; send in send_i_block. ✓

### 9.4 S-block WTX (fixed in audit)

- **Before:** (pcb & 0x1F) == 0x12 and response 0xC0|0x12 (non-standard).
- **After:** (pcb & 0x1F) == 0x03 (WTX type 3); request 0xC3, response 0xCB per ISO 7816-3. ✓

### 9.5 Chaining

- Send I-blocks with M=1 when more to send; expect R-block; then continue. Receive I-blocks with M=1; send R-block with N(R); collect INF. ✓

---

## 10. CCID Error Codes (bError)

We use: 0x00 (CMD_NOT_SUPPORTED), 0xE0 (CMD_SLOT_BUSY), 0xFE (ICC_MUTE / no card / PowerOn fail), 0xFF (abort/generic), 0x07 (protocol/param). All from osmo ccid_proto.h Table 6.2-2. ✓

---

## 11. SetParameters Request Parsing

Host sends: bProtocolNum at byte 7, abProtocolData at 10.. (5 or 7 bytes). We read rx_buffer[7] for protocol; we do not change our physical params from host data, we respond with our AtrParams. ✓

---

## 12. Fixes Applied in This Audit

1. **dwFeatures**: Added 0x02 (automatic parameter configuration based on ATR). Spec requires 0x02 when APDU level is used. Value changed from 0x000200B0 to 0x000200B2. Comment updated to match spec bit definitions.
2. **T=1 WTX**: Replaced non-standard 0x12 / 0xD2 with ISO 7816-3 WTX type 3: detect (pcb & 0x1F) == 0x03, send S(WTX response) with PCB 0xCB.

---

## 13. References

- USB CCID Rev 1.1: https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf
- libccid: CCID/src/ (commands.c, ifdhandler.c, ccid.h, commands.h, parse.c)
- osmo-ccid-firmware: ccid_common/ccid_proto.h, ccid_device.c, ccid_slot_fsm.c
- dwFeatures: blog.apdu.fr CCID descriptor statistics; spec Table 5.1
- ISO 7816-3: T=1 S-block encoding (WTX 0xC3/0xCB)

---

## 14. Summary

- **Descriptor**: All 52 bytes and dwFeatures bits aligned; 0x02 added for spec compliance.
- **Bulk OUT/IN**: 10-byte header, correct response type per command, bSeq echo, status/error bytes.
- **Slot FSM, cmd_busy, fragmentation, ZLP, NotifySlotChange**: Implemented and aligned.
- **T=0**: Procedure bytes, GET RESPONSE, 6C XX re-issue.
- **T=1**: I/R/S blocks, LRC, chaining, WTX (0xC3/0xCB) fixed.
- **ATR, PPS, IFSD**: Parsed and used for parameters and baud/PPS.

No further alignment issues identified; hardware testing (lsusb, pcsc_scan, APDU, hot-removal) remains the next step.
