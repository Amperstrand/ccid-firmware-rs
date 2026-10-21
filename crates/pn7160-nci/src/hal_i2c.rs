//! #149 P3 — `embedded_hal::i2c::I2c` (1.0) adapter over the raw
//! `i2c_master` driver_ng FFI.
//!
//! The pinned esp-idf-hal generation did not wrap driver_ng (issues
//! #63/#80); since the #242/#230 wave every EMBEDDED-HAL I2C consumer
//! (MFRC522 included) rides THIS adapter over the raw driver_ng FFI —
//! the PN7160 transport and the diagnostic PN7160 modules access
//! `i2c_master_*` through raw FFI directly (D-9 scope,
//! docs/DECISION_REGISTER.md). The vendored-hal `legacy-i2c` gate is
//! retired. This adapter is the convergence piece: one I2C driver
//! generation for both projects (ccid + bolty-rs), the retirement
//! path for the vendor patch, and IDF-6 pre-alignment (legacy driver
//! removed there).
//!
//! Scope today (#149 P3, post-#242): the trait impl + the
//! host-testable pure mapping (esp-idf error codes →
//! `embedded_hal::i2c::ErrorKind`, address-vs-handle validation) are
//! IN SERVICE — the `backend-mfrc522` migration and the vendor-patch
//! retirement this adapter was staged for have both landed (#242 /
//! #230); the skeleton-era "bench-gated follow-up" framing is
//! obsolete.
//!
//! Consumer scope (#149 P3, this change): `I2cMasterBus` (bus creation
//! + probe + device-add, the `esp_transport::from_peripherals` pattern
//! lifted to a shared constructor) and the fused Write→Read
//! `i2c_master_transmit_receive` path — the wire-shape parity the
//! legacy esp-idf-hal `I2cDriver` produced, which the bench 0xD3
//! cadence compare and the MFRC522/AXP192 register reads depend on.
//!
//! Dispatch layering (#332, Codex review on #321): the production
//! `transaction` walk lives in the PURE, host-compiled
//! [`dispatch_transaction`] and classifies exclusively through the
//! tested [`write_run_len`] / [`op_is_read`] / [`classify_ops`] — the
//! esp-side `run_len_ops`/`read_at` private duplicates are deleted.
//! The only target-only code left is the FFI seam
//! (`impl RawI2cMaster for I2cMasterDevice`) and the two-line
//! `I2c::transaction` wrapper.

use embedded_hal::i2c::{ErrorKind, NoAcknowledgeSource, Operation};
#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
use embedded_hal::i2c::{ErrorType, I2c};

/// Wraps an existing `i2c_master_dev_handle_t` (created via the bus
/// APIs in `esp_transport`, or directly). The device handle carries its
/// configured 7-bit address — the embedded-hal API passes an address
/// per call, so the adapter VALIDATES it against the handle's config
/// and refuses a mismatch (a silent wrong-address write is the worst
/// failure mode on a shared bus).
#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
pub struct I2cMasterDevice {
    dev: RawDeviceHandle,
    addr: u8,
}

/// Raw device handle wrapper — `esp_idf_sys::i2c_master_dev_handle_t`
/// plus the address it was added with. Kept minimal so transport
/// internals can hand one over without exposing FFI types in the
/// embedded-hal surface.
#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
pub struct RawDeviceHandle {
    pub(crate) handle: esp_idf_sys::i2c_master_dev_handle_t,
}

// SAFETY: the raw driver_ng handles are only accessed from the thread
// that owns the bus/device (driver_ng is not internally thread-safe —
// same single-owner discipline as `esp_transport::EspPn7160Transport`).
// The m5stick LCD path wraps the pair in a Mutex, which serializes
// access by construction.
#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
unsafe impl Send for I2cMasterBus {}
#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
unsafe impl Send for I2cMasterDevice {}

/// driver_ng master bus owner — `i2c_master_bus_handle_t` from
/// `i2c_new_master_bus`, the shared constructor for every non-PN7160
/// consumer of the adapter (mfrc522 backend, AXP192 LCD path, bolty's
/// MFRC522 boards). Field values mirror
/// `esp_transport::EspPn7160Transport::from_peripherals` exactly:
/// glitch filter 7, `I2C_CLK_SRC_DEFAULT`, flags zero — internal
/// pull-ups OFF (every board this crate serves carries external
/// pulls: nucula 2.2 kΩ to 3.0 V, M5 Grove + RC522-module onboard).
///
/// The bus handle is deliberately NEVER deleted (`i2c_master_del_bus`
/// is not called anywhere): firmware buses live for the whole program,
/// and dropping this wrapper leaves the C-side bus alive so devices
/// added from it stay valid — driver_ng requires the bus to outlive
/// its devices.
#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
pub struct I2cMasterBus {
    handle: esp_idf_sys::i2c_master_bus_handle_t,
}

