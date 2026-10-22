//! Layer-B frontend constructors (boards-as-data design §3.1,
//! kanban t_4864e74e): the per-board glue that reads a Layer-A
//! `boards::BoardDesc` table, constructs the embedded-hal bus, runs
//! the frontend crate's constructor + self-test, and wraps the driver
//! for the serving kernel. The firmware mains reduce to **board table
//! + constructor + service spawn** (the #383 shape).
//!
//! Two layers by design (§1.1): the table cannot hand out esp-idf-hal
//! type-state tokens, so the number→bus/token step lives HERE (Layer
//! B), consuming only numbers resolved from the table — never inline
//! pin data.

use boards::{BoardDesc, BusRole};

/// The NFC-frontend bus binding resolved from a `BoardDesc`:
/// controller port + raw GPIO numbers + controller address + run
/// speed — the numbers driver_ng claims the controller by (the #149
/// P3 shared adapter owns the bus, so no typed tokens are consumed).
///
/// Host-testable pure data: constructing it from a table can never
/// touch hardware, and the board tables themselves are host-tested in
/// `crates/boards`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NfcBusBinding {
    /// I2C controller port (`I2cMasterBus::new(port, ..)`).
    pub port: u8,
    /// SDA raw GPIO number.
    pub sda: i32,
    /// SCL raw GPIO number.
    pub scl: i32,
    /// NFC controller 7-bit I2C address.
    pub i2c_addr: u8,
    /// Bus run speed (Hz) the device claim uses.
    pub hz_run: u32,
}

impl NfcBusBinding {
    /// Resolve the NFC-frontend bus + controller facts from a board
    /// table (`BusRole::NfcFrontend` + `nfc`). Panics on a
    /// wired-contact-only table — those boards never build an NFC
    /// frontend, and a missing entry at a construction site is a
    /// programming error that must fail loudly, not serve a phantom
    /// reader.
    pub fn from_board(b: &BoardDesc) -> Self {
        let bus = b.bus(BusRole::NfcFrontend).i2c();
        let nfc = b.nfc.unwrap_or_else(|| {
            panic!(
                "board {} has no NFC frontend for BusRole::NfcFrontend",
                b.name
            )
        });
        Self {
            port: bus.port,
            sda: bus.sda as i32,
            scl: bus.scl as i32,
            i2c_addr: nfc.i2c_addr,
            hz_run: bus.hz_run,
        }
    }
}

/// The MFRC522 classic-main frontend: the driver wrapped exactly as
/// the mains wrap it today (`RetryFrontend` inside `EitherFrontend`,
/// fallback arm the `UnavailableNfcDriver` stand-in).
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
pub mod mfrc522 {
    use super::NfcBusBinding;
    use crate::frontend::{EitherFrontend, RetryFrontend, UnavailableNfcDriver};
    use crate::mfrc522_driver::Mfrc522NfcDriver;
    use pn7160_nci::hal_i2c::I2cMasterDevice;

    /// The constructed frontend + whether bring-up succeeded (boot
    /// init ok ⇒ `true`; hw-init failure ⇒ the fallback arm + `false`).
    pub type Frontend = EitherFrontend<
        RetryFrontend<Mfrc522NfcDriver<I2cMasterDevice>>,
        RetryFrontend<UnavailableNfcDriver>,
    >;

