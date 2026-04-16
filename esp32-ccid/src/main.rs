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
    delay::FreeRtos,
    gpio::{self, AnyIOPin, PinDriver},
    peripherals::Peripherals,
    spi::{self, SpiDeviceDriver},
    uart::{self, config::DataBits, config::FlowControl, config::StopBits, UartDriver},
    units::Hertz,
};
#[cfg(target_arch = "xtensa")]
use esp_idf_sys::EspError;

#[cfg(target_arch = "xtensa")]
const UART_RX_TIMEOUT_MS: u64 = 500;
#[cfg(target_arch = "xtensa")]
const UART_BUF_SIZE: usize = 548;
#[cfg(target_arch = "xtensa")]
const MAX_FRAME_SIZE: usize = 274;
#[cfg(target_arch = "xtensa")]
const MAX_CCID_RESPONSE_SIZE: usize = 271;

#[cfg(target_arch = "xtensa")]
struct IrqPin<'d>(PinDriver<'d, gpio::Input>);

#[cfg(target_arch = "xtensa")]
impl embedded_hal::digital::ErrorType for IrqPin<'_> {
    type Error = Infallible;
}

#[cfg(target_arch = "xtensa")]
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
fn main() {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();

    let peripherals = Peripherals::take().unwrap();

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
    .unwrap();

    let irq_pin = IrqPin(PinDriver::input(peripherals.pins.gpio16, gpio::Pull::Down).unwrap());
    let rst_pin = PinDriver::output(peripherals.pins.gpio26).unwrap();

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
    .unwrap();

    let mut pn532_driver = Pn532NfcDriver::new(spi_device, irq_pin, rst_pin).unwrap();

    let mut pn532_ok = false;
    for _ in 0..5 {
        match pn532_driver.init() {
            Ok(()) => {
                pn532_ok = true;
                break;
            }
            Err(_) => {
                FreeRtos::delay_ms(1000);
            }
        }
    }

    let mut ccid_handler = CcidHandler::new(pn532_driver);
    let mut frame_parser = FrameParser::new();
    let mut frame_buf = [0u8; MAX_FRAME_SIZE];
    let mut frame_len = 0usize;
    let mut byte_buf = [0u8; 1];
    let timeout_ticks = esp_idf_hal::delay::TickType::new_millis(UART_RX_TIMEOUT_MS).ticks();

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
                    let _ = write_all(&uart, &nak[..nak_len]);
                    frame_len = 0;
                    frame_parser.reset();
                    continue;
                }

                match frame_parser.feed(byte) {
                    Some(FrameEvent::Command { ccid_bytes }) => {
                        let _ = write_all(&uart, &frame_buf[..frame_len]);

                        if let Some(present) = ccid_handler.check_card_change() {
                            let mut notif = [0u8; 2];
                            let notif_len = build_slot_change_notification(present, &mut notif);
                            let _ = write_all(&uart, &notif[..notif_len]);
                        }

                        let mut resp_buf = [0u8; MAX_CCID_RESPONSE_SIZE];
                        let resp_len = ccid_handler.process_command(&ccid_bytes, &mut resp_buf);

                        let mut frame_out = [0u8; MAX_FRAME_SIZE];
                        let out_len = build_response_frame(&resp_buf[..resp_len], &mut frame_out);
                        let _ = write_all(&uart, &frame_out[..out_len]);

                        frame_len = 0;
                        frame_parser.reset();
                    }
                    Some(FrameEvent::Error(_)) => {
                        let mut nak = [0u8; 3];
                        let nak_len = build_nak_frame(&mut nak);
                        let _ = write_all(&uart, &nak[..nak_len]);
                        frame_len = 0;
                        frame_parser.reset();
                    }
                    None => {}
                }
            }
            _ => {
                frame_len = 0;
                frame_parser.reset();
            }
        }
    }
}

#[cfg(not(target_arch = "xtensa"))]
fn main() {}