#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
impl I2cMasterBus {
    /// Create a driver_ng master bus on I2C controller `port`. The new
    /// driver claims the controller by port number, so the legacy
    /// `Peripherals::i2cN` token is neither needed nor consumed.
    pub fn new(port: i32, sda: i32, scl: i32) -> Result<Self, AdapterError> {
        let mut handle: esp_idf_sys::i2c_master_bus_handle_t = core::ptr::null_mut();
        // REFERENCE: driver_ng `i2c_master_bus_config_t`
        // (esp-idf components/esp_driver_i2c/i2c_master.h) — field set
        // identical to the bench-proven #149 P1 transport fill.
        unsafe {
            let mut cfg: esp_idf_sys::i2c_master_bus_config_t = core::mem::zeroed();
            cfg.i2c_port = port;
            cfg.sda_io_num = sda as esp_idf_sys::gpio_num_t;
            cfg.scl_io_num = scl as esp_idf_sys::gpio_num_t;
            cfg.__bindgen_anon_1.clk_source =
                esp_idf_sys::soc_periph_i2c_clk_src_t_I2C_CLK_SRC_DEFAULT;
            cfg.glitch_ignore_cnt = 7;
            let rc = esp_idf_sys::i2c_new_master_bus(&cfg, &mut handle);
            if rc != 0 {
                return Err(AdapterError {
                    kind: esp_err_to_kind(rc),
                    raw: rc,
                });
            }
        }
        Ok(Self { handle })
    }

    /// Zero-length health probe via the new driver's dedicated API
    /// (`i2c_master_probe` — bus-level, no device handle needed).
    /// Ok(()) = device ACKs; Err = NAK/timeout.
    pub fn probe(&mut self, addr: u8, timeout_ms: i32) -> Result<(), AdapterError> {
        let rc = unsafe { esp_idf_sys::i2c_master_probe(self.handle, addr as u16, timeout_ms) };
        if rc == 0 {
            Ok(())
        } else {
            Err(AdapterError {
                kind: esp_err_to_kind(rc),
                raw: rc,
            })
        }
    }

    /// Add a 7-bit device at `addr` and wrap it in the embedded-hal
    /// adapter. `scl_speed_hz` is per-device in driver_ng (the nucula
    /// PN7160 rides 100 kHz, the M5 MFRC522/AXP192 buses 400 kHz).
    pub fn add_hal_device(
        &mut self,
        addr: u8,
        scl_speed_hz: u32,
    ) -> Result<I2cMasterDevice, AdapterError> {
        let mut dev: esp_idf_sys::i2c_master_dev_handle_t = core::ptr::null_mut();
        unsafe {
            let mut dev_cfg: esp_idf_sys::i2c_device_config_t = core::mem::zeroed();
            dev_cfg.dev_addr_length = esp_idf_sys::i2c_addr_bit_len_t_I2C_ADDR_BIT_LEN_7;
            dev_cfg.device_address = addr as u16;
            dev_cfg.scl_speed_hz = scl_speed_hz;
            let rc = esp_idf_sys::i2c_master_bus_add_device(self.handle, &dev_cfg, &mut dev);
            if rc != 0 {
                return Err(AdapterError {
                    kind: esp_err_to_kind(rc),
                    raw: rc,
                });
            }
        }
        // SAFETY: the handle originates from `i2c_master_bus_add_device`
        // on `self`'s bus one statement above and is handed to the sole
        // owner returned below (`from_raw`'s contract).
        Ok(I2cMasterDevice::new(
            unsafe { RawDeviceHandle::from_raw(dev) },
            addr,
        ))
    }
}

/// Pure, host-testable: the operation shape at one position of an
/// embedded-hal `transaction` slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Write,
    Read,
}

/// Pure, host-testable: length of the run of consecutive `Write` ops
/// starting at `idx`. Consecutive Writes must coalesce into ONE
/// `i2c_master_transmit` — a single START…address…bytes…STOP stream —
/// because that is the wire shape the legacy esp-idf-hal cmd-link
/// `I2cDriver::transaction` produced (one cmd link = one bus
/// transaction; subsequent `i2c_master_write` stages append bytes, no
/// new START). The MFRC522 I2C slave treats the first byte after the
/// address as the register address in EVERY transaction (NXP MFRC522
/// data sheet, I2C interface protocol), so two standalone transmits
/// re-parse payload bytes as register addresses — which is #307: every
/// `write_many` FIFO load (each transceive frame) scattered into
/// random registers, tag presence never fired, while single-register
/// reads/writes (probe, version read, CCID service) kept working.
pub fn write_run_len(kinds: &[OpKind], idx: usize) -> usize {
    let mut n = 0;
    while kinds.get(idx + n) == Some(&OpKind::Write) {
        n += 1;
    }
    n
}

/// Pure, host-testable: is the op at `idx` a `Read`? (Used to decide
/// Write-run→Read fusion — embedded-hal 1.0 `write_read(addr, w, r)`
/// dispatches to `transaction(addr, [Write(w), Read(r)])` and must ride
/// ONE repeated-START `i2c_master_transmit_receive`, the legacy
/// `i2c_master_write_read_device` shape the bench 0xD3 cadence compare
/// reads on the wire.)
pub fn op_is_read(kinds: &[OpKind], idx: usize) -> bool {
    kinds.get(idx) == Some(&OpKind::Read)
}

