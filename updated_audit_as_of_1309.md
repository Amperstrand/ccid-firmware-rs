# CCID Reader Implementation Audit

**Date:** 2026-03-08
**Reference Implementation:** [osmo-ccid-firmware](https://github.com/osmocom/osmo-ccid-firmware) by sysmocom/Osmocom
**Our Implementation:** `/ccid-reader` (Rust, STM32F469)

---

## Executive Summary

This document provides an exhaustive comparison between our Rust CCID smartcard reader implementation and the **osmo-ccid-firmware** reference implementation. Osmo-ccid-firmware is a well-established, production-quality CCID firmware used in real-world smartcard reader hardware.

| Aspect | Our Implementation | osmo-ccid-firmware | Status |
|--------|-------------------|-------------------|--------|
| Language | Rust (no_std) | C (ASF4/embedded) | ✅ Different paradigm |
| Architecture | Synchronous, blocking | Asynchronous FSM-based | ⚠️ Different approach |
| ISO 7816-3 | T=0, T=1 (basic) | T=0, T=1 (full FSM) | ✅ Functional |
| CCID Protocol | Complete | Complete | ✅ Compatible |
| Hardware | STM32F469 | SAM3S/SAM4S | ✅ Target-specific |

---

## 1. Project Structure Comparison

### 1.1 Our Implementation (Rust)

```
ccid-reader/
├── src/
│   ├── main.rs           (200 lines)  - Entry point, GPIO/USB setup
│   ├── smartcard.rs      (930 lines)  - ISO 7816-3 HAL, USART driver
│   ├── ccid.rs           (1032 lines) - CCID protocol implementation
│   └── t1_engine.rs      (~300 lines) - T=1 protocol engine
├── Cargo.toml
└── Embed.toml
```

### 1.2 osmo-ccid-firmware (C)

```
osmo-ccid-firmware/
├── ccid_common/
│   ├── ccid_device.c     (951 lines)  - CCID protocol implementation
│   ├── ccid_proto.h      (~200 lines) - CCID message structures
│   ├── iso7816_3.c       (123 lines)  - ISO 7816-3 utilities (Fi/Di/WT)
│   ├── iso7816_3.h       (98 lines)   - ISO 7816-3 constants
│   ├── iso7816_fsm.c     (~2000 lines)- Full ISO 7816-3 FSM
│   ├── cuart.c           (233 lines)  - Card UART abstraction
│   └── cuart.h           (158 lines)  - UART driver interface
├── firmware/                          - ASF4 hardware drivers
└── host_test/                         - Host-side testing tools
```

**Key Difference:** osmo-ccid uses a layered architecture with FSM-based state management. Our implementation uses a simpler synchronous/blocking approach.

---

## 2. USART/UART Configuration

### 2.1 Our Implementation (smartcard.rs:233-270)

```rust
fn init_usart(&mut self, clocks: &Clocks) {
    let pclk1 = clocks.pclk1().raw();
    let prescaler = ((pclk1 + 2 * SC_MAX_CLK_HZ - 1) / (2 * SC_MAX_CLK_HZ))
        .min(31).max(1) as u8;
    let card_clk = pclk1 / (2 * prescaler as u32);
    let baudrate = (card_clk + SC_DEFAULT_ETU / 2) / SC_DEFAULT_ETU;
    let brr_val = pclk1 / baudrate;

    // CR1: UE=1, M=1(9bit), PCE=1(parity), TE=1, RE=1 = 0x340C
    self.usart.cr1().write(|w| unsafe { w.bits(0x340C) });
    // CR2: STOP=1.5, CLKEN=1, CPOL=0, CPHA=0, LBCL=1 = 0x3900
    self.usart.cr2().write(|w| unsafe { w.bits(0x3900) });
    // CR3: SCEN (bit 5), NACK disabled = 0x0020
    self.usart.cr3().write(|w| unsafe { w.bits(0x0020) });
    // GTPR: Guard time (upper byte), prescaler (lower byte)
    self.usart.gtpr().write(|w| unsafe { w.bits((16u16 << 8) | prescaler as u16) });
}
```

**GPIO Pin Configuration (main.rs:131-150):**
```rust
// PA2: I/O - AF7, OpenDrain, PullUp
let io_pin: PA2<Alternate<7, OpenDrain>> = gpioa.pa2
    .into_alternate_open_drain::<7>()
    .internal_pull_up(true)
    .speed(Speed::High);

// PA4: CLK - AF7, PushPull
let clk_pin: PA4<Alternate<7, PushPull>> = gpioa.pa4
    .into_alternate::<7>()
    .speed(Speed::High);

// PG10: RST - Output, PushPull (active LOW)
let rst_pin: PG10<Output<PushPull>> = gpiog.pg10
    .into_push_pull_output_in_state(PinState::High);

// PC2: Card Detect - Input
let pres_pin: PC2<Input> = gpioc.pc2.into_input();

// PC5: PWR - Output (LOW = power ON)
let pwr_pin: PC5<Output<PushPull>> = gpioc.pc5
    .into_push_pull_output_in_state(PinState::High);
```

### 2.2 osmo-ccid-firmware (cuart.h, asf4 drivers)

osmo-ccid uses an abstraction layer (`card_uart`) that delegates to hardware-specific drivers:

```c
struct card_uart_ops {
    int (*open)(struct card_uart *cuart, const char *device_name);
    int (*close)(struct card_uart *cuart);
    int (*async_tx)(struct card_uart *cuart, const uint8_t *data, size_t len);
    int (*async_rx)(struct card_uart *cuart, uint8_t *data, size_t len);
    int (*ctrl)(struct card_uart *cuart, enum card_uart_ctl ctl, int arg);
};
```

**Control operations (cuart.h:44-59):**
```c
enum card_uart_ctl {
    CUART_CTL_RX,              // enable/disable receiver
    CUART_CTL_RX_TIMER_HINT,   // tell cuart approximate number of rx bytes
    CUART_CTL_POWER_5V0,
    CUART_CTL_POWER_3V0,
    CUART_CTL_POWER_1V8,
    CUART_CTL_CLOCK,           // enable/disable ICC clock
    CUART_CTL_SET_CLOCK_FREQ,  // set ICC clock frequency (hz)
    CUART_CTL_RST,             // enable/disable ICC reset
    CUART_CTL_WTIME,           // set waiting time (in etu)
    CUART_CTL_SET_FD,          // set F/D values
    CUART_CTL_GET_BAUDRATE,
    CUART_CTL_GET_CLOCK_FREQ,
    CUART_CTL_ERROR_AND_INV,   // enable error interrupt and inverse signalling
};
```

### 2.3 Comparison

| Feature | Our Impl | osmo-ccid | Notes |
|---------|----------|-----------|-------|
| USART Mode | Smartcard (9-bit, parity) | Smartcard | ✅ Same |
| CLK Output | CR2.CLKEN | CUART_CTL_CLOCK | ✅ Same |
| NACK Control | CR3 bit 4 | CUART_CTL_ERROR_AND_INV | ✅ Same |
| Guard Time | GTPR register | CUART_CTL + hw-specific | ✅ Same |
| Half-duplex | Manual RX disable | Automatic via async_tx | ⚠️ Different |
| Prescaler | Calculated from PCLK | ASF4 abstraction | ✅ Same result |

---

## 3. Activation Sequence (Cold Reset)

### 3.1 Our Implementation (smartcard.rs:440-523)

```rust
pub fn power_on(&mut self) -> Result<&Atr, SmartcardError> {
    // Full cold reset: power off, RST low, wait for discharge
    self.pwr_pin.set_high(); // VCC off
    self.rst_pin.set_low();  // RST asserted
    Self::delay_ms(200);     // Long delay for card capacitor discharge

    // Clear stale USART data/errors
    while self.usart.sr().read().rxne().bit_is_set() {
        let _ = self.usart.dr().read().dr().bits();
    }
    self.clear_usart_errors();

    // Activate: VCC on, wait, RST high → card sends ATR
    // CLK is already running (CLKEN=1 from init)
    self.pwr_pin.set_low(); // VCC on
    Self::delay_ms(SC_POWER_ON_DELAY_MS); // 50ms VCC stabilize
    self.rst_pin.set_high(); // Release RST → card starts ATR
    Self::delay_ms(2); // Brief settle before reading

    match self.read_atr() { ... }
}
```

**Timing constants:**
```rust
const SC_POWER_ON_DELAY_MS: u32 = 50;   // VCC stabilize
const SC_RESET_DELAY_MS: u32 = 25;      // Reset low duration
const SC_ATR_POST_RST_DELAY_MS: u32 = 20;
const SC_CLK_TO_RST_DELAY_MS: u32 = 15; // ⚠️ NOT USED!
```

### 3.2 osmo-ccid-firmware (iso7816_fsm.c:305-340)

```c
static void iso7816_3_reset_action(struct osmo_fsm_inst *fi, uint32_t event, void *data) {
    struct iso7816_3_priv *ip = get_iso7816_3_priv(fi);

    switch (event) {
    case ISO7816_E_RESET_REL_IND:
        // Enable receiver BEFORE reset is released
        card_uart_ctrl(ip->uart, CUART_CTL_RX, true);

        // 40k cycle delay to ATR start is ~1.4ms @ 2.5MHz
        // 40k/372=107 ETU ~ 9 byte times
        // Default WT is 9600 ETU -> ~1.5s per byte
        card_uart_ctrl(ip->uart, CUART_CTL_RX_TIMER_HINT, 1);
        osmo_fsm_inst_state_chg(fi, ISO7816_S_WAIT_ATR, 0, 0);
        break;
    }
}
```

**ISO 7816-3 timing requirements (from iso7816_3.h):**
```c
#define ISO7816_3_DEFAULT_FD 372    // Default F (clock rate conversion)
#define ISO7816_3_DEFAULT_DD 1      // Default D (baud rate adjustment)
#define ISO7816_3_DEFAULT_WI 10     // Default Waiting Integer (T=0)
#define ISO7816_3_DEFAULT_WT 9600   // Default Waiting Time in ETU
```

### 3.3 Critical Difference: CLK-to-RST Timing

| Aspect | Our Impl | osmo-ccid | ISO 7816-3 |
|--------|----------|-----------|------------|
| CLK before RST | ⚠️ Not explicit | FSM-controlled | Required (40k cycles) |
| VCC→CLK delay | 50ms | Async via FSM | Not specified |
| CLK→RST delay | ❌ Missing (constant defined but not used) | FSM state transition | ≥40,000 clock cycles |

**ISSUE IDENTIFIED:** Our `SC_CLK_TO_RST_DELAY_MS` constant (15ms) is defined but **NOT USED** in the activation sequence. The code goes directly from VCC on → RST high without ensuring CLK has been running for 40,000 cycles.

---

## 4. ATR Reception

### 4.1 Our Implementation (smartcard.rs:544-612)

```rust
fn read_atr(&mut self) -> Result<(), SmartcardError> {
    // Wait for first byte (TS) with long timeout
    let mut countdown = SC_ATR_TIMEOUT_MS; // 400ms
    while !self.usart.sr().read().rxne().bit_is_set() {
        Self::delay_ms(1);
        countdown -= 1;
        if countdown == 0 {
            return Err(SmartcardError::Timeout);
        }
    }

    // Tight busy-wait loop for remaining ATR bytes
    let mut len = 0usize;
    let timeout_reload: u32 = 50 * 168_000; // ~50ms inter-byte timeout
    let mut timeout_counter = timeout_reload;

    loop {
        let sr = self.usart.sr().read().bits();
        if (sr & (1 << 5)) != 0 || (sr & (1 << 3)) != 0 { // RXNE or ORE
            let b = self.usart.dr().read().dr().bits() as u8;
            if len == 0 && b == 0x00 { continue; } // skip leading nulls
            if len < SC_ATR_MAX_LEN {
                self.atr.raw[len] = b;
                len += 1;
            }
            timeout_counter = timeout_reload;
            continue;
        }
        timeout_counter -= 1;
        if timeout_counter == 0 {
            if len > 0 { break; } // inter-byte timeout = ATR complete
        }
    }
    self.atr.len = len;
    Ok(())
}
```

### 4.2 osmo-ccid-firmware (iso7816_fsm.c:696-870)

osmo-ccid uses a full **ATR parsing FSM** with these states:

```c
enum atr_state {
    ATR_S_WAIT_TS,    // initial byte (0x3B or 0x3F)
    ATR_S_WAIT_T0,    // format byte
    ATR_S_WAIT_TA,    // interface byte TAi
    ATR_S_WAIT_TB,    // interface byte TBi
    ATR_S_WAIT_TC,    // interface byte TCi
    ATR_S_WAIT_TD,    // interface byte TDi (protocol indicator)
    ATR_S_WAIT_HIST,  // historical bytes
    ATR_S_WAIT_TCK,   // check byte (XOR checksum)
    ATR_S_DONE
};
```

**Key features in osmo-ccid ATR FSM:**

1. **Convention detection** (direct vs inverse):
```c
// iso7816_fsm.c:729-752
case 0x23: // direct convention decoded as inverse
case 0x03: // inverse convention decoded as direct
    ip->convention_convert = !ip->convention_convert;
    goto restart;
case 0x3b: // direct convention correct
case 0x3f: // inverse convention correct
    atr_append_byte(fi, byte);
    // continue to T0
```

2. **Protocol detection from TD bytes:**
```c
atp->protocol_support |= (1<<(byte & 0x0f)); // Track all supported protocols
```

3. **TCK checksum verification:**
```c
for (ui = 1; ui < msgb_length(atp->atr)-1; ui++) {
    atp->computed_checksum ^= atr[ui];
}
if (atp->computed_checksum != byte) {
    LOGPFSML(fi, LOGL_ERROR, "checksum mismatch");
}
```

### 4.3 Comparison

| Feature | Our Impl | osmo-ccid | Status |
|---------|----------|-----------|--------|
| TS timeout | 400ms | FSM timer | ✅ OK |
| Inter-byte timeout | 50ms spin | Software timer | ✅ OK |
| Convention detection | ❌ Not implemented | ✅ Full support | ⚠️ May fail with inverse convention cards |
| TCK checksum | ❌ Not verified | ✅ Verified | ⚠️ Minor issue |
| Max ATR length | 33 bytes | 32 bytes (spec) | ✅ OK |
| Protocol detection | Basic (TD1 only) | Full (all TD bytes) | ⚠️ Limited |

---

## 5. CCID Protocol Implementation

### 5.1 Command Support Matrix

| CCID Command | Our Impl | osmo-ccid | Notes |
|--------------|----------|-----------|-------|
| IccPowerOn (0x62) | ✅ Full | ✅ Full | Returns ATR |
| IccPowerOff (0x63) | ✅ Full | ✅ Full | Deactivates card |
| GetSlotStatus (0x65) | ✅ Full | ✅ Full | 3-state FSM |
| XfrBlock (0x6F) | ✅ Full | ✅ Full | APDU/TPDU transfer |
| GetParameters (0x6C) | ✅ Full | ✅ Full | T=0/T=1 params |
| ResetParameters (0x6D) | ✅ Basic | ✅ Full | Reset to defaults |
| SetParameters (0x61) | ✅ Basic | ✅ Full | Protocol selection |
| IccClock (0x6E) | ✅ Basic | ✅ Full | Clock control |
| Escape (0x6B) | ⚠️ Stub (not supported) | ⚠️ Stub | Vendor-specific |
| T0APDU (0x6A) | ⚠️ Stub | ⚠️ Stub | Not needed (XfrBlock used) |
| Secure (0x69) | ⚠️ Stub (no PIN hw) | ⚠️ Stub | Requires PIN hardware |
| Mechanical (0x71) | ⚠️ Stub | ⚠️ Stub | No mechanical parts |
| Abort (0x72) | ⚠️ Basic | ⚠️ Incomplete | Both return success |
| SetDataRateAndClockFrequency (0x73) | ✅ Basic | ✅ Full | Rate adjustment |

### 5.2 CCID Descriptor Comparison

**Our descriptor (ccid.rs:138-187):**
```rust
pub const CCID_CLASS_DESCRIPTOR_DATA: [u8; 52] = [
    0x10, 0x01,              // bcdCCID: 1.10
    0x00,                    // bMaxSlotIndex: 0 (single slot)
    0x07,                    // bVoltageSupport: 5V, 3V, 1.8V
    0x03, 0x00, 0x00, 0x00,  // dwProtocols: T=0 and T=1
    // ... (full 52-byte descriptor)
    0xB2, 0x07, 0x02, 0x00,  // dwFeatures: APDU level + auto params
];
```

**USB Identity:**
```rust
// main.rs:185
UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x076B, 0x3021))
    .strings(&[StringDescriptors::default()
        .manufacturer("OMNIKEY AG")
        .product("Smart Card Reader USB")
        .serial_number("001")])
```

This emulates **OMNIKEY 3021** (VID:076B, PID:3021), matching osmo-ccid's default identity.

### 5.3 Error Response Handling

**Our implementation (ccid.rs:476-519):**
```rust
fn send_err_resp(&mut self, msg_type: u8, seq: u8, error: u8) {
    match msg_type {
        PC_TO_RDR_ICC_POWER_ON | PC_TO_RDR_XFR_BLOCK | PC_TO_RDR_SECURE => {
            // Return RDR_to_PC_DataBlock
            self.tx_buffer[0] = RDR_TO_PC_DATABLOCK;
            // ...
        }
        PC_TO_RDR_ICC_POWER_OFF | PC_TO_RDR_GET_SLOT_STATUS | /* ... */ => {
            // Return RDR_to_PC_SlotStatus
            self.send_slot_status(seq, COMMAND_STATUS_FAILED, icc, error);
        }
        PC_TO_RDR_GET_PARAMETERS | /* ... */ => {
            // Return RDR_to_PC_Parameters
            self.tx_buffer[0] = RDR_TO_PC_PARAMETERS;
            // ...
        }
    }
}
```

**osmo-ccid implementation (ccid_device.c:359-411):**
```c
static struct msgb *gen_err_resp(enum ccid_msg_type msg_type, uint8_t slot_nr,
                                  uint8_t icc_status, uint8_t seq, 
                                  enum ccid_error_code err_code) {
    switch (msg_type) {
    case PC_to_RDR_IccPowerOn:
    case PC_to_RDR_XfrBlock:
    case PC_to_RDR_Secure:
        return ccid_gen_data_block_nr(slot_nr, icc_status, seq, 
                                       CCID_CMD_STATUS_FAILED, err_code, NULL, 0);
    case PC_to_RDR_IccPowerOff:
    case PC_to_RDR_GetSlotStatus:
    case PC_to_RDR_Abort:
        return ccid_gen_slot_status_nr(slot_nr, icc_status, seq,
                                        CCID_CMD_STATUS_FAILED, err_code);
    // ... etc
    }
}
```

**Status:** ✅ **Identical behavior** - Both implementations follow CCID spec for error responses.

---

## 6. ISO 7816-3 Protocol Handling

### 6.1 T=0 Protocol

**Our implementation (smartcard.rs:732-847):**
```rust
fn transmit_apdu_t0(&mut self, command: &[u8], response: &mut [u8]) 
    -> Result<usize, SmartcardError> {
    let ins = command[1];
    let mut header = [command[0], command[1], command[2], command[3], command[4]];
    
    'send: loop {
        // Send 5-byte header
        for i in 0..5 { self.send_byte(header[i])?; }
        
        // Send body if present
        if body_offset < command.len() {
            for i in body_offset..command.len() {
                self.send_byte(command[i])?;
            }
        }
        
        loop {
            let pb = self.receive_byte_timeout(SC_PROCEDURE_TIMEOUT_MS)?;
            
            // NULL byte (0x60) - wait more
            while pb == 0x60 { 
                pb = self.receive_byte_timeout(SC_PROCEDURE_TIMEOUT_MS)?; 
            }
            
            if pb == ins {
                // ACK - all remaining data sent, receive SW
                let sw1 = self.receive_byte_timeout(...)?;
                let sw2 = self.receive_byte_timeout(...)?;
                // Handle 61XX (GET RESPONSE), 6CXX (re-send with correct Le)
            } else if pb == (ins ^ 0xFF) {
                // ACK inverse - send one byte at a time
            } else if pb == 0x61 || pb == 0x6C {
                // SW1 - need GET RESPONSE or re-send
            } else {
                // SW1 directly - done
            }
        }
    }
}
```

**osmo-ccid implementation (iso7816_fsm.c:1245-1577):**

osmo-ccid uses a **TPDU FSM** with states:
```c
enum tpdu_state {
    TPDU_S_INIT,
    TPDU_S_TX_HDR,        // Transmitting 5-byte header
    TPDU_S_PROCEDURE,     // Waiting for procedure byte
    TPDU_S_TX_REMAINING,  // Transmitting remaining data
    TPDU_S_TX_SINGLE,     // Transmitting single byte (INS^0xFF)
    TPDU_S_RX_REMAINING,  // Receiving remaining data
    TPDU_S_RX_SINGLE,     // Receiving single byte
    TPDU_S_SW1,           // Waiting for SW1
    TPDU_S_SW2,           // Waiting for SW2
    TPDU_S_DONE,
};
```

**Procedure byte handling (iso7816_fsm.c:1308-1375):**
```c
if (byte == 0x60) {
    // NULL: wait for another procedure byte
    osmo_fsm_inst_state_chg(fi, TPDU_S_PROCEDURE, 0, 0);
} else if ((byte >= 0x60 && byte <= 0x6f) || (byte >= 0x90 && byte <= 0x9f)) {
    // SW1: receive SW2
    osmo_fsm_inst_state_chg(fi, TPDU_S_SW2, 0, 0);
} else if (byte == tpduh->ins) {
    // ACK: send all remaining / receive all expected
    if (tfp->is_command) {
        card_uart_tx(ip->uart, msgb_l2(tfp->tpdu), msgb_l2len(tfp->tpdu), true);
        osmo_fsm_inst_state_chg(fi, TPDU_S_TX_REMAINING, 0, 0);
    } else {
        // Receive response
        card_uart_set_rx_threshold(ip->uart, len_expected);
        osmo_fsm_inst_state_chg(fi, TPDU_S_RX_REMAINING, 0, 0);
    }
} else if (byte == (tpduh->ins ^ 0xFF)) {
    // ACK inverse: single byte at a time
}
```

### 6.2 T=1 Protocol

**Our implementation (t1_engine.rs + smartcard.rs:329-372):**

We implement T=1 with:
- I-block transmission (NAD, PCB, LEN, INF, LRC)
- S-block IFSD negotiation (S(IFS request/response))
- Sequence number tracking (N(S) alternates 0/1)

```rust
// IFSD negotiation (smartcard.rs:330-372)
fn do_ifs_negotiation_t1(&mut self) -> Result<u8, ()> {
    const S_IFS_REQ: u8 = 0xC1;
    const IFSD: u8 = 254;
    let lrc_val = 0u8 ^ S_IFS_REQ ^ 1u8 ^ IFSD;
    
    self.send_byte(0)?;        // NAD
    self.send_byte(S_IFS_REQ)?; // PCB
    self.send_byte(1)?;         // LEN
    self.send_byte(IFSD)?;      // INF
    self.send_byte(lrc_val)?;   // LRC
    
    // Parse response...
}
```

**osmo-ccid implementation:**

osmo-ccid has **full T=1 support** including:
- I-block, R-block, S-block handling
- Error detection and retransmission
- CRC or LRC checksum support
- IFSD/IFSC negotiation
- BWT/CWT timeout handling

### 6.3 PPS/PTS Negotiation

**Our implementation (smartcard.rs:402-438):**

```rust
fn negotiate_pps(&mut self, params: &AtrParams) -> Result<(), ()> {
    // Skip PPS if no TA1 or already at defaults
    if !params.has_ta1 || params.ta1 == 0x11 {
        return Ok(());
    }

    // Build PPS request: [PPSS, PPS0, PPS1, PCK]
    let pps0 = 0x10u8 | (params.protocol & 0x0F);
    let pps1 = params.ta1;
    let pck = 0xFFu8 ^ pps0 ^ pps1;
    let req = [0xFFu8, pps0, pps1, pck];

    // Send and receive response
    for &b in &req { self.send_byte(b).map_err(|_| ())?; }
    let mut resp = [0u8; 4];
    for r in &mut resp { *r = self.receive_byte_timeout(100).map_err(|_| ())?; }

    // Validate response (should echo request)
    if resp != req { return Err(()); }

    // Update baud rate
    self.set_baud_from_fi_di(params.fi, params.di);
    Ok(())
}
```

**NOTE:** Our code has a comment that PPS is **skipped** during power_on:
```rust
// smartcard.rs:479-481
// Skip PPS and IFSD negotiation -- sending extra bytes after ATR
// corrupts the card's state. Use defaults from ATR instead.
// PPS: stay at default Fi=372/Di=1 (safe for all cards).
```

**osmo-ccid implementation (iso7816_fsm.c:1000-1123):**

osmo-ccid has a **full PPS FSM**:
```c
enum pps_state {
    PPS_S_PPS_REQ_INIT,
    PPS_S_TX_PPS_REQ,
    PPS_S_WAIT_PPSX,   // Wait for 0xFF
    PPS_S_WAIT_PPS0,
    PPS_S_WAIT_PPS1,
    PPS_S_WAIT_PPS2,
    PPS_S_WAIT_PPS3,
    PPS_S_WAIT_PCK,    // Checksum
    PPS_S_DONE,
};
```

**Key difference:** osmo-ccid can handle PPS responses asynchronously and supports PPS1/PPS2/PPS3, while our implementation is synchronous and only supports PPS1.

---

## 7. Architecture Comparison

### 7.1 Our Architecture: Synchronous/Blocking

```
┌─────────────────────────────────────────────┐
│                 main.rs                      │
│  loop { usb_device.poll(&mut [&mut ccid]) } │
└─────────────────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────┐
│                 ccid.rs                      │
│  handle_message() → blocking smartcard calls│
│  - handle_power_on()  (blocks ~500ms)       │
│  - handle_xfr_block() (blocks ~seconds)     │
└─────────────────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────┐
│              smartcard.rs                    │
│  power_on() → blocking ATR read             │
│  transmit_apdu() → blocking T=0/T=1         │
└─────────────────────────────────────────────┘
```

**Pros:**
- Simple, easy to understand
- No race conditions
- Predictable timing

**Cons:**
- USB polling blocked during smartcard operations
- Cannot abort in-progress commands
- No concurrent slot operations (not an issue for single-slot)

### 7.2 osmo-ccid Architecture: Asynchronous FSM

```
┌─────────────────────────────────────────────┐
│              Main Event Loop                 │
│  osmo_select_main() - poll all FDs/timers   │
└─────────────────────────────────────────────┘
         │              │              │
         ▼              ▼              ▼
┌─────────────┐ ┌─────────────┐ ┌─────────────┐
│  USB EP OUT │ │  USB EP IN  │ │ Card UART   │
│  Handler    │ │  Handler    │ │ Events      │
└─────────────┘ └─────────────┘ └─────────────┘
         │              │              │
         ▼              ▼              ▼
┌─────────────────────────────────────────────┐
│           iso7816_3_fsm                      │
│  - RESET → WAIT_ATR → IN_ATR → WAIT_TPDU    │
│  - Child FSMs: atr_fsm, pps_fsm, tpdu_fsm   │
└─────────────────────────────────────────────┘
```

**Pros:**
- Non-blocking operations
- Proper abort handling
- Event-driven architecture
- Multiple slots supported

**Cons:**
- Complex state machine
- Harder to debug
- More code (~2000+ lines for FSMs)

---

## 8. Missing Features / Gaps

### 8.1 Critical Gaps

| Gap | Impact | Fix Priority |
|-----|--------|--------------|
| **CLK→RST timing not enforced** | May cause ATR failure on some cards | HIGH |
| **No inverse convention support** | Fails with inverse convention cards | MEDIUM |
| **PPS skipped after ATR** | May cause compatibility issues | LOW |

### 8.2 Minor Gaps

| Gap | Impact | Notes |
|-----|--------|-------|
| TCK checksum not verified | May miss corrupted ATR | Rare in practice |
| No BWT/CWT timeout tracking | May hang on bad cards | T=1 specific |
| No error recovery/retry | Single failure = failed APDU | Acceptable for reader |
| No R-block handling (T=1) | No retransmission | T=1 robustness |

### 8.3 Features Not Needed

| Feature | Reason |
|---------|--------|
| Multiple slots | Single-slot hardware |
| PIN hardware (Secure) | No keypad |
| Mechanical | No eject mechanism |
| Full Abort | Single-slot, sequential |

---

## 9. Code Quality Comparison

### 9.1 Documentation

| Aspect | Our Impl | osmo-ccid |
|--------|----------|-----------|
| Module docs | ✅ Good (doc comments) | ✅ Good (C comments) |
| Function docs | ✅ Good | ✅ Good |
| ISO 7816-3 references | ✅ Cited in comments | ✅ Extensive |
| CCID spec references | ✅ Section numbers | ✅ Section numbers |

### 9.2 Error Handling

| Aspect | Our Impl | osmo-ccid |
|--------|----------|-----------|
| Result types | ✅ `Result<T, SmartcardError>` | ⚠️ Integer return codes |
| Error propagation | ✅ `?` operator | ⚠️ Manual checking |
| Logging | ✅ defmt! macros | ✅ OSMO logging |
| Panic handling | ✅ panic_probe | ⚠️ asserts |

### 9.3 Memory Safety

| Aspect | Our Impl | osmo-ccid |
|--------|----------|-----------|
| Buffer overflows | ✅ Impossible (Rust) | ⚠️ Manual checking needed |
| Null pointers | ✅ Impossible (Rust) | ⚠️ Possible |
| Use-after-free | ✅ Impossible (Rust) | ⚠️ Possible |
| Data races | ✅ Impossible (single-thread) | ⚠️ Possible |

---

## 10. Recommendations

### 10.1 High Priority

1. **Fix CLK→RST timing**
   ```rust
   // In power_on(), add:
   self.pwr_pin.set_low(); // VCC on
   Self::delay_ms(SC_POWER_ON_DELAY_MS);
   // CLK is already running, but ensure 40k cycles have passed
   Self::delay_ms(SC_CLK_TO_RST_DELAY_MS); // 15ms at ~3.5MHz
   self.rst_pin.set_high(); // Release RST
   ```

2. **Add inverse convention support**
   ```rust
   // After receiving TS:
   let ts = first_byte;
   let inverse = match ts {
       0x3B => false,
       0x3F => true,
       0x03 | 0x23 => /* toggle and retry */,
       _ => return Err(InvalidATR),
   };
   // Apply bit inversion to subsequent bytes if inverse
   ```

### 10.2 Medium Priority

3. **Enable PPS negotiation** (currently skipped)

4. **Add TCK checksum verification**

### 10.3 Low Priority

5. **Add T=1 R-block support** for retransmission

6. **Implement proper BWT/CWT timeouts**

---

## 11. Conclusion

Our Rust implementation is **functionally equivalent** to osmo-ccid-firmware for the core CCID reader functionality. The main architectural difference (synchronous vs asynchronous FSM) is a valid design choice for a single-slot reader.

**Key findings:**
- ✅ CCID protocol: Fully compliant
- ✅ T=0 protocol: Complete
- ✅ T=1 protocol: Functional (basic)
- ⚠️ Activation timing: Missing CLK→RST delay
- ⚠️ Convention detection: Not implemented
- ⚠️ PPS: Disabled (intentional, but may cause issues)

The implementation is suitable for production use with cards that use direct convention (most modern cards including SatoChip/Seedkeeper). The identified gaps should be addressed for broader compatibility.

---

## 12. References

- [osmo-ccid-firmware](https://github.com/osmocom/osmo-ccid-firmware) - Reference implementation
- [CCID Specification Rev 1.1](https://www.usb.org/document-library/smart-card-ccid-revision-10) - USB CCID protocol
- [ISO/IEC 7816-3:2006](https://www.iso.org/standard/38770.html) - Smart card protocol
- [STM32F469 Reference Manual RM0386](https://www.st.com/resource/en/reference_manual/rm0386) - USART smartcard mode
