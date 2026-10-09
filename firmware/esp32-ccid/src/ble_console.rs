//! One-call BLE debug console bring-up for firmware mains (issue #66).
//!
//! Splits the debug channel from the CCID wire: firmware logs ride BLE
//! GATT notifications (Nordic UART Service) while USB-CDC / UART0 carry
//! only CCID traffic. Any BT-capable ESP32 target (classic xtensa or
//! C3) built with the `ble` feature plus a `sdkconfig-*-ble.full`
//! config can use it:
//!
//! ```ignore
//! #[cfg(feature = "ble")]
//! let ble = esp32_ccid::ble_console::BleConsole::init(peripherals.modem, true);
//! // ... later, at safe points:
//! if let Some(ble) = ble.as_ref() { ble.drain(); }
//! ```
//!
//! Under `ble` the main must NOT call `EspLogger::initialize_default()`
//! — the log crate accepts one global logger, first installer wins, and
//! the console logger would permanently displace the BLE sink (the
//! latent bug that kept the original M5Stick build's BLE queue empty).

use std::sync::Arc;

use esp_idf_hal::modem::BluetoothModemPeripheral;
use esp_idf_svc::bt::ble::gap::EspBleGap;
use esp_idf_svc::bt::ble::gatt::server::EspGatts;
use esp_idf_svc::bt::{Ble, BtDriver};
use esp_idf_svc::nvs::EspDefaultNvsPartition;

use crate::ble_debug::BleDebugServer;
use crate::ble_logger::BleLogger;

pub struct BleConsole {
    server: BleDebugServer,
    _driver: Arc<BtDriver<'static, Ble>>,
}

impl BleConsole {
    /// Install the BLE logger as the global `log` sink, optionally
    /// silence ESP-IDF C-level logs (they bypass the Rust `log` crate
    /// and would otherwise still print on the CCID-shared console),
    /// then bring up the GATT server. `None` = bring-up failed; C-log
    /// silencing is reverted in that case so failures stay debuggable.
    pub fn init(
        modem: impl BluetoothModemPeripheral + 'static,
        silence_c_logs: bool,
    ) -> Option<Self> {
        let _ = BleLogger::install();
        log::set_max_level(log::LevelFilter::Debug);

        if silence_c_logs {
            set_c_log_level(CLogLevel::None);
        }

        let console = Self::bring_up(modem);
        if console.is_none() && silence_c_logs {
            set_c_log_level(CLogLevel::Warn);
        }
        console
    }

    fn bring_up(modem: impl BluetoothModemPeripheral + 'static) -> Option<Self> {
        // Raw println! (not log::) — when BLE is what's failing, the BLE
        // log sink is dead and the console is the only remaining channel.
        let nvs = EspDefaultNvsPartition::take().ok();
        let driver = Arc::new(unwrap_or_report(
            "BtDriver",
            BtDriver::<Ble>::new(modem, nvs),
        )?);
        let gap = Arc::new(unwrap_or_report(
            "EspBleGap",
            EspBleGap::new(driver.clone()),
        )?);
        let gatts = Arc::new(unwrap_or_report("EspGatts", EspGatts::new(driver.clone()))?);
        let server = BleDebugServer::new(gap, gatts);
        unwrap_or_report("subscribe", server.subscribe())?;
        unwrap_or_report("register_app", server.register_app())?;
        log::info!("ble: NUS debug console advertising");
        Some(Self {
            server,
            _driver: driver,
        })
    }

    /// Pump queued log lines into GATT notifications. No-op without a
    /// subscribed central.
    pub fn drain(&self) {
        BleLogger::global().drain(&self.server);
    }
}

enum CLogLevel {
    None,
    Warn,
}

fn unwrap_or_report<T>(where_: &'static str, r: Result<T, esp_idf_sys::EspError>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            println!("ble: bring-up FAILED at {where_}: {e}");
            None
        }
    }
}

fn set_c_log_level(level: CLogLevel) {
    use esp_idf_sys::{
        esp_log_level_set, esp_log_level_t_ESP_LOG_NONE, esp_log_level_t_ESP_LOG_WARN,
    };
    let raw = match level {
        CLogLevel::None => esp_log_level_t_ESP_LOG_NONE,
        CLogLevel::Warn => esp_log_level_t_ESP_LOG_WARN,
    };
    unsafe { esp_log_level_set(b"*\0".as_ptr().cast(), raw) };
}
