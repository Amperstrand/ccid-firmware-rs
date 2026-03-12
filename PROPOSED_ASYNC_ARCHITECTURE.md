# Proposed Async Architecture for CCID Reader

**Status:** PROPOSED (Not Implemented)
**Decision:** Staying synchronous for now
**Date:** 2026-03-08

---

## Executive Summary

This document describes an alternative asynchronous architecture for the CCID reader firmware. While this approach offers benefits for multi-slot readers and abort support, we have decided to **keep the current synchronous architecture** for our single-slot reader because:

1. CCID protocol is inherently sequential (host waits for response)
2. Simpler code is easier to maintain and debug
3. No concurrent slot operations needed
4. Rust's safety guarantees make blocking code safe

This document exists as a reference for future consideration or for developers building multi-slot readers.

---

## 1. Current Architecture (Synchronous)

### 1.1 Execution Model

```
┌─────────────────────────────────────────────────────────────────┐
│                        Main Loop                                 │
│                                                                  │
│  loop {                                                          │
│      usb_device.poll(&mut [&mut ccid_class]);                   │
│      // Blocks internally during smartcard operations           │
│  }                                                               │
│                                                                  │
│  Timeline:                                                       │
│  [USB RX] ──► [Process] ──► [Smartcard Op (BLOCKS)] ──► [USB TX]│
│                            ~~~~~~~~~~~~~~~~                      │
│                            500ms - 5s blocked                    │
└─────────────────────────────────────────────────────────────────┘
```

### 1.2 Characteristics

| Aspect | Behavior |
|--------|----------|
| USB responsiveness | Blocked during smartcard operations |
| Abort support | Not possible (blocked in smartcard code) |
| Code complexity | Low (linear execution) |
| Memory usage | Minimal (no task stacks) |
| Debugging | Easy (stack traces work) |

### 1.3 Why This Works

For a **single-slot CCID reader**, the USB host:
1. Sends a command (e.g., `IccPowerOn`)
2. **Waits** for the response
3. Only then sends the next command

The host doesn't send concurrent commands, so there's no benefit to handling USB events while waiting for the smartcard.

---

## 2. Proposed Async Architecture

### 2.1 Overview

The async architecture would use Rust's `async/await` with the [embassy](https://embassy.dev/) embedded async framework.

```
┌─────────────────────────────────────────────────────────────────┐
│                     Embassy Executor                             │
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │  USB Task    │  │ Smartcard    │  │  Timer Task  │          │
│  │  (async)     │  │ Task (async) │  │  (async)     │          │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘          │
│         │                 │                 │                   │
│         └─────────────────┴─────────────────┘                   │
│                           │                                      │
│                    Event/Channel                                 │
│                           │                                      │
│  Timeline (concurrent):                                         │
│  [USB RX] ──► [Queue Cmd] ──► [Smartcard processes] ──► [USB TX]│
│       │              ↑                │                          │
│       └──────────────┴── USB still responsive!                  │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 Key Components

#### 2.2.1 Embassy Runtime

```rust
// Cargo.toml
[dependencies]
embassy-executor = { version = "0.5", features = ["arch-cortex-m", "executor-thread"] }
embassy-time = "0.3"
embassy-stm32 = { version = "0.1", features = ["stm32f469ni", "time-driver-any"] }
embassy-usb = "0.2"
embassy-sync = "0.5"
```

#### 2.2.2 Task Structure

```rust
#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use embassy_executor::Spawner;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};

// Command queue from USB to smartcard task
static CCID_CMD_CHANNEL: Channel<CcidCommand, 1> = Channel::new();
// Response queue from smartcard to USB task
static CCID_RESP_CHANNEL: Channel<CcidResponse, 1> = Channel::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Spawn USB task
    spawner.spawn(usb_task()).unwrap();
    // Spawn smartcard task
    spawner.spawn(smartcard_task()).unwrap();
    // Spawn card detection task
    spawner.spawn(card_detect_task()).unwrap();
}