/// Pure, host-testable: concatenate the payloads of a slice of `Write`
/// operations into `buf`, returning the total length. Errors loudly on
/// overflow — a truncated coalesce would silently change the wire
/// shape, the exact failure class this adapter exists to prevent.
pub fn concat_writes<'a>(ops: &[Operation<'a>], buf: &mut [u8]) -> Result<usize, AdapterError> {
    let mut n = 0;
    for op in ops {
        let Operation::Write(bytes) = op else {
            return Err(AdapterError {
                kind: ErrorKind::Other,
                raw: -(2/*EINVAL: non-Write op in a Write run — shape pinned by write_run_len*/),
            });
        };
        if n + bytes.len() > buf.len() {
            return Err(AdapterError {
                kind: ErrorKind::Other,
                raw: (n + bytes.len()) as i32, // required length for diagnostics
            });
        }
        buf[n..n + bytes.len()].copy_from_slice(bytes);
        n += bytes.len();
    }
    Ok(n)
}

/// Pure classification of an `Operation` (host-testable; no FFI).
pub fn op_kind(op: &Operation<'_>) -> OpKind {
    match op {
        Operation::Write(_) => OpKind::Write,
        Operation::Read(_) => OpKind::Read,
    }
}

/// Cap on operations per `transaction` slice: the dispatch walk
/// materializes the kind of every op up front ([`classify_ops`]).
/// Real embedded-hal 1.0 drivers issue command-style sequences (the
/// MFRC522 fork: `write_many` → 2 ops, `write_read` → 2; the AXP192
/// LCD path: 1–2); 32 is far beyond any real caller, and exceeding it
/// errors LOUDLY — never silently truncates (the same contract as the
/// coalesce buffer `WBUF_SZ` below).
pub const MAX_TRANSACTION_OPS: usize = 32;

/// Pure, host-testable: classify a whole `transaction` slice into
/// per-op kinds. THE only production path from `Operation` to
/// `OpKind` (#332) — the esp dispatch walks this Vec through
/// [`write_run_len`] / [`op_is_read`]; it keeps no private classifier.
pub fn classify_ops(
    ops: &[Operation<'_>],
) -> Result<heapless::Vec<OpKind, MAX_TRANSACTION_OPS>, AdapterError> {
    let mut kinds = heapless::Vec::new();
    for op in ops {
        kinds.push(op_kind(op)).map_err(|_| AdapterError {
            kind: ErrorKind::Other,
            raw: -(2 /*EINVAL: transaction longer than MAX_TRANSACTION_OPS — refuse, never truncate*/),
        })?;
    }
    Ok(kinds)
}

/// Pure, host-testable: ONE dispatch decision of the transaction
/// walk — the complete set of rulings the target-side executor makes.
/// Every run-length / read-fusion ruling rides the tested
/// [`write_run_len`] / [`op_is_read`] (via [`next_step`]), so host
/// tests pin the production wire shape BY CONSTRUCTION (#332: the
/// pre-refactor dispatch classified through private cfg(esp)
/// `run_len_ops`/`read_at` twins that never compiled on host).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchStep {
    /// Standalone Read at `idx`: one `i2c_master_receive`.
    Read { idx: usize },
    /// Run of `run` Write ops at `idx` fused with the Read at
    /// `idx + run`: ONE repeated-START `i2c_master_transmit_receive`
    /// whose tx payload is the CONCATENATED Write run.
    WriteRead { idx: usize, run: usize },
    /// Run of `run` Write ops at `idx` with no following Read: ONE
    /// `i2c_master_transmit` of the concatenated payloads (a run of 1
    /// passes the single payload straight through, no coalesce copy).
    Write { idx: usize, run: usize },
}

impl DispatchStep {
    /// Walk-cursor advance: index of the first op this step does NOT
    /// cover.
    pub fn next_idx(&self) -> usize {
        match *self {
            DispatchStep::Read { idx } => idx + 1,
            DispatchStep::Write { idx, run } => idx + run,
            DispatchStep::WriteRead { idx, run } => idx + run + 1,
        }
    }
}

/// Pure, host-testable: the next dispatch step at `idx` (`None` when
/// the walk has covered the slice). Implemented ONLY through
/// [`write_run_len`] and [`op_is_read`] — the single classification
/// authority for the production dispatch.
pub fn next_step(kinds: &[OpKind], idx: usize) -> Option<DispatchStep> {
    if idx >= kinds.len() {
        return None;
    }
    let run = write_run_len(kinds, idx);
    Some(if run == 0 {
        DispatchStep::Read { idx }
    } else if op_is_read(kinds, idx + run) {
        DispatchStep::WriteRead { idx, run }
    } else {
        DispatchStep::Write { idx, run }
    })
}

