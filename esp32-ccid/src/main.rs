#![cfg_attr(target_arch = "xtensa", allow(unused_mut))]

#[cfg(all(
    target_arch = "xtensa",
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
use core::convert::Infallible;
#[cfg(all(
    target_arch = "xtensa",
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
    target_arch = "xtensa",
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
    target_arch = "xtensa",
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
use esp_idf_sys::EspError;

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use esp32_ccid::{
    ble_debug::BleDebugServer,
    ble_logger::BleLogger,
    ccid_handler::CcidHandler,
    ccid_types::PC_TO_RDR_GET_SLOT_STATUS,
    nfc::NfcDriver,
    serial_framing::{
        build_nak_frame, build_response_frame, build_slot_change_notification, FrameEvent,
        FrameParser,
    },
};
#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use esp_idf_svc::{
    bt::{ble::gap::EspBleGap, ble::gatt::server::EspGatts, Ble, BtDriver},
    nvs::EspDefaultNvsPartition,
};
#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use std::sync::Arc;

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use esp_idf_hal::{
    delay::FreeRtos,
    gpio::AnyIOPin,
    i2c,
    peripherals::Peripherals,
    uart::{self, config::DataBits, config::FlowControl, config::StopBits, UartDriver},
    units::Hertz,
};
#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
use esp_idf_sys::EspError;

#[cfg(target_arch = "xtensa")]
const UART_RX_TIMEOUT_MS: u64 = 500;
#[cfg(target_arch = "xtensa")]
const CARD_POLL_INTERVAL_MS: u64 = 3000;
#[cfg(target_arch = "xtensa")]
const UART_BUF_SIZE: usize = 548;
#[cfg(target_arch = "xtensa")]
const MAX_FRAME_SIZE: usize = 274;
#[cfg(target_arch = "xtensa")]
const MAX_CCID_RESPONSE_SIZE: usize = 271;

#[cfg(all(
    target_arch = "xtensa",
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
struct IrqPin<'d>(PinDriver<'d, gpio::Input>);

#[cfg(all(
    target_arch = "xtensa",
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
impl embedded_hal::digital::ErrorType for IrqPin<'_> {
    type Error = Infallible;
}

#[cfg(all(
    target_arch = "xtensa",
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

#[cfg(target_arch = "xtensa")]
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

#[cfg(target_arch = "xtensa")]
fn write_all_logged(uart: &UartDriver, bytes: &[u8]) {
    if let Err(e) = write_all(uart, bytes) {
        log::error!("UART write failed: {:?}", e);
    }
}

#[cfg(all(
    target_arch = "xtensa",
    feature = "backend-pn532",
    not(feature = "backend-mfrc522")
))]
fn main() {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();

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

    let _pn532_ok = (0..5).any(|_| {
        if pn532_driver.init().is_ok() {
            true
        } else {
            FreeRtos::delay_ms(1000);
            false
        }
    });

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

#[cfg(all(target_arch = "xtensa", feature = "backend-mfrc522"))]
fn main() {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();

    let peripherals = Peripherals::take().expect("ESP32 peripherals already taken");

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

    let _ = BleLogger::install();
    log::set_max_level(log::LevelFilter::Debug);
    log::info!("ESP32-CCID: BLE logger installed");
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

    let uart = UartDriver::new(
        peripherals.uart0,
        peripherals.pins.gpio1,
        peripherals.pins.gpio3,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &uart_config,
    )
    .expect("UART0 init failed (TX=GPIO1, RX=GPIO3)");

    let i2c_config = i2c::config::Config::new().baudrate(Hertz(400_000).into());
    let i2c = i2c::I2cDriver::new(
        peripherals.i2c1,
        peripherals.pins.gpio26,
        peripherals.pins.gpio32,
        &i2c_config,
    )
    .expect("I2C1 init failed (SDA=GPIO26, SCL=GPIO32)");

    let mfrc522_result =
        mfrc522::Mfrc522::new(mfrc522::comm::blocking::i2c::I2cInterface::new(i2c, 0x28)).init();

    let mut led = esp32_ccid::led::LedStatus::new();

    let mfrc522_hw = match mfrc522_result {
        Ok(hw) => hw,
        Err(_) => {
            led.set_state(esp32_ccid::led::LedState::Error);
            loop {
                FreeRtos::delay_ms(5000);
            }
        }
    };

    let transceiver = esp32_ccid::mfrc522_transceiver::Mfrc522Transceiver::new(mfrc522_hw);
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
                        if let Some(server) = ble_server.as_ref() {
                            BleLogger::global().drain(server);
                        }
                    }
                    Some(FrameEvent::Error(_)) => {
                        led.set_state(esp32_ccid::led::LedState::Error);
                        let mut nak = [0u8; 3];
                        let nak_len = build_nak_frame(&mut nak);
                        write_all_logged(&uart, &nak[..nak_len]);
                        frame_len = 0;
                        frame_parser.reset();
                    }
                    _ => {}
                }
            }
            _ => {
                frame_len = 0;
                frame_parser.reset();

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
    not(target_arch = "xtensa"),
    all(not(feature = "backend-pn532"), not(feature = "backend-mfrc522"))
))]
fn main() {}