// USB task - handles CCID protocol
#[embassy_executor::task]
async fn usb_task() {
    let usb = setup_usb().await;
    let mut ccid = CcidClass::new(usb);
    
    loop {
        // Non-blocking USB poll
        match ccid.poll().await {
            Some(command) => {
                // Send to smartcard task (non-blocking)
                CCID_CMD_CHANNEL.send(command).await;
            }
            None => {}
        }
        
        // Check for responses (non-blocking)
        if let Ok(response) = CCID_RESP_CHANNEL.try_receive() {
            ccid.send_response(response).await;
        }
    }
}

// Smartcard task - handles ISO 7816-3
#[embassy_executor::task]
async fn smartcard_task() {
    let mut smartcard = SmartcardUart::new(/* ... */);
    
    loop {
        // Wait for command (yields to other tasks while waiting)
        let command = CCID_CMD_CHANNEL.receive().await;
        
        // Process command (can be interrupted by higher-priority tasks)
        let response = match command {
            CcidCommand::PowerOn => {
                match smartcard.power_on_async().await {
                    Ok(atr) => CcidResponse::Atr(atr),
                    Err(e) => CcidResponse::Error(e),
                }
            }
            CcidCommand::XfrBlock(apdu) => {
                let mut resp = [0u8; 261];
                match smartcard.transmit_apdu_async(&apdu, &mut resp).await {
                    Ok(len) => CcidResponse::Data(resp[..len].to_vec()),
                    Err(e) => CcidResponse::Error(e),
                }
            }
            // ... other commands
        };
        
        // Send response back to USB task
        CCID_RESP_CHANNEL.send(response).await;
    }
}

// Card detection task - monitors card presence
#[embassy_executor::task]
async fn card_detect_task() {
    let mut card_present = false;
    
    loop {
        Timer::after(Duration::from_millis(100)).await;
        
        let now_present = is_card_present();
        if now_present != card_present {
            card_present = now_present;
            // Notify USB task of card change
            notify_card_change(now_present);
        }
    }
}
```

#### 2.2.3 Async Smartcard Operations

```rust
impl SmartcardUart {
    /// Async power-on with cancellable ATR reception
    pub async fn power_on_async(&mut self) -> Result<&Atr, SmartcardError> {
        // Activation sequence
        self.pwr_pin.set_low();
        Timer::after(Duration::from_millis(50)).await;
        self.rst_pin.set_high();
        
        // Async ATR reception
        self.read_atr_async().await
    }
    
    /// Async ATR reception - yields while waiting for bytes
    async fn read_atr_async(&mut self) -> Result<&Atr, SmartcardError> {
        let mut timeout = Duration::from_millis(400);
        
        loop {
            // Check for byte (non-blocking)
            if self.usart.sr().read().rxne().bit_is_set() {
                let byte = self.usart.dr().read().dr().bits() as u8;
                // Process byte...
            } else {
                // Yield to other tasks while waiting
                Timer::after(Duration::from_micros(100)).await;
                timeout = timeout.checked_sub(Duration::from_micros(100))
                    .ok_or(SmartcardError::Timeout)?;
            }
        }
    }
    
