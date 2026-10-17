//! #149 P3 — `embedded_hal::i2c::I2c` (1.0) adapter over the raw
//! `i2c_master` driver_ng FFI.
//!
//! The pinned esp-idf-hal generation does not wrap driver_ng (issues
//! #63/#80), which is why `backend-mfrc522` still rides the legacy
//! driver behind the vendored-hal `legacy-i2c` gate. This adapter is
//! the convergence piece: one I2C driver generation for both projects
//! (ccid + bolty-rs), the retirement path for the vendor patch, and
//! IDF-6 pre-alignment (legacy driver removed there).
//!
//! Skeleton scope (#149): the trait impl + the host-testable pure
//! mapping (esp-idf error codes → `embedded_hal::i2c::ErrorKind`,
//! address-vs-handle validation). The `backend-mfrc522` migration and
//! the vendor-patch retirement are bench-gated follow-ups per the
//! issue — the adapter lands first, proven, then consumers move.
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

/// Pure, host-testable: does position `idx` begin a fuseable
/// Write→Read pair? embedded-hal 1.0 `write_read(addr, w, r)`
/// dispatches to `transaction(addr, [Write(w), Read(r)])`, and the
/// adapter fuses exactly that pair into ONE repeated-START
/// `i2c_master_transmit_receive` — the wire shape the legacy
/// esp-idf-hal `I2cDriver::write_read` produced
/// (`i2c_master_write_read_device`), which MFRC522/AXP192 register
/// reads and the bench 0xD3 cadence compare rely on. A lone Write (or
/// `[Write, Write]` — mfrc522 `write_many`) stays on plain
/// `i2c_master_transmit`, also the legacy shape.
pub fn fuses_write_read(kinds: &[OpKind], idx: usize) -> bool {
    kinds.get(idx) == Some(&OpKind::Write) && kinds.get(idx + 1) == Some(&OpKind::Read)
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
            // Fusion decision computed from immutable looks first, so
            // the split_at_mut below gets disjoint mutable borrows.
            let fused = match operations.get(idx..idx + 2) {
                Some(pair) => {
                    let kinds = [op_kind(&pair[0]), op_kind(&pair[1])];
                    fuses_write_read(&kinds, 0)
                }
                None => false,
            };
            if fused {
                let (head, tail) = operations[idx..].split_at_mut(1);
                let Operation::Write(wbuf) = &head[0] else {
                    unreachable!("shape pinned by fuses_write_read")
                };
                let Operation::Read(rbuf) = &mut tail[0] else {
                    unreachable!("shape pinned by fuses_write_read")
                };
                self.transmit_receive(wbuf, rbuf, OP_TIMEOUT_MS)?;
                idx += 2;
            } else {
                match &mut operations[idx] {
                    Operation::Write(buf) => self.transmit(buf, OP_TIMEOUT_MS)?,
                    Operation::Read(buf) => self.receive(buf, OP_TIMEOUT_MS)?,
                }
                idx += 1;
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
    fn write_read_pair_fuses_but_plain_sequences_do_not() {
        // embedded-hal 1.0 `write_read(addr, w, r)` dispatches to
        // `transaction(addr, [Write(w), Read(r)])` — the adapter must
        // fuse that pair into ONE repeated-START transmit_receive
        // (legacy `write_read` wire shape), while `[Write, Write]`
        // (mfrc522 `write_many`) and lone ops stay on plain
        // transmit/receive. Locked here because the bench 0xD3 cadence
        // compare reads the difference on the wire.
        use super::{fuses_write_read, OpKind};

        assert!(fuses_write_read(&[OpKind::Write, OpKind::Read], 0));
        assert!(!fuses_write_read(&[OpKind::Write, OpKind::Write], 0));
        assert!(!fuses_write_read(&[OpKind::Read, OpKind::Write], 0));
        assert!(!fuses_write_read(&[OpKind::Write], 0));
        assert!(!fuses_write_read(&[OpKind::Read], 0));
        // A Write after a fused pair is judged fresh at its own index.
        assert!(fuses_write_read(
            &[OpKind::Read, OpKind::Write, OpKind::Read],
            1
        ));
    }
}
