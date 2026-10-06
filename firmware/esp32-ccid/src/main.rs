#![cfg_attr(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    allow(unused_mut)
)]

#[cfg(all(
    feature = "backend-mfrc522",
    feature = "board-m5atom",
    feature = "board-m5stick"
))]
compile_error!("features board-m5atom and board-m5stick are mutually exclusive");
#[cfg(all(
    feature = "backend-mfrc522",
    feature = "board-nucula",
    any(feature = "board-m5atom", feature = "board-m5stick")
))]
compile_error!("feature board-nucula is mutually exclusive with board-m5atom/board-m5stick");
#[cfg(all(
    feature = "backend-mfrc522",
    not(any(
        feature = "board-m5atom",
        feature = "board-m5stick",
        feature = "board-nucula"
    ))
))]
compile_error!("select a board feature: board-m5atom (Grove SDA=26/SCL=32), board-m5stick (Grove SDA=32/SCL=33), or board-nucula (ESP32-C3, SDA=4/SCL=5)");

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
use core::convert::Infallible;
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
use esp32_ccid::{
    ccid_handler::CcidHandler,
    ccid_types::PC_TO_RDR_GET_SLOT_STATUS,
    nfc::NfcDriver,
    pn532_driver::Pn532NfcDriver,
    serial_framing::{
        build_nak_frame, build_response_frame, build_slot_change_notification, FrameEvent,
        FrameParser,
    },
};
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
use esp_idf_hal::{
    delay::FreeRtos,
    gpio::{self, AnyIOPin, PinDriver},
    peripherals::Peripherals,
    spi::{self, SpiDeviceDriver},
    uart::{self, config::DataBits, config::FlowControl, config::StopBits, UartDriver},
    units::Hertz,
};
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
use esp_idf_sys::EspError;
#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use mfrc522_pcd::recover_i2c_bus;

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522",
    feature = "ble"
))]
use esp32_ccid::{ble_debug::BleDebugServer, ble_logger::BleLogger};
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use esp32_ccid::{
    ccid_handler::CcidHandler,
    ccid_types::PC_TO_RDR_GET_SLOT_STATUS,
    nfc::NfcDriver,
    serial_framing::{
        build_nak_frame, build_response_frame, build_slot_change_notification, FrameEvent,
        FrameParser,
    },
};
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522",
    feature = "ble"
))]
use esp_idf_svc::{
    bt::{ble::gap::EspBleGap, ble::gatt::server::EspGatts, Ble, BtDriver},
    nvs::EspDefaultNvsPartition,
};
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522",
    feature = "ble"
))]
use std::sync::Arc;

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use esp_idf_hal::{
    delay::FreeRtos,
    gpio::AnyIOPin,
    i2c,
    peripherals::Peripherals,
    uart::{self, config::DataBits, config::FlowControl, config::StopBits, UartDriver},
    units::Hertz,
};
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use esp_idf_sys::EspError;