/// The three driver_ng primitives the coalescing walk maps onto.
/// Split out as a trait so the dispatch brain ([`dispatch_transaction`])
/// is ONE generic, host-testable function (#332): `I2cMasterDevice`
/// implements it with the real FFI on target; host tests drive it with
/// a recording mock — so the production call sequence is exercised on
/// host by construction.
pub trait RawI2cMaster {
    /// `i2c_master_transmit` — one START…address…bytes…STOP stream.
    fn raw_transmit(&mut self, bytes: &[u8], timeout_ms: i32) -> Result<(), AdapterError>;
    /// `i2c_master_receive` — one read-phase transaction.
    fn raw_receive(&mut self, buffer: &mut [u8], timeout_ms: i32) -> Result<(), AdapterError>;
    /// `i2c_master_transmit_receive` — one repeated-START fused
    /// write→read transaction.
    fn raw_transmit_receive(
        &mut self,
        tx: &[u8],
        rx: &mut [u8],
        timeout_ms: i32,
    ) -> Result<(), AdapterError>;
}

/// Pure orchestration over any [`RawI2cMaster`] — THE production
/// dispatch (#332, Codex review on #321). `impl I2c for
/// I2cMasterDevice::transaction` validates the address, supplies the
/// stack buffers, and delegates here; every classification decision
/// routes through the tested [`classify_ops`] / [`next_step`] /
/// [`write_run_len`] / [`op_is_read`]. Wire shape (the #307/#321
/// contract, unchanged by this refactor):
///
/// - Write run followed by a Read → ONE fused repeated-START
///   `raw_transmit_receive`, tx = concatenated run payloads;
/// - Write run of 2+ with no following Read → ONE `raw_transmit` of
///   the concatenated payloads;
/// - single Write → one `raw_transmit` of its payload as-is;
/// - standalone Read → one `raw_receive`.
pub fn dispatch_transaction(
    bus: &mut impl RawI2cMaster,
    ops: &mut [Operation<'_>],
    wbuf: &mut [u8],
    timeout_ms: i32,
) -> Result<(), AdapterError> {
    let kinds = classify_ops(ops)?;
    let mut idx = 0;
    while let Some(step) = next_step(&kinds, idx) {
        match step {
            DispatchStep::Read { idx } => {
                let Operation::Read(buf) = &mut ops[idx] else {
                    unreachable!(
                        "write_run_len returned 0 at a non-Read op — kinds from classify_ops"
                    )
                };
                bus.raw_receive(buf, timeout_ms)?;
            }
            DispatchStep::WriteRead { idx, run } => {
                // (Write…Write run) → Read: ONE repeated-START
                // `i2c_master_transmit_receive` whose tx payload is the
                // CONCATENATED Write run — the legacy
                // `i2c_master_write_read_device` wire shape. Covers
                // embedded-hal `write_read` ([W,R]) and the general
                // [W…W,R] cmd-link shape alike.
                let (wr_ops, rest) = ops[idx..idx + run + 1].split_at_mut(run);
                let n = concat_writes(wr_ops, wbuf)?;
                let Operation::Read(rbuf) = &mut rest[0] else {
                    unreachable!("shape pinned by op_is_read")
                };
                bus.raw_transmit_receive(&wbuf[..n], rbuf, timeout_ms)?;
            }
            DispatchStep::Write { idx, run: 1 } => {
                let Operation::Write(buf) = &ops[idx] else {
                    unreachable!("write_run_len >= 1 at a non-Write op — kinds from classify_ops")
                };
                bus.raw_transmit(buf, timeout_ms)?;
            }
            DispatchStep::Write { idx, run } => {
                // Write run of 2+: ONE `i2c_master_transmit` carrying the
                // concatenated payloads — a single START…address…bytes…STOP
                // stream. Two standalone transmits would make the MFRC522
                // re-parse payload bytes as register addresses (#307 —
                // mfrc522 `write_many` FIFO loads).
                let n = concat_writes(&ops[idx..idx + run], wbuf)?;
                bus.raw_transmit(&wbuf[..n], timeout_ms)?;
            }
        }
        idx = step.next_idx();
    }
    Ok(())
}

#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
impl RawDeviceHandle {
    /// Wrap a raw driver_ng device handle.
    ///
    /// # Safety
    /// `handle` must originate from `i2c_master_bus_add_device`, remain
    /// valid for the adapter's lifetime, and not be used concurrently
    /// from elsewhere (driver_ng handles are not thread-safe).
    pub unsafe fn from_raw(handle: esp_idf_sys::i2c_master_dev_handle_t) -> Self {
        Self { handle }
    }
}

/// Pure, host-testable: which `ErrorKind` an esp-idf `esp_err_t`
/// maps to on the embedded-hal surface. `ESP_ERR_TIMEOUT` (0x107) is
/// the NAK/timeout class a bench probe must surface as
/// `NoAcknowledge`; everything else is `Other` (the raw code travels
/// in the adapter's error type for diagnostics).
pub fn esp_err_to_kind(rc: i32) -> ErrorKind {
    // esp_err_t: ESP_ERR_TIMEOUT = 0x107 (esp_err.h); the driver_ng
    // family surfaces bus NAK/timeout through it.
    if rc == 0x107 {
        ErrorKind::NoAcknowledge(NoAcknowledgeSource::Unknown)
    } else {
        ErrorKind::Other
    }
}

/// Adapter error: the kind plus the raw esp_err_t for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterError {
    pub kind: ErrorKind,
    pub raw: i32,
}

