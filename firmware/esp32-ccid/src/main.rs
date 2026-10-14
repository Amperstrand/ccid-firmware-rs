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
#[cfg(all(feature = "ble", feature = "spp"))]
compile_error!(
    "features ble and spp are mutually exclusive (both claim the single Bluetooth modem)"
);

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
use esp32_ccid::ble_console::BleConsole;
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use esp32_ccid::ccid_handler::CcidHandler;

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522"
))]
use esp_idf_hal::{delay::FreeRtos, i2c, peripherals::Peripherals, units::Hertz};

// UART0 CCID wiring — absent in `spp` builds (RFCOMM replaces it).
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522",
    not(feature = "spp")
))]
use esp_idf_hal::{
    gpio::AnyIOPin,
    uart::{self, config::DataBits, config::FlowControl, config::StopBits, UartDriver},
};
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522",
    not(feature = "spp")
))]
use esp_idf_sys::EspError;

#[cfg(any(target_arch = "xtensa", target_arch = "riscv32"))]
const UART_RX_TIMEOUT_MS: u64 = 500;
#[cfg(any(target_arch = "xtensa", target_arch = "riscv32"))]
const CARD_POLL_INTERVAL_MS: u64 = 3000;
// #101 RX hardening: the serve loop BLOCKS inside card exchanges
// (20–60 ms typical, WTX-class 150 ms+) while host retries keep
// arriving at line rate. ESP-IDF sizes the driver ring at
// rx_buffer_size − 128 (HW FIFO), so the old 548 gave ~420 usable
// bytes — under one 60 ms burst at 115200 8N2 (≈ 628 B). 4096 holds
// ~380 ms of continuous arrivals, far beyond any host retry pattern
// (libccid retransmits at BWT ≥ 1.4 s or the 3 s serial timeout).
#[cfg(any(target_arch = "xtensa", target_arch = "riscv32"))]
const UART_BUF_SIZE: usize = 4096;
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
    any(feature = "backend-mfrc522", feature = "backend-pn532"),
    not(feature = "spp")
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
    any(feature = "backend-mfrc522", feature = "backend-pn532"),
    not(feature = "spp")
))]
fn write_all_logged(uart: &UartDriver, bytes: &[u8]) {
    if let Err(e) = write_all(uart, bytes) {
        log::error!("UART write failed: {:?}", e);
    }
}

/// Feed one host byte through the server and, when the byte completes
/// a frame, write echo (+notify) + response and flush. Extracted so the
/// #101 drain loop can feed queued bytes through the identical path.
/// `arrival_floor` is a conservative LOWER BOUND on the byte's arrival
/// (the tick captured when the transport last knew its queue empty, or
/// before the blocking section that queued it): feed-time stamps
/// overstate arrivals queued behind a handler, which would resurrect
/// the #153 late-answer bug (Codex round 2 on #158).
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    any(feature = "backend-mfrc522", feature = "backend-pn532"),
    not(feature = "spp")
))]
fn serve_byte<D: esp32_ccid::nfc::NfcDriver>(
    uart: &UartDriver,
    server: &mut esp32_ccid::ccid_serial_server::CcidSerialServer<D>,
    byte: u8,
    arrival_floor: u32,
) {
    use esp32_ccid::ccid_serial_server::ServeAction;

    let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
    // Issue #120 turnaround stamp: measured from this byte's feed (the
    // frame-completing byte is the one that matters) through flush_write.
    let byte_us = esp32_ccid::t1::now_us();
    match server.feed_byte(byte, now) {
        ServeAction::Respond => {
            // #153 arrival-anchor correction: the frame may have queued
            // behind the PREVIOUS handler — judge it against the floor,
            // not the drain-time feed stamp.
            server.refresh_host_deadline(arrival_floor);
            // #153 answer-or-silence: the handler ran inside feed_byte;
            // if it outlived the host's read window, late bytes must
            // not reach the wire (libccidtwin has no mid-stream
            // resync). Drop echo+notify+response as a unit.
            let post = unsafe { esp_idf_sys::xTaskGetTickCount() };
            if server.response_expired(post) {
                server.record_late_response();
                server.poll_if_due_after_response(unsafe { esp_idf_sys::xTaskGetTickCount() });
                return;
            }
            write_all_logged(uart, server.echo());
            let notify = server.notify();
            if !notify.is_empty() {
                write_all_logged(uart, notify);
            }
            write_all_logged(uart, server.response());
            let _ = uart.flush_write();
            let turnaround_us = esp32_ccid::t1::now_us() - byte_us;
            server
                .handler_mut()
                .record_turnaround_us(turnaround_us.clamp(0, u32::MAX as i64) as u32);
            // Presence under continuous traffic (issue #114, Codex on
            // #98): with inline_poll=false the poll on the read-idle
            // path only fired when the host paused, so commands
            // arriving <500 ms apart starved it entirely. Post-response
            // the wire is quiet from our side — poll here too (same
            // primitive the USB-CDC main uses) so the presence cache
            // stays fresh.
            let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
            server.poll_if_due(now);
            // Dump-and-retrieve: the 0xD1 ack went out — panic now so
            // the panic handler writes the flash coredump.
            if server.take_snapshot_request() {
                panic!("escape 0xD1: diagnostic snapshot requested");
            }
            server.poll_if_due_after_response(unsafe { esp_idf_sys::xTaskGetTickCount() });
        }
        ServeAction::Nak => {
            write_all_logged(uart, server.nak_frame());
        }
        ServeAction::None => {}
    }
}

