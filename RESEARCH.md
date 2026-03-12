# CCID Reader — Research Summary (Online + Local Projects)

This document summarizes online research and local project references used to align the ccid-reader firmware with the CCID specification, libccid, and proven reader implementations.

---

## 1. Online references

### 1.1 USB CCID specification (Rev 1.1)

- **Source**: [USB-IF Smart Card CCID 1.1](https://www.usb.org/document-library/smart-card-ccid-version-11), PDF: [DWG_Smart-Card_CCID_Rev110.pdf](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf)
- **Findings**:
  - Messages use a **10-byte header**: byte 0 = command/response code, bytes 1–4 = payload length (LSB-first), byte 5 = slot, byte 6 = sequence number, bytes 7–9 = command parameters or status/error.
  - **PC_to_RDR_XfrBlock**: payload is the APDU in `abData` (at offset 10).
  - **RDR_to_PC_DataBlock**: same header layout; response carries ATR or R-APDU data.
  - **Interrupt pipe**: used for card insertion/removal (e.g. 8-byte NotifySlotChange).

### 1.2 SpringCore CCID (host view)

- **Source**: [SpringCore CCID (PC/SC) over USB](https://docs.springcard.com/books/SpringCore/Host_Interfaces/USB/CCID_(PCSC))
- **Findings**:
  - Endpoints: Control, **BulkOut 64B** (PC→RDR), **BulkIn 64B** (RDR→PC), **InterruptIn 8B** (notifications).
  - Bulk messages: bytes 0–9 = header, 10–N = payload (up to 64 kB).
  - Header: byte 0 = code, 1–4 = length (LSB), 5 = slot, 6 = seq, 7–9 = parameters (out) or status/error (in).

### 1.3 libccid / ccid.apdu.fr

- **Sources**: [CCID driver](https://ccid.apdu.fr/), [Extended APDU](https://ccid.apdu.fr/ccid_extended_apdu.html), [Card movement notification](https://blog.apdu.fr/posts/2010/08/new-ccid-140-and-card-movement/)
- **Findings**:
  - **Exchange levels** (from `dwFeatures`): Character, TPDU, **Short APDU**, Short+Extended APDU.
  - **Short APDU**: reader limited to short APDU only; no extended APDU; driver sends raw APDU in XfrBlock.
  - **Extended APDU**: requires TPDU or Extended APDU reader, or T=1 card; T=0 extended is done via ENVELOPE/GET RESPONSE at application level.
  - NotifySlotChange (CCID 1.4+) avoids polling; without interrupt endpoint the driver falls back to usleep polling.

### 1.4 ISO 7816-3 T=0 procedure bytes

- **Sources**: Stack Overflow / CardWerk (T=0, GET RESPONSE, wrong Le).
- **Findings**:
  - **Case 4 (command + response)**: card may respond with `61 YY` (YY bytes ready); terminal sends GET RESPONSE `00 C0 00 00 YY`; card returns data + SW1-SW2.
  - **Case 2 (response only)**: if Le is wrong, card returns `6C XX` (correct Le = XX); terminal resends same command with P3=XX.
  - Procedure bytes: NULL (0x60), INS, ~INS, 61 XX, 6C XX as in ISO 7816-3/4.

### 1.5 T=1 I-block PCB (ISO 7816-3)

- **Findings**:
  - I-block: PCB bit 8 = 0, bit 7 = N(S), bit 6 = M (more data).
  - R-block: N(R) in PCB; S-blocks for IFS, WTX, etc.
  - LRC: XOR of NAD, PCB, LEN, information field.

---

## 2. Local project references

### 2.1 osmo-ccid-firmware (`osmo-ccid-firmware/`)

**Role**: Main reference for reader-side CCID behaviour and slot/command handling.

| Path | What we use |
|------|------------------|
| **ccid_common/ccid_proto.h** | Message types (PC_to_RDR_*, RDR_to_PC_*), structs (`ccid_header`, `ccid_pc_to_rdr_xfr_block`, `ccid_rdr_to_pc_*`), error codes (`CCID_ERR_*`), status masks (`CCID_ICC_STATUS_*`, `CCID_CMD_STATUS_*`). |
| **ccid_common/ccid_device.c** | Message builders (`ccid_gen_data_block_nr`, `ccid_gen_slot_status_nr`, `ccid_gen_parameters_t0_nr`/`t1_nr`), **gen_err_resp()** (command → response type: DataBlock vs SlotStatus vs Parameters vs Escape), **ccid_slot_send_unbusy()** (clear cmd_busy after send), Get/SetParameters decode/encode. |
| **ccid_common/ccid_slot_fsm.c** | Slot FSM: `icc_present`, `icc_powered`, `cmd_busy`; power-on async; **cmd_busy** cleared on card removal; slot ops (power_on, power_off, xfr_block_async, set_params, etc.). |
| **ccid_common/iso7816_fsm.c** | ISO 7816-3 FSM states (RESET, WAIT_ATR, IN_ATR, WAIT_TPDU, …), ATR parsing, PPS. |
| **ccid_common/iso7816_3.h** | Fi/Di tables, WT calculation, default WI, Fd/Dd; used for ATR/params. |

**Takeaways**:
- 3-state slot: NO_ICC / PRES_INACT / PRES_ACT; `get_icc_status()` returns `CCID_ICC_STATUS_*`.
- On error, respond with the **correct response type** for the command (DataBlock for IccPowerOn/XfrBlock/Secure; SlotStatus for GetSlotStatus/IccPowerOff/…; Parameters for Get/Set/ResetParameters; Escape for Escape).
- Parameters response: osmo uses `ccid_rdr_to_pc_parameters` with **header_in + abProtocolData** only (no separate bProtocolNum in struct); dwLength = size of t0 or t1. Our firmware sends bProtocolNum in byte 9 then abProtocolData; both are valid if the host accepts either.
- XfrBlock is handled **asynchronously** via `xfr_block_async`; we do it synchronously in one handler.

### 2.2 libccid — CCID driver (`CCID/`)

**Role**: Host-side behaviour: how commands are sent and responses parsed.

| Path | What we use |
|------|------------------|
| **src/commands.c** | **CmdPowerOn**: 10-byte command, response 10+ATR; **CmdXfrBlock** / **CCID_Transmit**: `cmd[0]=0x6F`, `i2dw(tx_length, cmd+1)`, `cmd[5]=slot`, `cmd[6]=bSeq`, `cmd[7]=bBWI`, `cmd[8..9]=wLevelParameter` (LE), `memcpy(cmd+10, tx_buffer, tx_length)` → **APDU starts at offset 10**. ReadPort waits for response with matching bSeq. **SetParameters**: `cmd[7]=protocol`, params at `cmd+10`. |
| **src/ifdhandler.c** | Uses CCID_Transmit for APDU exchange; checks bStatus (ICC status, command status). **IFDHSetProtocolParameters** handles protocol selection via PPS and SetParameters. |
| **src/towitoko/atr.c** | **ATR_GetDefaultProtocol** (lines 319-364): parses TD(i) bytes to find first offered protocol; defaults to T=0 if no TD found; checks TA2 for specific mode. |
| **src/towitoko/pps.c** | **PPS_Exchange**: sends PPS request, expects exact echo for success. |
| **README.md** | CCID/ICCD spec links; debug levels; voltage selection. |

**Takeaways**:
- Host **always** sends XfrBlock with 10-byte header then raw APDU; we must take APDU from `rx_buffer[10..]`.
- bSeq is incremented per command; we must **echo bSeq** in every response.
- SetParameters request: bProtocolNum at byte 7, protocol data at 10+; our response layout (header + bProtocolNum + abProtocolData) matches.

### 2.3 canokey-stm32 (`canokey-stm32/`)

**Role**: STM32 USB device with CCID interface (token-style).

| Path | What we use |
|------|------------------|
| **Src/usb.c** | Allocates CCID endpoint and interface (`EP_TABLE.ccid`, `IFACE_TABLE.ccid`). |
| **Src/main.c** | Integration of USB + CCID (actual CCID logic likely in canokey-core). |

**Takeaway**: Token presents CCID; our reader is a **slot reader** (card present/absent, slot FSM), so we follow osmo/CCID spec more closely than CanoKey for slot and command semantics.

### 2.4 GoKey, OpenSC, usbarmory, specter-diy

- **GoKey**: No CCID/XfrBlock hits in workspace; not used as CCID reference.
- **OpenSC**: Host/tools (pkcs15-tool, etc.) and some ISO 7816; not reader firmware.
- **usbarmory**: Not inspected for CCID.
- **specter-diy**, **docs/**: Specter-specific; not CCID reader reference.

---

## 3. SCARD_E_PROTO_MISMATCH (0x8010000F) Research

### 3.1 Definition and Causes

- **Definition** (from PC/SC): "The requested protocols are incompatible with the protocol currently in use with the smart card."
- **Generated by**: `pcscd` in `SCardConnect`, `SCardReconnect`, and `SCardTransmit` (see `PCSC/src/winscard.c`).

**Common Causes**:

| Cause | Description |
|-------|-------------|
| **ATR TD1 Parsing** | TD1 indicates card's first offered protocol. If missing, defaults to T=0. Mismatch if host expects different protocol. |
| **TA2 Specific Mode** | If TA2 present, card is locked to specific protocol. Host requesting different protocol causes mismatch. |
| **SetParameters Failure** | If reader rejects or mishandles SetParameters command, protocol cannot be established. |
| **PPS Negotiation Failure** | If card doesn't echo PPS request, negotiation fails. Reader may report wrong protocol. |
| **Truncated ATR** | Missing TD1 causes firmware to default to T=0 when card is actually T=1. |
| **Voltage Mismatch** | Card at wrong voltage may produce corrupted ATR, leading to wrong protocol detection. |

### 3.2 Protocol Selection Flow (libccid)

1. **PowerOn** → `CmdPowerOn` → receive ATR
2. **ATR Parsing** → `ATR_GetDefaultProtocol`:
   - Scan TD(i) bytes for protocol indicators
   - If TA2 present → specific mode (locked protocol)
   - If no TD found → default to T=0
3. **Protocol Check** → `IFDHSetProtocolParameters`:
   - Verify requested protocol in `dwProtocols` (CCID descriptor)
   - If not supported → return `IFD_ERROR_NOT_SUPPORTED`
4. **PPS Negotiation** (if not auto-PPS):
   - Send PPS request: `FF PPS0 PPS1 PCK`
   - Expect exact echo for success
   - Failure → retry or error
5. **SetParameters** → send `PC_to_RDR_SetParameters`:
   - Host sends protocol + parameters
   - Reader responds with current parameters
6. **XfrBlock** → APDU exchange begins

### 3.3 ATR_GetDefaultProtocol Logic (from atr.c)

```c
int ATR_GetDefaultProtocol(ATR_t * atr, int *protocol, int *availableProtocols) {
    *protocol = PROTOCOL_UNSET;
    
    for (i=0; i<ATR_MAX_PROTOCOLS; i++) {
        if (atr->ib[i][ATR_INTERFACE_BYTE_TD].present) {
            int T = atr->ib[i][ATR_INTERFACE_BYTE_TD].value & 0x0F;
            if (PROTOCOL_UNSET == *protocol)
                *protocol = T;  // First protocol found
        }
    }
    
    // Specific mode if TA2 present
    if (atr->ib[1][ATR_INTERFACE_BYTE_TA].present) {
        *protocol = atr->ib[1][ATR_INTERFACE_BYTE_TA].value & 0x0F;
    }
    
    // Default to T=0 if nothing found
    if (PROTOCOL_UNSET == *protocol) {
        *protocol = ATR_PROTOCOL_TYPE_T0;
    }
    
    return ATR_OK;
}
```

---

## 4. SetParameters Response Format (CCID Spec)

### 4.1 Message Layout

**Response (RDR_to_PC_Parameters, 0x82)**:

| Offset | Field | Size | Description |
|--------|-------|------|-------------|
| 0 | bMessageType | 1 | 0x82 |
| 1-4 | dwLength | 4 | 5 (T=0) or 7 (T=1), LSB first |
| 5 | bSlot | 1 | Slot number |
| 6 | bSeq | 1 | Sequence number (echo) |
| 7 | bStatus | 1 | Status byte |
| 8 | bError | 1 | Error code |
| 9 | bProtocolNum | 1 | 0x00 (T=0) or 0x01 (T=1) |
| 10+ | abProtocolData | var | 5 bytes (T=0) or 7 bytes (T=1) |

### 4.2 abProtocolData Layout

**T=0 (5 bytes)**:
| Offset | Field | Description |
|--------|-------|-------------|
| 0 | bmFindexDindex | Fi (bits 7-4), Di (bits 3-0) |
| 1 | bmTCCKST0 | Checksum (bit 0), Convention (bit 1) |
| 2 | bGuardTimeT0 | Extra guard time (0-254, 255=0) |
| 3 | bWaitingIntegerT0 | WI (Waiting Integer) |
| 4 | bClockStop | Clock stop support (0-3) |

**T=1 (7 bytes)**:
| Offset | Field | Description |
|--------|-------|-------------|
| 0 | bmFindexDindex | Fi (bits 7-4), Di (bits 3-0) |
| 1 | bmTCCKST1 | Checksum (bit 0), Convention (bit 1) |
| 2 | bGuardTimeT1 | Extra guard time |
| 3 | bWaitingIntegersT1 | BWI (bits 7-4), CWI (bits 3-0) |
| 4 | bClockStop | Clock stop support |
| 5 | bIFSC | Information Field Size Card |
| 6 | bNadValue | Node Address (usually 0x00) |

### 4.3 Common Mistakes

1. **dwLength mismatch**: Only counts abProtocolData, not bProtocolNum
2. **Missing bProtocolNum**: Byte 9 must contain protocol number
3. **Wrong field order**: bmFindexDindex must be first
4. **Convention bit**: Must match TS byte (0x3B=direct, 0x3F=inverse)

---

## 5. NotifySlotChange (Interrupt Endpoint)

### 5.1 Message Format

| Offset | Field | Description |
|--------|-------|-------------|
| 0 | bMessageType | 0x50 |
| 1 | bmSlotCCState | 2 bits per slot: bit 0=present, bit 1=changed |

**Single-slot example**:
- Card inserted: `[0x50, 0x03]` (present + changed)
- Card removed: `[0x50, 0x02]` (not present + changed)

### 5.2 Host Usage

- **Interrupt mode**: If interrupt endpoint present, libccid uses it for card events (preferred)
- **Polling fallback**: If no interrupt endpoint, pcscd polls with `GetSlotStatus` every 200-400ms

### 5.3 USB Descriptor Requirements

- Endpoint type: Interrupt (0x03)
- Direction: IN
- Max packet size: 8 bytes (standard)
- Interval: 24ms (full-speed) or 8ms (high-speed)

---

## 6. Voltage Handling

### 6.1 bVoltageSupport and bPowerSelect

| Value | Voltage |
|-------|---------|
| 0x01 | 5.0V (Class A) |
| 0x02 | 3.0V (Class B) |
| 0x04 | 1.8V (Class C) |
| 0x07 | All three |

**bPowerSelect in PC_to_RDR_IccPowerOn**:
- 0x00: Automatic (reader decides)
- 0x01: 5.0V
- 0x02: 3.0V
- 0x03: 1.8V

### 6.2 Automatic Voltage Selection

If `bPowerSelect = 0x00` (automatic), reader should try voltages in sequence until valid ATR:
1. Try lowest supported voltage (usually 1.8V)
2. If no ATR, deactivate and try next voltage
3. Continue until ATR received or all voltages exhausted

### 6.3 Voltage Mismatch Consequences

| Condition | Effect |
|-----------|--------|
| 3V card @ 5V | Risk of damage or thermal issues |
| 5V card @ 3V | Card may not boot, appears mute |
| Wrong voltage | Corrupted ATR → protocol detection failure |

---

## 7. Alignment checklist (from research)

| Topic | Spec / reference | Our implementation |
|-------|------------------|--------------------|
| Header size | 10 bytes (0–9), then payload | `CCID_HEADER_SIZE = 10` |
| XfrBlock APDU offset | libccid: `cmd+10` | `rx_buffer[CCID_HEADER_SIZE..]` |
| Response types per command | osmo `gen_err_resp()` | `send_err_resp()` maps command → DataBlock/SlotStatus/Parameters/Escape |
| Slot state | NO_ICC / PRES_INACT / PRES_ACT | `SlotState::Absent` / `PresentInactive` / `PresentActive` |
| bStatus | (cmd_status << 6) \| icc_status | `build_status(cmd_status, icc_status)` |
| Error codes | osmo `ccid_proto.h` | `CCID_ERR_CMD_SLOT_BUSY`, `CCID_ERR_*` |
| T=0 GET RESPONSE / 6C XX | ISO 7816-3/4, Stack Overflow | `transmit_apdu_t0()`: 61 XX → GET RESPONSE; 6C XX → re-send with P3=XX |
| T=1 I-block | N(S) bit 7, M bit 6, LRC | `t1_engine.rs`: PCB, LRC, chaining |
| Short APDU only | dwFeatures bit 17; no extended APDU | Reject dwLength > 261; descriptor 0x000207B2 (Omnikey 3021 compatible) |
| Interrupt IN | NotifySlotChange | Interrupt EP in config; send slot change when applicable |
| Protocol from ATR | TD1 low nibble, default T=0 | `detect_protocol_from_atr()` in smartcard.rs |
| SetParameters response | Header + bProtocolNum + abProtocolData | `handle_set_parameters()` in ccid.rs |

---

## 8. References quick list

- [USB CCID Rev 1.1 PDF](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf)
- [SpringCore CCID over USB](https://docs.springcard.com/books/SpringCore/Host_Interfaces/USB/CCID_(PCSC))
- [ccid.apdu.fr](https://ccid.apdu.fr/) — driver, extended APDU, supported readers
- [osmo-ccid-firmware (GitHub)](https://github.com/osmocom/osmo-ccid-firmware) — reader firmware reference
- Local: `osmo-ccid-firmware/ccid_common/` (ccid_proto, ccid_device, ccid_slot_fsm, iso7816_*)
- Local: `CCID/src/commands.c`, `ifdhandler.c`, `towitoko/atr.c` (host command layout, ATR parsing, bSeq)
- [ATR Parser](https://smartcard-atr.apdu.fr/) — online ATR analysis tool

Use this together with **VERIFICATION.md** for spec alignment and pre-test checks.