    /// Async APDU transmission - can be cancelled
    pub async fn transmit_apdu_async(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> Result<usize, SmartcardError> {
        // Async byte-by-byte transmission with yields
        for &byte in command {
            self.send_byte_async(byte).await?;
        }
        
        // Async reception
        self.receive_response_async(response).await
    }
    
    async fn send_byte_async(&mut self, byte: u8) -> Result<(), SmartcardError> {
        // Wait for TX empty
        while !self.usart.sr().read().txe().bit_is_set() {
            Timer::after(Duration::from_micros(10)).await;
        }
        self.usart.dr().write(|w| unsafe { w.dr().bits(byte as u16) });
        
        // Wait for TX complete
        while !self.usart.sr().read().tc().bit_is_set() {
            Timer::after(Duration::from_micros(10)).await;
        }
        Ok(())
    }
}
```

### 2.3 FSM-Based Alternative (osmo-ccid Style in Rust)

For even more robustness, we could implement a full FSM in Rust:

```rust
use embassy_sync::signal::Signal;

/// ISO 7816-3 states
#[derive(Debug, Clone, Copy, PartialEq)]
enum Iso7816State {
    Reset,
    WaitAtr,
    InAtr,
    WaitTpdu,
    InTpdu,
    WaitPps,
    InPps,
}

/// FSM events
#[derive(Debug)]
enum Iso7816Event {
    RxByte(u8),
    TxComplete,
    Timeout,
    CardRemoval,
    Command(CcidCommand),
}

/// ISO 7816-3 Finite State Machine
struct Iso7816Fsm {
    state: Iso7816State,
    atr_buffer: [u8; 33],
    atr_len: usize,
    // ... other state
}

impl Iso7816Fsm {
    /// Process event - returns immediately, may emit response
    fn process_event(&mut self, event: Iso7816Event) -> Option<Iso7816Action> {
        match (self.state, event) {
            // Reset state: waiting for RST release
            (Iso7816State::Reset, Iso7816Event::Command(CcidCommand::PowerOn)) => {
                self.state = Iso7816State::WaitAtr;
                Some(Iso7816Action::EnableRx)
            }
            
            // Waiting for ATR start
            (Iso7816State::WaitAtr, Iso7816Event::RxByte(ts)) => {
                self.atr_buffer[0] = ts;
                self.atr_len = 1;
                self.state = Iso7816State::InAtr;
                None // Continue receiving
            }
            
            // Receiving ATR bytes
            (Iso7816State::InAtr, Iso7816Event::RxByte(byte)) => {
                self.atr_buffer[self.atr_len] = byte;
                self.atr_len += 1;
                // Check if ATR complete (complex logic here)
                if self.is_atr_complete() {
                    self.state = Iso7816State::WaitTpdu;
                    Some(Iso7816Action::AtrComplete(self.atr_len))
                } else {
                    None
                }
            }
            
            // Timeout during ATR
            (Iso7816State::WaitAtr, Iso7816Event::Timeout) |
            (Iso7816State::InAtr, Iso7816Event::Timeout) => {
                self.state = Iso7816State::Reset;
                Some(Iso7816Action::AtrError)
            }
            
            // Card removal - any state
            (_, Iso7816Event::CardRemoval) => {
                self.state = Iso7816State::Reset;
                Some(Iso7816Action::CardRemoved)
            }
            
            // ... many more state/event combinations
            _ => None,
        }
    }
}

/// FSM task
#[embassy_executor::task]
async fn iso7816_fsm_task() {
    let mut fsm = Iso7816Fsm::new();
    let event_signal: Signal<Iso7816Event> = Signal::new();
    
    loop {
        // Wait for any event
        let event = event_signal.wait().await;
        
        // Process and get action
        if let Some(action) = fsm.process_event(event) {
            match action {
                Iso7816Action::AtrComplete(len) => {
                    CCID_RESP_CHANNEL.send(CcidResponse::Atr(
                        fsm.atr_buffer[..len].to_vec()
                    )).await;
                }
                Iso7816Action::AtrError => {
                    CCID_RESP_CHANNEL.send(CcidResponse::Error(
                        SmartcardError::Timeout
                    )).await;
                }
                // ... other actions
            }
        }
    }
}
```

---

## 3. Comparison: Sync vs Async

### 3.1 Code Size Comparison

| Component | Sync (Current) | Async (Embassy) | FSM (Embassy) |
|-----------|----------------|-----------------|---------------|
| main.rs | 200 lines | 100 lines | 100 lines |
| ccid.rs | 1032 lines | 800 lines | 600 lines |
| smartcard.rs | 930 lines | 1100 lines | 400 lines |
| fsm.rs | N/A | N/A | 800 lines |
| **Total** | **~2162 lines** | **~2000 lines** | **~1900 lines** |
| Dependencies | 5 crates | 8 crates | 8 crates |
| Binary size | ~50KB | ~60KB | ~65KB |
| RAM usage | ~8KB | ~12KB | ~16KB |

### 3.2 Complexity Comparison

```
Synchronous (Current):
┌────────────────────────────────────┐
│ main.rs                            │  ★ Simple
│   loop { usb.poll() }              │
├────────────────────────────────────┤
│ ccid.rs                            │  ★ Linear flow
│   handle_command() → block → resp  │
├────────────────────────────────────┤
│ smartcard.rs                       │  ★ Linear flow
│   power_on() → block → return      │
└────────────────────────────────────┘

Async (Embassy):
┌────────────────────────────────────┐
│ main.rs                            │  ⚠ Task spawning
│   spawn(usb_task)                  │
│   spawn(smartcard_task)            │
├────────────────────────────────────┤
│ ccid.rs                            │  ⚠ Channel-based
│   poll().await → send().await      │
├────────────────────────────────────┤
│ smartcard.rs                       │  ⚠ Async/await
│   power_on_async().await           │
└────────────────────────────────────┘

FSM (Embassy):
┌────────────────────────────────────┐
│ main.rs                            │  ⚠ Multiple tasks
├────────────────────────────────────┤
│ ccid.rs                            │  ⚠ Event-driven
├────────────────────────────────────┤
│ fsm.rs                             │  ❌ Complex state machine
│   20+ states × 5+ events           │
├────────────────────────────────────┤
│ smartcard.rs                       │  ⚠ Just HAL
└────────────────────────────────────┘
```

### 3.3 Feature Comparison

| Feature | Sync | Async | FSM |
|---------|------|-------|-----|
| **USB responsiveness** | ❌ Blocked during ops | ✅ Always responsive | ✅ Always responsive |
| **Abort support** | ❌ No | ✅ Cancel futures | ✅ Event-driven |
| **Multi-slot** | ❌ No | ✅ Spawn N tasks | ✅ N FSM instances |
| **Card hot-plug** | ⚠️ Polling only | ✅ Dedicated task | ✅ Event-driven |
| **Power management** | ❌ Always running | ✅ Sleep between polls | ✅ Sleep between events |
| **Code simplicity** | ★★★★★ | ★★★☆☆ | ★★☆☆☆ |
| **Debugging ease** | ★★★★★ | ★★★☆☆ | ★★☆☆☆ |
| **Memory safety** | ★★★★★ | ★★★★★ | ★★★★★ |

---

## 4. When to Choose Async

### 4.1 Async is Better When:

1. **Multi-slot reader** (2+ slots)
   ```rust
   // Can handle multiple slots concurrently
   spawner.spawn(smartcard_task(0)).unwrap();
   spawner.spawn(smartcard_task(1)).unwrap();
   ```

2. **Abort support required**
   ```rust
   // Can cancel in-progress operations
   let power_on = smartcard.power_on_async();
   select(power_on, abort_signal.wait()).await;
   ```

3. **Other USB functions needed** (HID, mass storage)
   ```rust
   // Concurrent USB endpoints
   spawner.spawn(ccid_task()).unwrap();
   spawner.spawn(hid_task()).unwrap();
   ```

4. **Low power requirements**
   ```rust
   // CPU sleeps while waiting
   Timer::after(Duration::from_secs(1)).await; // WFI
   ```

5. **Real-time card monitoring**
   ```rust
   // Immediate notification on card change
   spawner.spawn(card_monitor_task()).unwrap();
   ```

### 4.2 Sync is Better When:

1. **Single-slot reader** ✅ Our case
2. **Simple CCID-only device** ✅ Our case
3. **No abort required** ✅ Our case
4. **Minimal code size** ✅ Our case
5. **Easy debugging** ✅ Our case

---

## 5. Migration Path (If Needed)

If we ever need to migrate to async, here's the path:

### Phase 1: Add Embassy (No Behavior Change)

```toml
# Cargo.toml
[dependencies]
embassy-executor = "0.5"
embassy-time = "0.3"
```

```rust
// main.rs - Wrap sync code in async
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    spawner.spawn(sync_wrapper()).unwrap();
}