/// Shared UART serving loop (issue #90): both classic mains route through
/// [`CcidSerialServer`] with the UART-main profile (NAK on malformed
/// frames, NotifySlotChange on presence transitions) — one implementation
/// instead of three near-copies that drifted apart bug-by-bug.
/// (Not compiled under `spp` — RFCOMM replaces UART0 as the CCID wire
/// there; building pn532+spp fails loudly here by design.)
#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    any(feature = "backend-mfrc522", feature = "backend-pn532"),
    not(feature = "spp")
))]
fn serve_uart<D: esp32_ccid::nfc::NfcDriver>(
    uart: &UartDriver,
    handler: esp32_ccid::ccid_handler::CcidHandler<D>,
) -> ! {
    use esp32_ccid::ccid_serial_server::CcidSerialServer;

    let poll_interval_ticks =
        esp_idf_hal::delay::TickType::new_millis(CARD_POLL_INTERVAL_MS).ticks() as u32;
    // #153 window in TICKS — the tick rate is 100 Hz in production, so
    // 2500 ms must be converted (Codex #158 P1), never written as raw
    // ticks.
    let response_deadline_ticks =
        esp32_ccid::ccid_serial_server::response_window_ticks(esp_idf_sys::configTICK_RATE_HZ);
    let mut server = CcidSerialServer::with_config(
        handler,
        poll_interval_ticks,
        response_deadline_ticks,
        unsafe { esp_idf_sys::xTaskGetTickCount() },
        esp32_ccid::ccid_serial_server::ServeConfig {
            nak_on_error: true,
            notify_slot_change: true,
            // #89: MFRC522 presence polls on marginal coupling block for
            // hundreds of ms — never make a pending response wait on one.
            // Polls run on read-idle; transitions are delivered in the next
            // command window (ccid_serial_server ServeConfig docs).
            inline_poll: false,
        },
    );

    // Purge any stale UART data from boot/init: pcscd expects a clean
    // protocol start (SYNC byte first).
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

    // #101 RX hardening: bytes that queued while the loop was busy
    // (card exchange inside feed_byte, response flush, presence poll)
    // are host retries or the next frame — feed them BEFORE blocking
    // again. Blocking only happens BETWEEN frames (feed_byte runs the
    // card only after a frame completes), so the drained bytes always
    // begin at a frame boundary; the ring sizing above guarantees they
    // survived the block. The blocking read stays 1-byte: UART reads
    // wait for the FULL requested length, so a larger buffer would
    // add latency, not throughput.
    //
    // #153 arrival floors (Codex round 2 on #158): a byte fed from the
    // drain loop queued behind a handler — its true arrival is only
    // bounded below by the tick captured before that handler's
    // dispatch. Drained bytes are anchored there, so drain-time feed
    // stamps cannot extend the host read window. A read that returns
    // empty proves the ring idle and raises the floor.
    let mut byte_buf = [0u8; 1];
    let mut drain_buf = [0u8; 256];
    let timeout_ticks = esp_idf_hal::delay::TickType::new_millis(UART_RX_TIMEOUT_MS).ticks();
    let drain_ticks = esp_idf_hal::delay::TickType::new_millis(0).ticks();
    loop {
        let read_start = unsafe { esp_idf_sys::xTaskGetTickCount() };
        match uart.read(&mut byte_buf, timeout_ticks) {
            Ok(1) => {
                // Bytes queued behind this byte's handler arrived at or
                // after its dispatch tick.
                let pre_dispatch = unsafe { esp_idf_sys::xTaskGetTickCount() };
                serve_byte(uart, &mut server, byte_buf[0], read_start);
                let mut floor = pre_dispatch;
                loop {
                    match uart.read(&mut drain_buf, drain_ticks) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            // One read = one burst: every byte in it
                            // shares the floor captured before it.
                            let burst_floor = floor;
                            let mut last_dispatch = burst_floor;
                            for &b in &drain_buf[..n] {
                                let pre = unsafe { esp_idf_sys::xTaskGetTickCount() };
                                serve_byte(uart, &mut server, b, burst_floor);
                                last_dispatch = pre;
                            }
                            floor = last_dispatch;
                        }
                    }
                }
            }
            _ => {
                // Read idle: abandon any partial frame, background poll.
                let now = unsafe { esp_idf_sys::xTaskGetTickCount() };
                server.on_read_idle(now);
            }
        }
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
    // Same RX-TOUT fix as the MFRC522 main: default flush adds ~40 ms per
    // CCID frame (bench perf_ab 2026-10-10).
    unsafe { esp_idf_sys::uart_set_rx_timeout(esp_idf_sys::uart_port_t_UART_NUM_0, 1) };

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

    serve_uart(&uart, CcidHandler::new(pn532_driver));
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "backend-mfrc522",
    not(feature = "wifi-ccid"),
    not(feature = "wifi-vpcd")
))]
fn main() {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();
    // Note: with the default sdkconfig (CONFIG_ESP_CONSOLE_NONE=y) log output is
    // dropped — UART0 belongs to the CCID protocol. Logs surface in debug builds
    // (console enabled) and in the `ble` feature build (BLE log bridge). In
    // `spp` builds the console stays enabled: CCID rides RFCOMM, so UART0 is
    // free for logs.
    #[cfg(not(any(feature = "ble", feature = "spp")))]
    esp32_ccid::netlog::init();

    let peripherals = Peripherals::take().expect("ESP32 peripherals already taken");

    // BLE debug console (issue #66): logs ride GATT notifications; UART0 and
    // C-level console output stay reserved for CCID. Installs the global
    // `log` sink — no EspLogger::initialize_default() under `ble` (first
    // installer wins and would permanently displace the BLE logger).
    #[cfg(feature = "ble")]
    let ble_console = BleConsole::init(peripherals.modem, true);

    // WiFi + OTA for serial-free bring-up (bench power bricks): the
    // modem is exclusive with the `ble` and `spp` feature builds, and
    // credentials come from NUCULA_WIFI_SSID/PASS at build time — absent
    // creds leave the board serial/CCID-only exactly as before.
    #[cfg(not(any(feature = "ble", feature = "spp")))]
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

    #[cfg(feature = "ble")]
    if ble_console.is_some() {
        log::info!("ESP32-CCID: BLE debug console started, advertising");
    } else {
        log::warn!("ESP32-CCID: BLE debug console FAILED to start");
    }
    // UART0 is the CCID wire in this main and libccidtwin's parser is
    // strict: even WARN-level failure lines ("power_on failed") interleave
    // with GemPC frames and pcscd rejects the handshake. Diagnostics go
    // through Escape 0xD0 counters / 0xD1 coredumps / `ble` builds — the
    // wire itself stays pristine. NOT applied under `spp`: the CCID wire
    // is RFCOMM there, so console logs cannot reach it.
    #[cfg(not(any(feature = "ble", feature = "spp")))]
    log::set_max_level(log::LevelFilter::Off);

    // UART0 as the CCID wire — replaced entirely by the SPP transport in
    // `spp` builds (the modem goes to BtDriver<BtClassic> instead).
    #[cfg(not(feature = "spp"))]
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
    #[cfg(all(feature = "board-nucula", not(feature = "spp")))]
    let (uart_tx, uart_rx) = (peripherals.pins.gpio21, peripherals.pins.gpio20);
    #[cfg(all(not(feature = "board-nucula"), not(feature = "spp")))]
    let (uart_tx, uart_rx) = (peripherals.pins.gpio1, peripherals.pins.gpio3);

    #[cfg(not(feature = "spp"))]
    let uart = UartDriver::new(
        peripherals.uart0,
        uart_tx,
        uart_rx,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &uart_config,
    )
    .expect("UART0 init failed (CCID serial TX/RX)");
    // Bench (perf_ab, 2026-10-10): the default hardware RX-TOUT left short
    // CCID frames in the UART FIFO until the periodic flush — a fixed ~40 ms
    // first-byte (echo) delay dominated every APDU round-trip. A 1-symbol
    // TOUT flushes as soon as the line goes idle (~90 us at 115200 8N2).
    #[cfg(not(feature = "spp"))]
    unsafe {
        esp_idf_sys::uart_set_rx_timeout(esp_idf_sys::uart_port_t_UART_NUM_0, 1)
    };
    #[cfg(not(feature = "spp"))]
    log::warn!("bring-up: UART0 ok");
    #[cfg(not(feature = "spp"))]
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
        // Routed through the recovery wrapper (Codex reviews on #43/#40/#38,
        // P1): plain init() never reaches the failure tracker, so repeated
        // startup failures could not trigger the self-healing full re-init.
        if mfrc522_driver.init_with_recovery().is_ok() {
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

    // Transport split: `spp` builds serve the same CcidHandler over the
    // BT-Classic SPP byte pipe (spp_server::shell::serve_spp — same
    // echo/response/poll cadence as serve_uart, REFERENCE
    // esp-idf-svc bt_spp_acceptor.rs); everything else stays on UART0.
    #[cfg(feature = "spp")]
    esp32_ccid::spp_server::shell::serve_spp(peripherals.modem, CcidHandler::new(mfrc522_driver));
    #[cfg(not(feature = "spp"))]
    serve_uart(&uart, CcidHandler::new(mfrc522_driver));
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
    not(feature = "pn7160-bringup"),
    not(feature = "pn7160-bitbang"),
    not(feature = "pn7160-m1"),
    not(feature = "pn7160-v10raw"),
    not(feature = "pn7160-actdiag")
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
        log::warn!(
            "FWID esp32-ccid rev={} build={}",
            env!("FW_GIT_REV"),
            env!("FW_BUILD_TS")
        );
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
    not(feature = "pn7160-ccid"),
    not(feature = "wifi-ccid"),
    not(feature = "wifi-vpcd"),
    not(feature = "pn7160-bitbang"),
    not(feature = "pn7160-m1"),
    not(feature = "pn7160-v10raw"),
    not(feature = "pn7160-actdiag")
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
    feature = "pn7160-v10raw"
))]
fn main() {
    esp32_ccid::pn7160_v10raw::run()
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "pn7160-actdiag"
))]
fn main() {
    esp32_ccid::pn7160_actdiag::run()
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "wifi-vpcd"
))]
fn main() {
    esp32_ccid::wifi_vpcd::run()
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "wifi-ccid"
))]
fn main() {
    esp32_ccid::wifi_ccid::run()
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "pn7160-ccid"
))]
fn main() {
    esp32_ccid::pn7160_ccid::run()
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "pn7160-bitbang",
    not(any(feature = "pn7160-bringup", feature = "pn7160-ccid"))
))]
fn main() {
    esp32_ccid::pn7160_bitbang::run()
}

#[cfg(all(
    any(target_arch = "xtensa", target_arch = "riscv32"),
    feature = "pn7160-m1",
    not(any(
        feature = "pn7160-bringup",
        feature = "pn7160-ccid",
        feature = "pn7160-bitbang"
    ))
))]
fn main() {
    esp32_ccid::pn7160_m1::run()
}

// CI tier verification (2026-10-14): a firmware/ touch must run the
// PRIMARY esp32 legs on PRs; secondary legs stay on main/nightly/label.