impl embedded_hal::i2c::Error for AdapterError {
    fn kind(&self) -> ErrorKind {
        self.kind
    }
}

/// Pure, host-testable: address-vs-handle policy. The device handle
/// is bound to one address at `i2c_master_bus_add_device` time; an
/// embedded-hal caller addressing anything else must fail loudly
/// before any bus traffic.
pub fn validate_address(call_addr: u8, handle_addr: u8) -> Result<(), AdapterError> {
    if call_addr == handle_addr {
        Ok(())
    } else {
        Err(AdapterError {
            kind: ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address),
            raw: call_addr as i32,
        })
    }
}

#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
impl I2cMasterDevice {
    /// Wrap an existing driver_ng device handle bound to `addr`.
    pub fn new(dev: RawDeviceHandle, addr: u8) -> Self {
        Self { dev, addr }
    }

    /// The 7-bit address this device handle is bound to.
    pub fn address(&self) -> u8 {
        self.addr
    }

    fn check(&self, address: u8) -> Result<(), AdapterError> {
        validate_address(address, self.addr)
    }
}

#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
const OP_TIMEOUT_MS: i32 = 100;

/// Coalesced-transmit buffer. The largest real caller payload is an
/// MFRC522 FIFO load: 64-byte FIFO + one register-address byte = 65;
/// 128 leaves margin. Overflow errors loudly with the required length
/// in `raw` — never silently truncates (that would change the wire
/// shape, the exact failure class this adapter exists to prevent).
#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
const WBUF_SZ: usize = 128;