    /// MFRC522 bring-up over the shared driver_ng adapter (#149 P3),
    /// table-driven end to end (t_4864e74e): I2C bus recovery + settle
    /// (xtensa), master bus + probe, device claim at the table's
    /// address/speed, the `mfrc522_pcd` constructor (init + VersionReg
    /// self-test), the init-with-recovery ladder, and the
    /// fault-isolation wrap (#218). Behavior is byte-identical to the
    /// sequence this replaced in `main.rs`; only the home moved.
    pub fn bring_up(binding: NfcBusBinding) -> (Frontend, bool) {
        // I2C bus recovery (xtensa only — the riscv32 crash-probe path
        // skips it, mirroring the pre-construction main).
        #[cfg(target_arch = "xtensa")]
        {
            mfrc522_pcd::recover_i2c_bus(binding.scl, binding.sda);
            esp_idf_hal::delay::FreeRtos::delay_ms(50);
        }
        #[cfg(not(target_arch = "xtensa"))]
        log::warn!("bring-up: skipping vTaskDelay on riscv32 (crash probe)");

        log::warn!("bring-up: driver_ng i2c_new_master_bus");
        let mut i2c_bus =
            pn7160_nci::hal_i2c::I2cMasterBus::new(binding.port as i32, binding.sda, binding.scl)
                .expect("I2C0 init failed");
        log::warn!("bring-up: I2C0 ok (driver_ng)");

        // Probe at bus level before the first controller register
        // access (100 here is the probe TIMEOUT in ms, not a speed).
        let probe_found = i2c_bus.probe(binding.i2c_addr, 100).is_ok();
        log::info!(
            "i2c probe @{:#04x}: {}",
            binding.i2c_addr,
            if probe_found { "ack" } else { "no-ack" }
        );

        let i2c_dev = i2c_bus
            .add_hal_device(binding.i2c_addr, binding.hz_run)
            .expect("MFRC522 device add failed");

        // Constructor + self-test (t_4864e74e): init, antenna gain,
        // VersionReg gate — brought up inside the frontend crate.
        let hw = mfrc522_pcd::Mfrc522Transceiver::bring_up(
            i2c_dev,
            mfrc522_pcd::FrontendI2cBinding::new(binding.i2c_addr, binding.hz_run),
        );
        log::info!(
            "MFRC522 bring_up: {}",
            hw.as_ref()
                .map(|(_, v)| format!("self-test ok (version=0x{v:02X})"))
                .unwrap_or_else(|e| format!("{e:?}"))
        );

        // Frontend fault isolation (#218): a failed boot init serves
        // degraded (card-absent) and retries init on every
        // interval-gated presence poll; a hw-init failure cannot
        // construct the typestate at all, so the fallback arm serves
        // the stand-in.
        match hw {
            Ok((transceiver, _version)) => {
                let mut driver = Mfrc522NfcDriver::new(transceiver);

                let init_ok = (0..5).any(|_| {
                    // Routed through the recovery wrapper (Codex reviews
                    // on #43/#40/#38, P1): plain init() never reaches the
                    // failure tracker, so repeated startup failures could
                    // not trigger the self-healing full re-init.
                    if driver.init_with_recovery().is_ok() {
                        true
                    } else {
                        esp_idf_hal::delay::FreeRtos::delay_ms(1000);
                        false
                    }
                });

                if init_ok {
                    (
                        EitherFrontend::Primary(RetryFrontend::healthy(driver)),
                        true,
                    )
                } else {
                    log::error!(
                        "MFRC522 init failed — serving degraded (card absent), \
                         init retried on every presence poll"
                    );
                    (
                        EitherFrontend::Primary(RetryFrontend::degraded(driver)),
                        false,
                    )
                }
            }
            Err(e) => {
                log::error!(
                    "MFRC522 hw init failed ({e:?}) — no frontend; CCID stays up card-absent"
                );
                (
                    EitherFrontend::Fallback(RetryFrontend::degraded(UnavailableNfcDriver)),
                    false,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boards::{M5ATOM, M5STICK, NUCULA};

    /// The construction seam consumed by the m5stick pilot (t_4864e74e
    /// acceptance): the BoardDesc-derived binding equals today's
    /// bench wiring — Grove SDA=GPIO32/SCL=GPIO33, MFRC522 @ 0x28,
    /// 400 kHz run (bolty B2, bench-confirmed main.rs).
    #[test]
    fn m5stick_binding_matches_the_bench_wiring() {
        let b = NfcBusBinding::from_board(&M5STICK);
        assert_eq!(b.port, 0);
        assert_eq!(b.sda, 32);
        assert_eq!(b.scl, 33);
        assert_eq!(b.i2c_addr, 0x28);
        assert_eq!(b.hz_run, 400_000);
    }

    /// Same constructor, different table row: the whole point of
    /// boards-as-data (m5atom differs ONLY in pins/address facts).
    #[test]
    fn m5atom_binding_rides_the_same_constructor_path() {
        let b = NfcBusBinding::from_board(&M5ATOM);
        assert_eq!(b.port, 0);
        assert_eq!(b.sda, 26);
        assert_eq!(b.scl, 32);
        assert_eq!(b.i2c_addr, 0x28);
    }

    /// nucula reads the GENERATED table (design §2.1): 100 kHz run —
    /// the shared-bus PCF8574T limit (pn7160-nci board.rs rule).
    #[test]
    fn nucula_binding_reads_the_generated_table() {
        let b = NfcBusBinding::from_board(&NUCULA);
        assert_eq!(b.port, 0);
        assert_eq!(b.sda, 4);
        assert_eq!(b.scl, 5);
        assert_eq!(b.i2c_addr, 0x28);
        assert_eq!(b.hz_run, 100_000);
    }

    /// Wired-contact-only tables fail loudly at the construction seam
    /// (the card's DONE criterion, data side): an STM32 row has no NFC
    /// frontend bus, so the constructor must refuse, not fabricate one.
    #[test]
    #[should_panic(expected = "no bus for role NfcFrontend")]
    fn wired_contact_table_fails_loudly() {
        let _ = NfcBusBinding::from_board(&boards::F469);
    }
}
