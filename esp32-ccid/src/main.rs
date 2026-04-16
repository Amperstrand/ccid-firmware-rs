#![cfg_attr(target_arch = "xtensa", allow(unused_mut))]

#[cfg(target_arch = "xtensa")]
use core::convert::Infallible;
#[cfg(target_arch = "xtensa")]
use esp32_ccid::{
    ccid_handler::CcidHandler,
    nfc::NfcDriver,
    pn532_driver::Pn532NfcDriver,
    serial_framing::{
        build_nak_frame, build_response_frame, build_slot_change_notification, FrameEvent,
        FrameParser,
    },
};
#[cfg(target_arch = "xtensa")]
use esp_idf_hal::{
    delay::{FreeRtos, TickType},
    gpio::{self, AnyIOPin, PinDriver},
    peripherals::Peripherals,
    prelude::*,
    spi::{self, SpiDeviceDriver},
    uart::{self, config::DataBits, config::FlowControl, config::StopBits, UartDriver},
};
#[cfg(target_arch = "xtensa")]
use esp_idf_sys::{self, EspError, ESP_ERR_TIMEOUT};
#[cfg(target_arch = "xtensa")]
const UART_RX_TIMEOUT_MS: u64 = 500;
#[cfg(target_arch = "xtensa")]
const UART_MIN_RX_BUFFER_SIZE: usize = 548;
#[cfg(target_arch = "xtensa")]
const MAX_FRAME_SIZE: usize = 274;
#[cfg(target_arch = "xtensa")]
const MAX_CCID_RESPONSE_SIZE: usize = 271;

#[cfg(target_arch = "xtensa")]
struct IrqPin<'d, T: gpio::InputPin>(PinDriver<'d, T, gpio::Input>);

#[cfg(target_arch = "xtensa")]
impl<'d, T: gpio::InputPin> embedded_hal::digital::ErrorType for IrqPin<'d, T> {
    type Error = Infallible;
}

#[cfg(target_arch = "xtensa")]
impl<'d, T: gpio::InputPin> embedded_hal::digital::InputPin for IrqPin<'d, T> {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0.is_high())
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0.is_low())
    }
}

#[cfg(target_arch = "xtensa")]
fn write_all(uart: &UartDriver<'_>, mut bytes: &[u8]) -> Result<(), EspError> {
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
fn main() {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();

    let Some(peripherals) = Peripherals::take() else {
        eprintln!("Failed to take ESP32 peripherals");
        loop {
            FreeRtos::delay_ms(1000);
        }
    };

    let uart_config = uart::config::Config::new()
        .baudrate(Hertz(115_200))
        .data_bits(DataBits::DataBits8)
        .stop_bits(StopBits::STOP2)
        .parity_none()
        .flow_control(FlowControl::None)
        .rx_fifo_size(UART_MIN_RX_BUFFER_SIZE)
        .tx_fifo_size(UART_MIN_RX_BUFFER_SIZE);

    let uart = UartDriver::new(
        peripherals.uart0,
        peripherals.pins.gpio1,
        peripherals.pins.gpio3,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &uart_config,
    )
    .unwrap();

    let irq_pin = IrqPin(PinDriver::input(peripherals.pins.gpio16).unwrap());
    let rst_pin = PinDriver::output(peripherals.pins.gpio26).unwrap();

    let spi_config = spi::config::Config::new()
        .baudrate(1.MHz().into())
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
    .unwrap();

    let mut pn532_driver = Pn532NfcDriver::new(spi_device, irq_pin, rst_pin).unwrap();

    loop {
        match pn532_driver.init() {
            Ok(()) => break,
            Err(err) => {
                eprintln!("PN532 init failed: {:?}", err);
                FreeRtos::delay_ms(1000);
            }
        }
    }

    println!("ESP32 serial CCID reader initialized");

    let mut ccid_handler = CcidHandler::new(pn532_driver);
    let mut frame_parser = FrameParser::new();
    let mut frame_buf = [0u8; MAX_FRAME_SIZE];
    let mut frame_len = 0usize;
    let mut byte_buf = [0u8; 1];
    let timeout_ticks = TickType::new_millis(UART_RX_TIMEOUT_MS).ticks();

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
                    if let Err(err) = write_all(&uart, &nak[..nak_len]) {
                        eprintln!("UART write error after frame overflow: {:?}", err);
                    }
                    frame_len = 0;
                    frame_parser.reset();
                    continue;
                }

                match frame_parser.feed(byte) {
                    Some(FrameEvent::Command { ccid_bytes }) => {
                        if let Err(err) = write_all(&uart, &frame_buf[..frame_len]) {
                            eprintln!("UART echo write error: {:?}", err);
                        }

                        if let Some(present) = ccid_handler.check_card_change() {
                            let mut notif = [0u8; 2];
                            let notif_len = build_slot_change_notification(present, &mut notif);
                            if let Err(err) = write_all(&uart, &notif[..notif_len]) {
                                eprintln!("UART notify write error: {:?}", err);
                            }
                        }

                        let mut resp_buf = [0u8; MAX_CCID_RESPONSE_SIZE];
                        let resp_len = ccid_handler.process_command(&ccid_bytes, &mut resp_buf);

                        let mut frame_out = [0u8; MAX_FRAME_SIZE];
                        let out_len = build_response_frame(&resp_buf[..resp_len], &mut frame_out);
                        if let Err(err) = write_all(&uart, &frame_out[..out_len]) {
                            eprintln!("UART response write error: {:?}", err);
                        }

                        frame_len = 0;
                        frame_parser.reset();
                    }
                    Some(FrameEvent::Error(err)) => {
                        eprintln!("Frame parse error: {:?}", err);
                        let mut nak = [0u8; 3];
                        let nak_len = build_nak_frame(&mut nak);
                        if let Err(write_err) = write_all(&uart, &nak[..nak_len]) {
                            eprintln!("UART NAK write error: {:?}", write_err);
                        }
                        frame_len = 0;
                        frame_parser.reset();
                    }
                    None => {}
                }
            }
            Ok(_) => {}
            Err(err) if err.code() == ESP_ERR_TIMEOUT => {
                let _ = ccid_handler.check_card_change();
                frame_len = 0;
                frame_parser.reset();
            }
            Err(err) => {
                eprintln!("UART read error: {:?}", err);
                let _ = ccid_handler.check_card_change();
                frame_len = 0;
                frame_parser.reset();
            }
        }
    }
}

#[cfg(not(target_arch = "xtensa"))]
fn main() {}