#[cfg(any(target_arch = "xtensa", target_arch = "riscv32"))]
const UART_RX_TIMEOUT_MS: u64 = 500;
#[cfg(any(target_arch = "xtensa", target_arch = "riscv32"))]
const CARD_POLL_INTERVAL_MS: u64 = 3000;
#[cfg(any(target_arch = "xtensa", target_arch = "riscv32"))]
const UART_BUF_SIZE: usize = 548;
#[cfg(any(target_arch = "xtensa", target_arch = "riscv32"))]
const MAX_FRAME_SIZE: usize = 274;
#[cfg(any(target_arch = "xtensa", target_arch = "riscv32"))]
const MAX_CCID_RESPONSE_SIZE: usize = 271;

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
struct IrqPin<'d>(PinDriver<'d, gpio::Input>);

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
impl embedded_hal::digital::ErrorType for IrqPin<'_> {
    type Error = Infallible;
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
impl embedded_hal::digital::InputPin for IrqPin<'_> {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0.is_high())
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0.is_low())
    }
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    any(feature = "backend-mfrc522", feature = "backend-pn532")
))]
fn write_all(uart: &UartDriver, mut bytes: &[u8]) -> Result<(), EspError> {
    while !bytes.is_empty() {
        let written = uart.write(bytes)?;
        if written == 0 {
            continue;
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    any(feature = "backend-mfrc522", feature = "backend-pn532")
))]
fn write_all_logged(uart: &UartDriver, bytes: &[u8]) {
    if let Err(e) = write_all(uart, bytes) {
        log::error!("UART write failed: {:?}", e);
    }
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
fn main() {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();
    // Note: with the default sdkconfig (CONFIG_ESP_CONSOLE_NONE=y) log output is
    // dropped — UART0 belongs to the CCID protocol. Logs surface in debug builds
    // (console enabled) and in the `ble` feature build (BLE log bridge).
    esp_idf_svc::log::EspLogger::initialize_default();
    log::set_max_level(log::LevelFilter::Debug);
    log::warn!("ESP32-CCID: rust main ALIVE (instrumented)");

    let peripherals = Peripherals::take().expect("ESP32 peripherals already taken");

    let uart_config = uart::config::Config::new()
        .baudrate(Hertz(115_200))
        .data_bits(DataBits::DataBits8)
        .stop_bits(StopBits::STOP2)
        .parity_none()
        .flow_control(FlowControl::None)
        .rx_fifo_size(UART_BUF_SIZE)
        .tx_fifo_size(UART_BUF_SIZE);

    let uart = UartDriver::new(
        peripherals.uart0,
        peripherals.pins.gpio1,
        peripherals.pins.gpio3,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &uart_config,
    )
    .expect("UART0 init failed (TX=GPIO1, RX=GPIO3)");

    let irq_pin = IrqPin(
        PinDriver::input(peripherals.pins.gpio16, gpio::Pull::Up)
            .expect("GPIO16 input init failed"),
    );
    let rst_pin = PinDriver::output(peripherals.pins.gpio26).expect("GPIO26 output init failed");

    let spi_config = spi::config::Config::new()
        .baudrate(Hertz(1_000_000).into())
        .data_mode(spi::config::MODE_0);

    let spi_device = SpiDeviceDriver::new_single(
        peripherals.spi2,
        peripherals.pins.gpio19,
        peripherals.pins.gpio17,
        Some(peripherals.pins.gpio18),
        Some(peripherals.pins.gpio25),
        &spi::SpiDriverConfig::new(),
        &spi_config,
    )
    .expect("SPI2 init failed");

    let mut pn532_driver =
        Pn532NfcDriver::new(spi_device, irq_pin, rst_pin).expect("PN532 driver init failed");

    let pn532_ok = (0..5).any(|_| {
        if pn532_driver.init().is_ok() {
            true
        } else {
            FreeRtos::delay_ms(1000);
            false
        }
    });

    // Reader-down visibility (bolty-rs docs/lessons-learned.md B5): without a
    // PN532 the CCID loop would run silently dead — worse, a blocking IRQ wait
    // in the transport could hang it forever now that the task WDT is off.
    // Halt CCID, hold LED Error, and log periodically instead.
    if !pn532_ok {
        // DevKitC targets have no LED matrix (the led module is mfrc522-gated);
        // visibility is log-only here.
        log::error!("PN532 init failed — CCID offline, halting card loop");
        let mut tick: u32 = 0;
        loop {
            if tick % 30 == 0 {
                log::warn!("reader absent ({}x5s) — CCID still offline", tick);
            }
            tick += 1;
            FreeRtos::delay_ms(5000);
        }
    }

    let mut ccid_handler = CcidHandler::new(pn532_driver);
    let mut frame_parser = FrameParser::new();
    let mut frame_buf = [0u8; MAX_FRAME_SIZE];
    let mut frame_len = 0usize;
    let mut byte_buf = [0u8; 1];
    let timeout_ticks = esp_idf_hal::delay::TickType::new_millis(UART_RX_TIMEOUT_MS).ticks();
    let poll_interval_ticks =
        esp_idf_hal::delay::TickType::new_millis(CARD_POLL_INTERVAL_MS).ticks() as u32;
    let mut last_card_poll_tick: u32 = unsafe { esp_idf_sys::xTaskGetTickCount() };

    // Purge any stale UART data from ESP-IDF boot log and PN532 init.
    // pcscd expects a clean protocol start (SYNC byte first).
    FreeRtos::delay_ms(500);
    uart.wait_tx_done(esp_idf_hal::delay::TickType::new_millis(100).into())
        .ok();
    let mut drain = [0u8; 256];
    loop {
        match uart.read(&mut drain, 1) {
            Ok(n) if n > 0 => continue,
            _ => break,
        }
    }

    loop {
        match uart.read(&mut byte_buf, timeout_ticks) {
            Ok(1) => {
                let byte = byte_buf[0];

                if frame_len < frame_buf.len() {
                    frame_buf[frame_len] = byte;
                    frame_len += 1;
                } else {
                    let mut nak = [0u8; 3];
                    let nak_len = build_nak_frame(&mut nak);
                    write_all_logged(&uart, &nak[..nak_len]);
                    ccid_handler.record_nak();
                    frame_len = 0;
                    frame_parser.reset();
                    continue;
                }

                match frame_parser.feed(byte) {
                    Some(FrameEvent::Command { ccid_bytes }) => {
                        // GemPC Twin protocol: echo → [NotifySlotChange] → response
                        write_all_logged(&uart, &frame_buf[..frame_len]);

                        // Time-gated card poll on GetSlotStatus only.
                        // InListPassiveTarget (PN532 UM §7.3.5) takes ~1s over SPI.
                        // libccidtwin readTimeout is 3s so this is safe.
                        let is_get_slot_status =
                            ccid_bytes.first() == Some(&PC_TO_RDR_GET_SLOT_STATUS);
                        if is_get_slot_status {
                            let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                            if now.wrapping_sub(last_card_poll_tick) >= poll_interval_ticks {
                                last_card_poll_tick = now;
                                if let Some(present) = ccid_handler.check_card_change() {
                                    let mut notif = [0u8; 2];
                                    let notif_len =
                                        build_slot_change_notification(present, &mut notif);
                                    write_all_logged(&uart, &notif[..notif_len]);
                                }
                            }
                        }

                        let mut resp_buf = [0u8; MAX_CCID_RESPONSE_SIZE];
                        let resp_len = ccid_handler.process_command(&ccid_bytes, &mut resp_buf);

                        let mut frame_out = [0u8; MAX_FRAME_SIZE];
                        let out_len = build_response_frame(&resp_buf[..resp_len], &mut frame_out);
                        write_all_logged(&uart, &frame_out[..out_len]);

                        frame_len = 0;
                        frame_parser.reset();
                    }
                    Some(FrameEvent::Error(_)) => {
                        let mut nak = [0u8; 3];
                        let nak_len = build_nak_frame(&mut nak);
                        write_all_logged(&uart, &nak[..nak_len]);
                        ccid_handler.record_nak();
                        frame_len = 0;
                        frame_parser.reset();
                    }
                    _ => {}
                }
            }
            _ => {
                frame_len = 0;
                frame_parser.reset();

                // Background card state tracking when UART is idle.
                // Only update internal state — do NOT send unsolicited
                // NotifySlotChange (pcscd's ReadSerial doesn't expect it).
                let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                if now.wrapping_sub(last_card_poll_tick) >= poll_interval_ticks {
                    last_card_poll_tick = now;
                    ccid_handler.check_card_change();
                }
            }
        }
    }
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
fn main() {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();
    // Note: with the default sdkconfig (CONFIG_ESP_CONSOLE_NONE=y) log output is
    // dropped — UART0 belongs to the CCID protocol. Logs surface in debug builds
    // (console enabled) and in the `ble` feature build (BLE log bridge).
    #[cfg(not(feature = "ble"))]
    esp32_ccid::netlog::init();
    #[cfg(feature = "ble")]
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take().expect("ESP32 peripherals already taken");

    // WiFi + OTA for serial-free bring-up (bench power bricks): the
    // modem is exclusive with the `ble` feature build, and credentials
    // come from NUCULA_WIFI_SSID/PASS at build time — absent creds
    // leave the board serial/CCID-only exactly as before.
    #[cfg(not(feature = "ble"))]
    match (
        option_env!("NUCULA_WIFI_SSID"),
        option_env!("NUCULA_WIFI_PASS"),
    ) {
        (Some(ssid), Some(pass)) => {
            let nvs = esp_idf_svc::nvs::EspDefaultNvsPartition::take().expect("nvs partition");
                match esp32_ccid::wifi::WifiManager::new(peripherals.modem, nvs) {
                    Ok(mut m) => match m.connect(ssid, pass) {
                        Ok(ip) => {
                            esp32_ccid::netlog::set_ip(&ip);
                            esp32_ccid::ota::spawn();
                        }
                        Err(e) => {
                            log::warn!("wifi: connect failed: {}", e);
                            m.log_visible_aps();
                        }
                    },
                    Err(e) => log::warn!("wifi: manager init failed: {}", e),
                }
        }
        _ => log::warn!("wifi: no credentials baked in - CCID only"),
    }

    #[cfg(all(feature = "backend-mfrc522", feature = "ble"))]
    let ble_server = (|| -> Result<BleDebugServer, EspError> {
        let nvs = EspDefaultNvsPartition::take().ok();
        let bt = Arc::new(BtDriver::<Ble>::new(peripherals.modem, nvs)?);
        let gap = Arc::new(EspBleGap::new(bt.clone())?);
        let gatts = Arc::new(EspGatts::new(bt.clone())?);
        let server = BleDebugServer::new(gap, gatts);
        server.subscribe()?;
        server.register_app()?;
        Ok(server)
    })()
    .ok();

    #[cfg(all(feature = "backend-mfrc522", feature = "ble"))]
    let _ = BleLogger::install();
    #[cfg(all(feature = "backend-mfrc522", feature = "ble"))]
    log::set_max_level(log::LevelFilter::Debug);
    // When BLE is disabled, suppress ALL log output — UART0 is reserved
    // exclusively for CCID serial protocol, no debug output allowed.
    #[cfg(not(all(feature = "backend-mfrc522", feature = "ble")))]
    log::set_max_level(log::LevelFilter::Info);
    #[cfg(all(feature = "backend-mfrc522", feature = "ble"))]
    log::info!("ESP32-CCID: BLE logger installed");
    #[cfg(all(feature = "backend-mfrc522", feature = "ble"))]
    if ble_server.is_some() {
        log::info!("ESP32-CCID: BLE server started, advertising");
    } else {
        log::warn!("ESP32-CCID: BLE server FAILED to start");
    }

    let uart_config = uart::config::Config::new()
        .baudrate(Hertz(115_200))
        .data_bits(DataBits::DataBits8)
        .stop_bits(StopBits::STOP2)
        .parity_none()
        .flow_control(FlowControl::None)
        .rx_fifo_size(UART_BUF_SIZE)
        .tx_fifo_size(UART_BUF_SIZE);

    // CCID serial protocol runs on UART0. M5 boards use the USB-UART bridge pins
    // (TX=GPIO1, RX=GPIO3); the ESP32-C3 nucula board has no UART bridge, so use
    // the C3 UART0 defaults (TX=GPIO21, RX=GPIO20). Console output on nucula is the
    // ROM USB-Serial/JTAG CDC on GPIO18/19 (see sdkconfig.defaults.esp32c3).
    #[cfg(feature = "board-nucula")]
    let (uart_tx, uart_rx) = (peripherals.pins.gpio21, peripherals.pins.gpio20);
    #[cfg(not(feature = "board-nucula"))]
    let (uart_tx, uart_rx) = (peripherals.pins.gpio1, peripherals.pins.gpio3);

    let uart = UartDriver::new(
        peripherals.uart0,
        uart_tx,
        uart_rx,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &uart_config,
    )
    .expect("UART0 init failed (CCID serial TX/RX)");
    log::warn!("bring-up: UART0 ok");
    log::warn!("bring-up: pins chosen, entering 50ms settle");
    #[cfg(target_arch = "riscv32")]
    log::warn!("bring-up: riscv32 skips i2c bus recovery (mfrc522-pcd xtensa-only)");

    // I2C pinout per board variant — see Cargo.toml [features].
    #[cfg(feature = "board-m5atom")]
    let (i2c_sda, i2c_scl, scl_gpio_no, sda_gpio_no) =
        (peripherals.pins.gpio26, peripherals.pins.gpio32, 32, 26);
    #[cfg(feature = "board-m5stick")]
    let (i2c_sda, i2c_scl, scl_gpio_no, sda_gpio_no) =
        (peripherals.pins.gpio32, peripherals.pins.gpio33, 33, 32);
    // ESP32-C3 nucula: PN7160 NFC on the Grove-free header, SDA=GPIO4 / SCL=GPIO5.
    // (PN7160 IRQ=GPIO6, VEN=GPIO7 — wired for the later PN7160 backend phase.)
    #[cfg(feature = "board-nucula")]
    let (i2c_sda, i2c_scl, scl_gpio_no, sda_gpio_no) =
        (peripherals.pins.gpio4, peripherals.pins.gpio5, 5, 4);

    #[cfg(target_arch = "xtensa")]
    recover_i2c_bus(scl_gpio_no, sda_gpio_no);
    // Mirror bolty's proven bring-up (bolty-rs apps/bolty-esp32): 50 ms settle
    // after recovery, i2c0 via GPIO matrix, and a bus probe before the first
    // MFRC522 register access. 400 kHz (was 100 kHz): this board was marginal
    // at 400 kHz in early bring-up (worked intermittently, then hung the
    // first transaction) — re-attempted per #59 with a measured RTT table
    // and a 100-transaction soak before landing.
    log::warn!("bring-up: skipping vTaskDelay on riscv32 (crash probe)");
    #[cfg(target_arch = "xtensa")]
    FreeRtos::delay_ms(50);
    let i2c_config = i2c::config::Config::new().baudrate(Hertz(400_000).into());
    log::warn!("bring-up: calling I2cDriver::new");
    let mut i2c = i2c::I2cDriver::new(peripherals.i2c0, i2c_sda, i2c_scl, &i2c_config)
        .expect("I2C0 init failed");
    log::warn!("bring-up: I2C0 ok");

    let probe_timeout = esp_idf_hal::delay::TickType::new_millis(100);
    let probe_found = i2c.write(0x28, &[], probe_timeout.into()).is_ok();
    log::info!(
        "i2c probe @0x28: {}",
        if probe_found { "ack" } else { "no-ack" }
    );

    let mfrc522_result =
        mfrc522::Mfrc522::new(mfrc522::comm::blocking::i2c::I2cInterface::new(i2c, 0x28)).init();
    log::info!(
        "MFRC522 init: {:?}",
        mfrc522_result.as_ref().err().map(|e| format!("{e:?}"))
    );

    let mut led = esp32_ccid::led::LedStatus::new();

    let mfrc522_hw = match mfrc522_result {
        Ok(hw) => hw,
        Err(e) => {
            // No reader frontend: CCID cannot be serviced. Stay visible instead of
            // silently dead — LED in Error state and a periodic log line (bolty-rs
            // docs/lessons-learned.md B5: log-and-continue hides months-long outages;
            // here the reader is load-bearing so CCID halts but keeps signalling).
            log::error!("MFRC522 init failed ({e:?}) — CCID offline, LED=Error, halting card loop");
            let mut tick: u32 = 0;
            loop {
                led.set_state(esp32_ccid::led::LedState::Error);
                if tick % 30 == 0 {
                    log::warn!("reader absent ({}x5s) — CCID still offline", tick);
                }
                tick += 1;
                FreeRtos::delay_ms(5000);
            }
        }
    };

    let transceiver = mfrc522_pcd::Mfrc522Transceiver::new(mfrc522_hw);
    let mut mfrc522_driver = esp32_ccid::mfrc522_driver::Mfrc522NfcDriver::new(transceiver);

    let init_ok = (0..5).any(|_| {
        if mfrc522_driver.init().is_ok() {
            true
        } else {
            FreeRtos::delay_ms(1000);
            false
        }
    });

    if init_ok {
        led.blink_state(esp32_ccid::led::LedState::Ready, 3, 150, 100);
    } else {
        led.set_state(esp32_ccid::led::LedState::Error);
    }

    let mut ccid_handler = CcidHandler::new(mfrc522_driver);
    let mut frame_parser = FrameParser::new();
    let mut frame_buf = [0u8; MAX_FRAME_SIZE];
    let mut frame_len = 0usize;
    let mut byte_buf = [0u8; 1];
    let timeout_ticks = esp_idf_hal::delay::TickType::new_millis(UART_RX_TIMEOUT_MS).ticks();
    let poll_interval_ticks =
        esp_idf_hal::delay::TickType::new_millis(CARD_POLL_INTERVAL_MS).ticks() as u32;
    let mut last_card_poll_tick: u32 = unsafe { esp_idf_sys::xTaskGetTickCount() };

    FreeRtos::delay_ms(500);
    uart.wait_tx_done(esp_idf_hal::delay::TickType::new_millis(100).into())
        .ok();
    let mut drain = [0u8; 256];
    loop {
        match uart.read(&mut drain, 1) {
            Ok(n) if n > 0 => continue,
            _ => break,
        }
    }

    loop {
        match uart.read(&mut byte_buf, timeout_ticks) {
            Ok(1) => {
                let byte = byte_buf[0];
                if frame_len < frame_buf.len() {
                    frame_buf[frame_len] = byte;
                    frame_len += 1;
                } else {
                    let mut nak = [0u8; 3];
                    let nak_len = build_nak_frame(&mut nak);
                    write_all_logged(&uart, &nak[..nak_len]);
                    ccid_handler.record_nak();
                    frame_len = 0;
                    frame_parser.reset();
                    continue;
                }
                match frame_parser.feed(byte) {
                    Some(FrameEvent::Command { ccid_bytes }) => {
                        write_all_logged(&uart, &frame_buf[..frame_len]);
                        let is_get_slot_status =
                            ccid_bytes.first() == Some(&PC_TO_RDR_GET_SLOT_STATUS);
                        if is_get_slot_status {
                            let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                            if now.wrapping_sub(last_card_poll_tick) >= poll_interval_ticks {
                                last_card_poll_tick = now;
                                if let Some(present) = ccid_handler.check_card_change() {
                                    if present {
                                        led.blink_state(
                                            esp32_ccid::led::LedState::CardPresent,
                                            3,
                                            120,
                                            80,
                                        );
                                    } else {
                                        led.blink_state(
                                            esp32_ccid::led::LedState::Ready,
                                            3,
                                            120,
                                            80,
                                        );
                                    }
                                    let mut notif = [0u8; 2];
                                    let notif_len =
                                        build_slot_change_notification(present, &mut notif);
                                    write_all_logged(&uart, &notif[..notif_len]);
                                }
                            }
                        }
                        let prev_led = led.state();
                        led.set_state(esp32_ccid::led::LedState::TxRx);
                        let mut resp_buf = [0u8; MAX_CCID_RESPONSE_SIZE];
                        let resp_len = ccid_handler.process_command(&ccid_bytes, &mut resp_buf);
                        led.set_state(prev_led);
                        let mut frame_out = [0u8; MAX_FRAME_SIZE];
                        let out_len = build_response_frame(&resp_buf[..resp_len], &mut frame_out);
                        write_all_logged(&uart, &frame_out[..out_len]);
                        frame_len = 0;
                        frame_parser.reset();

                        // Drain BLE logs after every command (not just on timeout)
                        #[cfg(all(feature = "backend-mfrc522", feature = "ble"))]
                        if let Some(server) = ble_server.as_ref() {
                            BleLogger::global().drain(server);
                        }
                    }
                    Some(FrameEvent::Error(_)) => {
                        led.set_state(esp32_ccid::led::LedState::Error);
                        let mut nak = [0u8; 3];
                        let nak_len = build_nak_frame(&mut nak);
                        write_all_logged(&uart, &nak[..nak_len]);
                        ccid_handler.record_nak();
                        frame_len = 0;
                        frame_parser.reset();
                    }
                    _ => {}
                }
            }
            _ => {
                frame_len = 0;
                frame_parser.reset();

                #[cfg(all(feature = "backend-mfrc522", feature = "ble"))]
                if let Some(server) = ble_server.as_ref() {
                    BleLogger::global().drain(server);
                }

                let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                if now.wrapping_sub(last_card_poll_tick) >= poll_interval_ticks {
                    last_card_poll_tick = now;
                    if let Some(present) = ccid_handler.check_card_change() {
                        if present {
                            led.blink_state(esp32_ccid::led::LedState::CardPresent, 3, 120, 80);
                        } else {
                            led.blink_state(esp32_ccid::led::LedState::Ready, 3, 120, 80);
                        }
                        let mut notif = [0u8; 2];
                        let notif_len = build_slot_change_notification(present, &mut notif);
                        write_all_logged(&uart, &notif[..notif_len]);
                    }
                }
            }
        }
    }
}

#[cfg(any(
    not(any(target_arch = "xtensa", target_arch = "riscv32")),
    all(
        not(feature = "backend-pn532"),
        not(feature = "backend-mfrc522"),
        not(feature = "backend-pn7160")
    )
))]
fn main() {}