/// FFI seam of the dispatch (#332): the three driver_ng primitives,
/// verbatim from the pre-refactor private methods. All classification
/// lives in the host-tested pure layer; this impl only executes.
#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
impl RawI2cMaster for I2cMasterDevice {
    fn raw_transmit(&mut self, bytes: &[u8], timeout_ms: i32) -> Result<(), AdapterError> {
        let rc = unsafe {
            esp_idf_sys::i2c_master_transmit(
                self.dev.handle,
                bytes.as_ptr(),
                bytes.len(),
                timeout_ms,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(AdapterError {
                kind: esp_err_to_kind(rc),
                raw: rc,
            })
        }
    }

    fn raw_receive(&mut self, buffer: &mut [u8], timeout_ms: i32) -> Result<(), AdapterError> {
        let rc = unsafe {
            esp_idf_sys::i2c_master_receive(
                self.dev.handle,
                buffer.as_mut_ptr(),
                buffer.len(),
                timeout_ms,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(AdapterError {
                kind: esp_err_to_kind(rc),
                raw: rc,
            })
        }
    }

    /// Fused Write→Read in ONE bus transaction (repeated START, no
    /// STOP between the phases) — `i2c_master_transmit_receive`
    /// (esp-idf components/esp_driver_i2c/i2c_master.h), the driver_ng
    /// equivalent of the legacy `i2c_master_write_read_device`.
    fn raw_transmit_receive(
        &mut self,
        tx: &[u8],
        rx: &mut [u8],
        timeout_ms: i32,
    ) -> Result<(), AdapterError> {
        let rc = unsafe {
            esp_idf_sys::i2c_master_transmit_receive(
                self.dev.handle,
                tx.as_ptr(),
                tx.len(),
                rx.as_mut_ptr(),
                rx.len(),
                timeout_ms,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(AdapterError {
                kind: esp_err_to_kind(rc),
                raw: rc,
            })
        }
    }
}

#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
impl I2c for I2cMasterDevice {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.check(address)?;
        let mut wbuf = [0u8; WBUF_SZ];
        dispatch_transaction(self, operations, &mut wbuf, OP_TIMEOUT_MS)
    }
}

#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
impl ErrorType for I2cMasterDevice {
    type Error = AdapterError;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_maps_to_no_acknowledge() {
        // ESP_ERR_TIMEOUT (0x107) — the bench NAK class — must surface
        // as NoAcknowledge so a stuck bus reads as "device not
        // answering", not an opaque Other.
        assert_eq!(
            esp_err_to_kind(0x107),
            ErrorKind::NoAcknowledge(NoAcknowledgeSource::Unknown)
        );
    }

    #[test]
    fn other_codes_map_to_other_with_raw_intact() {
        assert_eq!(esp_err_to_kind(0), ErrorKind::Other);
        assert_eq!(esp_err_to_kind(-1), ErrorKind::Other);
        assert_eq!(esp_err_to_kind(0x105), ErrorKind::Other);
    }

    #[test]
    fn address_mismatch_is_refused_before_any_bus_traffic() {
        // The handle is bound to 0x28; addressing 0x29 must fail loudly
        // (a silent wrong-address write on a shared bus is the worst
        // failure mode — the PCF8574T shares it).
        let err = validate_address(0x29, 0x28).unwrap_err();
        assert_eq!(
            err.kind,
            ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)
        );
        assert_eq!(err.raw, 0x29);
        assert!(validate_address(0x28, 0x28).is_ok());
    }

    #[test]
    fn consecutive_writes_coalesce_into_one_transmit() {
        // #307: [Write, Write] (mfrc522 `write_many`) MUST fuse into ONE
        // transmit — the legacy esp-idf-hal cmd-link streamed consecutive
        // writes as a single START…STOP transaction, and the MFRC522
        // parses the first byte after the address as a register address
        // in EVERY transaction. Two standalone transmits scattered FIFO
        // loads into random registers: tag presence dead, probe/version/
        // single-register ops fine — exactly the #307 evidence chain.
        use super::{write_run_len, OpKind};

        assert_eq!(write_run_len(&[OpKind::Write, OpKind::Write], 0), 2);
        assert_eq!(write_run_len(&[OpKind::Write, OpKind::Read], 0), 1);
        assert_eq!(write_run_len(&[OpKind::Read, OpKind::Write], 0), 0);
        assert_eq!(write_run_len(&[OpKind::Write], 0), 1);
        assert_eq!(write_run_len(&[OpKind::Read], 0), 0);
        assert_eq!(
            write_run_len(&[OpKind::Write, OpKind::Write, OpKind::Read], 0),
            2
        );
        // A run starting mid-sequence is judged fresh at its own index.
        assert_eq!(write_run_len(&[OpKind::Read, OpKind::Write], 1), 1);
    }

    #[test]
    fn write_run_followed_by_read_fuses_transmit_receive() {
        // embedded-hal 1.0 `write_read(addr, w, r)` dispatches to
        // `transaction(addr, [Write(w), Read(r)])` — ONE repeated-START
        // transmit_receive, the legacy `write_read` wire shape; the
        // general [W…W,R] shape rides the same fusion with a
        // concatenated tx payload. The bench 0xD3 cadence compare reads
        // this difference on the wire.
        use super::{op_is_read, write_run_len, OpKind};

        let kinds = [OpKind::Write, OpKind::Read];
        let run = write_run_len(&kinds, 0);
        assert_eq!(run, 1);
        assert!(op_is_read(&kinds, run));

        let kinds = [OpKind::Write, OpKind::Write, OpKind::Read];
        let run = write_run_len(&kinds, 0);
        assert_eq!(run, 2);
        assert!(op_is_read(&kinds, run));

        // [W,W] with NO following Read: plain transmit (no read fusion).
        let kinds = [OpKind::Write, OpKind::Write];
        let run = write_run_len(&kinds, 0);
        assert!(!op_is_read(&kinds, run));
    }

    #[test]
    fn concat_writes_orders_payloads_and_fails_loud_on_overflow() {
        use super::concat_writes;

        let ops = [Operation::Write(&[0x02u8]), Operation::Write(&[0xAA, 0xBB])];
        let mut buf = [0u8; 8];
        assert_eq!(concat_writes(&ops, &mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], &[0x02, 0xAA, 0xBB]);

        // Never truncate silently — overflow reports the required length.
        let mut small = [0u8; 2];
        let err = concat_writes(&ops, &mut small).unwrap_err();
        assert_eq!(err.raw, 3);
    }

    // --- #332: the PRODUCTION dispatch, executed on host ----------------
    // `impl I2c for I2cMasterDevice::transaction` validates the address
    // and delegates to `dispatch_transaction`; these tests drive that
    // exact function against a recording mock of the driver_ng FFI
    // seam. Pre-refactor this walk classified through the private
    // cfg(esp) `run_len_ops`/`read_at` twins that never compiled on
    // host — now the wire shape is pinned by construction: if the
    // dispatch stops routing through `classify_ops`/`next_step`
    // (write_run_len/op_is_read), these are the calls that change.

    /// One recorded driver_ng primitive call.
    #[derive(Debug, PartialEq, Eq)]
    enum RawCall {
        Transmit(Vec<u8>),
        Receive(usize),
        TransmitReceive(Vec<u8>, usize),
    }

    /// Recording mock of the FFI seam: logs every bus call and fills
    /// each read buffer with a per-call pattern (so tests can prove a
    /// read landed in the RIGHT `Operation`'s buffer).
    #[derive(Default)]
    struct RecordingBus {
        calls: Vec<RawCall>,
        fill: u8,
    }

    impl super::RawI2cMaster for RecordingBus {
        fn raw_transmit(&mut self, bytes: &[u8], _timeout_ms: i32) -> Result<(), AdapterError> {
            self.calls.push(RawCall::Transmit(bytes.to_vec()));
            Ok(())
        }

        fn raw_receive(&mut self, buffer: &mut [u8], _timeout_ms: i32) -> Result<(), AdapterError> {
            self.fill = self.fill.wrapping_add(1);
            buffer.fill(self.fill);
            self.calls.push(RawCall::Receive(buffer.len()));
            Ok(())
        }

        fn raw_transmit_receive(
            &mut self,
            tx: &[u8],
            rx: &mut [u8],
            _timeout_ms: i32,
        ) -> Result<(), AdapterError> {
            self.fill = self.fill.wrapping_add(1);
            rx.fill(self.fill);
            self.calls
                .push(RawCall::TransmitReceive(tx.to_vec(), rx.len()));
            Ok(())
        }
    }

    #[test]
    fn dispatch_coalesces_write_many_into_one_transmit() {
        // #307 on the wire, end to end through the dispatch: mfrc522
        // `write_many` issues [Write(addr), Write(payload)] and the
        // production path MUST issue ONE transmit carrying the
        // concatenation. The pre-#321 code (and any regression back to
        // standalone per-op transmits) records TWO calls here — the
        // MFRC522 then re-parses payload bytes as register addresses.
        let mut bus = RecordingBus::default();
        let mut ops = [Operation::Write(&[0x02u8]), Operation::Write(&[0xAA, 0xBB])];
        let mut wbuf = [0u8; 128];
        super::dispatch_transaction(&mut bus, &mut ops, &mut wbuf, 100).unwrap();
        assert_eq!(bus.calls, vec![RawCall::Transmit(vec![0x02, 0xAA, 0xBB])]);
    }

    #[test]
    fn dispatch_write_read_pair_is_one_fused_call() {
        // embedded-hal `write_read` → [W, R]: ONE repeated-START
        // transmit_receive (the legacy `write_read` wire shape the
        // bench 0xD3 cadence compare reads), never transmit+receive.
        let mut bus = RecordingBus::default();
        let mut rbuf = [0u8; 2];
        let mut ops = [Operation::Write(&[0x0Au8]), Operation::Read(&mut rbuf)];
        let mut wbuf = [0u8; 128];
        super::dispatch_transaction(&mut bus, &mut ops, &mut wbuf, 100).unwrap();
        assert_eq!(bus.calls, vec![RawCall::TransmitReceive(vec![0x0A], 2)]);
        // The read phase landed in the caller's buffer.
        assert_eq!(rbuf, [1, 1]);
    }

    #[test]
    fn dispatch_fuses_general_write_run_then_read() {
        // [W(reg), W(data), R(n)]: same fusion with a concatenated tx —
        // the cmd-link generalization beyond the plain [W,R] pair.
        let mut bus = RecordingBus::default();
        let mut rbuf = [0u8; 3];
        let mut ops = [
            Operation::Write(&[0x0Au8]),
            Operation::Write(&[0x01, 0x02]),
            Operation::Read(&mut rbuf),
        ];
        let mut wbuf = [0u8; 128];
        super::dispatch_transaction(&mut bus, &mut ops, &mut wbuf, 100).unwrap();
        assert_eq!(
            bus.calls,
            vec![RawCall::TransmitReceive(vec![0x0A, 0x01, 0x02], 3)]
        );
    }

    #[test]
    fn dispatch_standalone_read_is_one_receive() {
        let mut bus = RecordingBus::default();
        let mut rbuf = [0u8; 4];
        let mut ops = [Operation::Read(&mut rbuf)];
        let mut wbuf = [0u8; 128];
        super::dispatch_transaction(&mut bus, &mut ops, &mut wbuf, 100).unwrap();
        assert_eq!(bus.calls, vec![RawCall::Receive(4)]);
        assert_eq!(rbuf, [1, 1, 1, 1]);
    }

    #[test]
    fn dispatch_single_write_passes_straight_through() {
        // run of 1: payload transmitted as-is (no coalesce copy).
        let mut bus = RecordingBus::default();
        let mut ops = [Operation::Write(&[0x0Au8, 0x55])];
        let mut wbuf = [0u8; 128];
        super::dispatch_transaction(&mut bus, &mut ops, &mut wbuf, 100).unwrap();
        assert_eq!(bus.calls, vec![RawCall::Transmit(vec![0x0A, 0x55])]);
    }

    #[test]
    fn dispatch_mixed_sequence_splits_at_every_read() {
        // [W, W, R, W, R] → fused(run 2), fused(run 1) — and each read
        // phase lands in its OWN buffer (distinct fill patterns prove
        // the split_at_mut wiring never swaps read targets).
        let mut bus = RecordingBus::default();
        let mut r1 = [0u8; 1];
        let mut r2 = [0u8; 2];
        let mut ops = [
            Operation::Write(&[0x01u8]),
            Operation::Write(&[0x02]),
            Operation::Read(&mut r1),
            Operation::Write(&[0x03]),
            Operation::Read(&mut r2),
        ];
        let mut wbuf = [0u8; 128];
        super::dispatch_transaction(&mut bus, &mut ops, &mut wbuf, 100).unwrap();
        assert_eq!(
            bus.calls,
            vec![
                RawCall::TransmitReceive(vec![0x01, 0x02], 1),
                RawCall::TransmitReceive(vec![0x03], 2),
            ]
        );
        assert_eq!(r1, [1]);
        assert_eq!(r2, [2, 2]);
    }

    #[test]
    fn dispatch_empty_transaction_is_a_noop() {
        let mut bus = RecordingBus::default();
        let mut wbuf = [0u8; 128];
        super::dispatch_transaction(&mut bus, &mut [], &mut wbuf, 100).unwrap();
        assert!(bus.calls.is_empty());
    }

    #[test]
    fn dispatch_errors_loud_when_a_run_exceeds_the_coalesce_buffer() {
        // Never truncate: a Write run whose concatenation does not fit
        // the caller's buffer reports the required length.
        let mut bus = RecordingBus::default();
        let mut ops = [Operation::Write(&[0u8; 40]), Operation::Write(&[0u8; 40])];
        let mut wbuf = [0u8; 64];
        let err = super::dispatch_transaction(&mut bus, &mut ops, &mut wbuf, 100).unwrap_err();
        assert_eq!(err.raw, 80);
        assert!(bus.calls.is_empty());
    }

    #[test]
    fn next_step_pins_the_dispatch_branches() {
        use super::{next_step, DispatchStep, OpKind};

        // [W, W, R, R]: run 2 fused with the following Read; the next
        // standalone Read; walk terminates exactly past the slice.
        let kinds = [OpKind::Write, OpKind::Write, OpKind::Read, OpKind::Read];
        assert_eq!(
            next_step(&kinds, 0),
            Some(DispatchStep::WriteRead { idx: 0, run: 2 })
        );
        assert_eq!(next_step(&kinds, 3), Some(DispatchStep::Read { idx: 3 }));
        assert_eq!(next_step(&kinds, 4), None);

        // [R, W]: standalone Read first, then an unfused single Write.
        let kinds = [OpKind::Read, OpKind::Write];
        assert_eq!(next_step(&kinds, 0), Some(DispatchStep::Read { idx: 0 }));
        assert_eq!(
            next_step(&kinds, 1),
            Some(DispatchStep::Write { idx: 1, run: 1 })
        );

        // [W, W] with no following Read: coalesced transmit, no fusion.
        let kinds = [OpKind::Write, OpKind::Write];
        assert_eq!(
            next_step(&kinds, 0),
            Some(DispatchStep::Write { idx: 0, run: 2 })
        );
    }

    #[test]
    fn next_step_walk_advances_to_exactly_the_slice_end() {
        // The executor's cursor (`step.next_idx()`) must land on
        // ops.len() — never skip an op, never re-execute one.
        use super::{next_step, OpKind};

        let sequences: &[&[OpKind]] = &[
            &[],
            &[OpKind::Read],
            &[OpKind::Write],
            &[OpKind::Write, OpKind::Write],
            &[OpKind::Write, OpKind::Read],
            &[OpKind::Read, OpKind::Write, OpKind::Write, OpKind::Read],
            &[
                OpKind::Write,
                OpKind::Write,
                OpKind::Read,
                OpKind::Read,
                OpKind::Write,
            ],
        ];
        for kinds in sequences {
            let mut idx = 0;
            let mut steps = 0;
            while let Some(step) = next_step(kinds, idx) {
                assert!(step.next_idx() > idx, "cursor must advance");
                idx = step.next_idx();
                steps += 1;
                assert!(steps <= kinds.len() + 1, "walk must terminate");
            }
            assert_eq!(idx, kinds.len(), "walk must cover the whole slice");
        }
    }

    #[test]
    fn classify_ops_is_the_single_operation_to_kind_path() {
        // classify_ops must agree with op_kind element-for-element,
        // and refuse (loudly, raw=-2) transactions beyond the cap —
        // never silently truncate the walk's input.
        use super::{classify_ops, op_kind, OpKind, MAX_TRANSACTION_OPS};

        let writes: [u8; 3] = [1, 2, 3];
        let mut read_buf: [u8; 2] = [0; 2];
        let ops = [Operation::Write(&writes), Operation::Read(&mut read_buf)];
        let kinds = classify_ops(&ops).unwrap();
        assert_eq!(kinds.as_slice(), &[OpKind::Write, OpKind::Read]);
        assert_eq!(op_kind(&ops[0]), OpKind::Write);

        let too_many: Vec<Operation<'_>> = (0..=MAX_TRANSACTION_OPS)
            .map(|_| Operation::Write(&[]))
            .collect();
        let err = classify_ops(&too_many).unwrap_err();
        assert_eq!(err.raw, -2);

        let at_cap: Vec<Operation<'_>> = (0..MAX_TRANSACTION_OPS)
            .map(|_| Operation::Write(&[]))
            .collect();
        assert_eq!(classify_ops(&at_cap).unwrap().len(), MAX_TRANSACTION_OPS);
    }
}