#[embassy_executor::task]
async fn sync_wrapper() {
    // Existing sync code runs in a single task
    loop {
        usb_device.poll(&mut [&mut ccid_class]);
        // Still blocks, but in async context
    }
}
```

### Phase 2: Async Smartcard Operations

```rust
// Convert smartcard operations one by one
impl SmartcardUart {
    pub async fn power_on_async(&mut self) -> Result<&Atr, SmartcardError> {
        // Same logic, but with .await points
    }
}
```

### Phase 3: Full Async with Channels

```rust
// Split into tasks with channels
static CMD_CHANNEL: Channel<CcidCommand, 1> = Channel::new();

#[embassy_executor::task]
async fn usb_task() {
    loop {
        let cmd = ccid.poll().await;
        CMD_CHANNEL.send(cmd).await;
    }
}

#[embassy_executor::task]
async fn smartcard_task() {
    loop {
        let cmd = CMD_CHANNEL.receive().await;
        let resp = process(cmd).await;
        RESP_CHANNEL.send(resp).await;
    }
}
```

### Phase 4: FSM (If Needed)

```rust
// Replace linear async with FSM
let mut fsm = Iso7816Fsm::new();
loop {
    let event = wait_for_event().await;
    fsm.process(event);
}
```

---

## 6. Decision: Staying Synchronous

### 6.1 Rationale

For our **single-slot STM32F469-DISCO CCID reader**:

| Factor | Weight | Sync | Async | Winner |
|--------|--------|------|-------|--------|
| Simplicity | HIGH | ★★★★★ | ★★★☆☆ | **Sync** |
| Debugging | HIGH | ★★★★★ | ★★★☆☆ | **Sync** |
| CCID compliance | HIGH | ★★★★★ | ★★★★★ | Tie |
| Code size | MEDIUM | ★★★★★ | ★★★☆☆ | **Sync** |
| Multi-slot | LOW | ★☆☆☆☆ | ★★★★★ | Async (N/A) |
| Abort support | LOW | ★☆☆☆☆ | ★★★★★ | Async (N/A) |
| Power mgmt | LOW | ★★☆☆☆ | ★★★★★ | Async (N/A) |

**Conclusion:** Synchronous wins for our use case.

### 6.2 What We're Missing (Acceptable Tradeoffs)

1. **USB unresponsive during operations**
   - Impact: None (host waits anyway)
   - Mitigation: None needed

2. **No abort support**
   - Impact: Can't cancel long operations
   - Mitigation: Host can reset device

3. **No multi-slot**
   - Impact: None (single-slot hardware)
   - Mitigation: N/A

### 6.3 When to Reconsider

Revisit async if:

1. **Adding a second slot** → Async allows concurrent operations
2. **Adding HID/keyboard** → Async allows concurrent USB classes
3. **Battery power** → Async enables sleep between operations
4. **Strict abort requirements** → Async allows cancellation
5. **Real-time requirements** → Async provides better determinism

---

## 7. References

### 7.1 Async Embedded Rust

- [Embassy](https://embassy.dev/) - Async framework for embedded Rust
- [embassy-stm32](https://docs.embassy.dev/embassy-stm32/) - STM32 HAL with async
- [Embedded Rust Async Book](https://rust-lang.github.io/async-book/)

### 7.2 FSM Patterns

- [osmo-ccid-firmware](https://github.com/osmocom/osmo-ccid-firmware) - Reference FSM implementation
- [Rust State Machine Patterns](https://hoverbear.org/blog/rust-state-machine-pattern/)

### 7.3 Related Crates

```toml
[dependencies]
# For async approach:
embassy-executor = "0.5"      # Async executor
embassy-time = "0.3"          # Timers and delays
embassy-stm32 = "0.1"         # STM32 async HAL
embassy-usb = "0.2"           # Async USB stack
embassy-sync = "0.5"          # Channels, signals, mutexes

# For FSM approach:
futures = { version = "0.3", default-features = false }
heapless = "0.7"              # Queue for events
```

---

## 8. Conclusion

The **synchronous architecture is the right choice** for our single-slot CCID reader:

- ✅ Simpler code
- ✅ Easier debugging
- ✅ Smaller binary
- ✅ Fully CCID compliant
- ✅ Works correctly with SatoChip/Seedkeeper

The async architecture documented here is available as a reference if requirements change, but there's no current need to switch.

---

*Document maintained for future reference. Last updated: 2026-03-08*
