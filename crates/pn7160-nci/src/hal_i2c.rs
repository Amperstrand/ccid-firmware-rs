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

    fn transmit(&mut self, bytes: &[u8], timeout_ms: i32) -> Result<(), AdapterError> {
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

    fn receive(&mut self, buffer: &mut [u8], timeout_ms: i32) -> Result<(), AdapterError> {
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
    fn transmit_receive(
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
const OP_TIMEOUT_MS: i32 = 100;

/// Coalesced-transmit buffer. The largest real caller payload is an
/// MFRC522 FIFO load: 64-byte FIFO + one register-address byte = 65;
/// 128 leaves margin. Overflow errors loudly with the required length
/// in `raw` — never silently truncates (that would change the wire
/// shape, the exact failure class this adapter exists to prevent).
#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
const WBUF_SZ: usize = 128;

#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
fn run_len_ops(ops: &[Operation<'_>], idx: usize) -> usize {
    let mut n = 0;
    while matches!(ops.get(idx + n), Some(Operation::Write(_))) {
        n += 1;
    }
    n
}

#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
fn read_at(ops: &[Operation<'_>], idx: usize) -> bool {
    matches!(ops.get(idx), Some(Operation::Read(_)))
}

#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
impl I2c for I2cMasterDevice {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.check(address)?;
        let mut idx = 0;
        while idx < operations.len() {
            let run = run_len_ops(operations, idx);
            if run == 0 {
                let Operation::Read(buf) = &mut operations[idx] else {
                    unreachable!("run_len_ops returned 0 at a non-Read op")
                };
                self.receive(buf, OP_TIMEOUT_MS)?;
                idx += 1;
                continue;
            }
            if read_at(operations, idx + run) {
                // (Write…Write run) → Read: ONE repeated-START
                // `i2c_master_transmit_receive` whose tx payload is the
                // CONCATENATED Write run — the legacy
                // `i2c_master_write_read_device` wire shape. Covers
                // embedded-hal `write_read` ([W,R]) and the general
                // [W…W,R] cmd-link shape alike.
                let (wr_ops, rest) = operations[idx..idx + run + 1].split_at_mut(run);
                let mut wbuf = [0u8; WBUF_SZ];
                let n = concat_writes(wr_ops, &mut wbuf)?;
                let Operation::Read(rbuf) = &mut rest[0] else {
                    unreachable!("shape pinned by read_at")
                };
                self.transmit_receive(&wbuf[..n], rbuf, OP_TIMEOUT_MS)?;
                idx += run + 1;
            } else if run == 1 {
                let Operation::Write(buf) = &operations[idx] else {
                    unreachable!("run_len_ops >= 1 at a non-Write op")
                };
                self.transmit(buf, OP_TIMEOUT_MS)?;
                idx += 1;
            } else {
                // Write run of 2+: ONE `i2c_master_transmit` carrying the
                // concatenated payloads — a single START…address…bytes…
                // STOP stream. Two standalone transmits would make the
                // MFRC522 re-parse payload bytes as register addresses
                // (#307 — mfrc522 `write_many` FIFO loads).
                let wr_ops = &operations[idx..idx + run];
                let mut wbuf = [0u8; WBUF_SZ];
                let n = concat_writes(wr_ops, &mut wbuf)?;
                self.transmit(&wbuf[..n], OP_TIMEOUT_MS)?;
                idx += run;
            }
        }
        Ok(())
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
}