// ---------------------------------------------------------------------------
// PAD-DIAGNOSTIC PROBE (v53) — settle the GPIO4-7 JTAG-pad question.
//
// Evidence chain: PN7160 unpowered under Rust despite VEN(GPIO7) latch=high;
// phantom I2C ACKs on SDA(GPIO4)/SCL(GPIO5); GPIO6 reads floating. Per the
// ESP32-C3 pin tables, GPIO4-7 = JTAG pads MTMS/MTDI/MTCK/MTDO whose IO_MUX
// RESET-DEFAULT function is JTAG (Function 0); GPIO is Function 1 and IDF's
// gpio driver must flip MCU_SEL per-pad. This probe reads ground truth:
// raw IO_MUX registers (base 0x60009000, one 32-bit reg per pad) + real pad
// levels (input-buffer-ON readback) before and after gpio_config.
//
// Decision tree:
//   mux[4-7] @ boot show FUNC=JTAG + post-config MCU_SEL never flips to GPIO
//     -> JTAG claim confirmed, find the claimant.
//   GPIO7 drive-high reads back 0 ("PAD STUCK")
//     -> pad interference confirmed regardless of mux decode.
//   All pads OK + VEN high + IRQ(6) drops to 0 in heartbeat
//     -> PN7160 IS alive under Rust; earlier failures were config-order.
//   All pads OK + VEN high + IRQ stays 1/floating
//     -> JTAG theory dead for VEN; pivot to VEN-timing/DWL-coupling.
// ---------------------------------------------------------------------------
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-pn7160",
    not(feature = "pn7160-bringup")
))]
mod pad_diag {
    const IO_MUX_BASE: *mut u32 = 0x60009000 as *mut u32;

