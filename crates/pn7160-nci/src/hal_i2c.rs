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

use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};

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
}

const OP_TIMEOUT_MS: i32 = 100;

#[cfg(all(feature = "esp", any(target_arch = "xtensa", target_arch = "riscv32")))]
impl I2c for I2cMasterDevice {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.check(address)?;
        for op in operations {
            match op {
                Operation::Write(buf) => self.transmit(buf, OP_TIMEOUT_MS)?,
                Operation::Read(buf) => self.receive(buf, OP_TIMEOUT_MS)?,
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
}