    fn mux(pin: usize) -> u32 {
        unsafe { core::ptr::read_volatile(IO_MUX_BASE.add(pin)) }
    }

    fn to_input(pin: i32) {
        unsafe {
            let mut cfg: esp_idf_sys::gpio_config_t = core::mem::zeroed();
            cfg.pin_bit_mask = 1u64 << pin;
            cfg.mode = esp_idf_sys::gpio_mode_t_GPIO_MODE_INPUT;
            esp_idf_sys::gpio_config(&cfg);
        }
    }

    pub fn run() -> ! {
        esp_idf_sys::link_patches();
        esp_idf_svc::log::EspLogger::initialize_default();
        log::set_max_level(log::LevelFilter::Debug);
        log::warn!("pad-diag: rust main ALIVE (v53)");

        // 1. Boot-default IO_MUX dump — before touching any pad.
        for pin in 0..22usize {
            log::warn!("IO_MUX[{:02}] = 0x{:08X}", pin, mux(pin));
        }

        // 2. Pad input levels at boot (meaningful where IE=1).
        let mut levels = String::new();
        for pin in 0..22i32 {
            let lvl = unsafe { esp_idf_sys::gpio_get_level(pin) };
            levels.push_str(&format!(
                "{}{}",
                if lvl == 1 { "H" } else { "L" },
                if pin == 21 { "" } else { "," }
            ));
        }
        log::warn!("pads@boot: {}", levels);

        // 3. Drive/read test on the four NFC/I2C pads (INPUT_OUTPUT: real
        //    pad readback, not latch readback).
        for pin in [4i32, 5, 6, 7] {
            unsafe {
                let mut cfg: esp_idf_sys::gpio_config_t = core::mem::zeroed();
                cfg.pin_bit_mask = 1u64 << pin;
                cfg.mode = esp_idf_sys::gpio_mode_t_GPIO_MODE_INPUT_OUTPUT;
                let rc = esp_idf_sys::gpio_config(&cfg);
                esp_idf_sys::gpio_set_level(pin, 0);
                esp_idf_hal::delay::Ets::delay_us(1000);
                let lo = esp_idf_sys::gpio_get_level(pin);
                esp_idf_sys::gpio_set_level(pin, 1);
                esp_idf_hal::delay::Ets::delay_us(1000);
                let hi = esp_idf_sys::gpio_get_level(pin);
                log::warn!(
                    "pad {}: cfg_rc={} mux=0x{:08X} L->{} H->{} {}",
                    pin,
                    rc,
                    mux(pin as usize),
                    lo,
                    hi,
                    if hi == 1 && lo == 0 {
                        "[PAD OK]"
                    } else {
                        "[PAD STUCK!!!]"
                    }
                );
            }
        }

        // 4. Post-config mux for the four pads: did MCU_SEL flip to GPIO?
        for pin in 4..8usize {
            log::warn!("IO_MUX[{}] after cfg = 0x{:08X}", pin, mux(pin));
        }

        // 5. Restore observation posture: 4/5/6 passive inputs (6 = PN7160
        //    IRQ line, driven LOW by the chip when powered), 7 (VEN) stays
        //    driven HIGH — if pads work, the chip powers on NOW.
        to_input(4);
        to_input(5);
        to_input(6);
        unsafe {
            esp_idf_sys::gpio_set_level(7, 1);
        }
        log::warn!("pad-diag: VEN held high; watching IRQ(6) — 0 = PN7160 ALIVE");

        let mut tick: u32 = 0;
        loop {
            esp_idf_hal::delay::Ets::delay_us(5_000_000);
            tick += 1;
            let irq = unsafe { esp_idf_sys::gpio_get_level(6) };
            let ven = unsafe { esp_idf_sys::gpio_get_level(7) };
            log::warn!(
                "pad-diag: hb {} ven_pad={} irq_pad={} mux7=0x{:08X}",
                tick,
                ven,
                irq,
                mux(7)
            );
        }
    }
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-pn7160",
    not(feature = "pn7160-bringup"),
    not(feature = "pn7160-ccid")
))]
fn main() {
    crate::pad_diag::run()
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "pn7160-bringup"
))]
fn main() {
    esp32_ccid::pn7160_bringup::run()
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "pn7160-ccid"
))]
fn main() {
    esp32_ccid::pn7160_ccid::run()
}
