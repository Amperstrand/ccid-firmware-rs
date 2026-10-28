//! VPCD firmware main — the vsmartcard-aligned wifi path (REVERSED
//! connection: the board connects OUT to the host's vpcd listener).
//!
//! Feature: `wifi-vpcd` (the primary wifi path; wifi-ccid is legacy).
//! Compose with a backend+board pair, e.g.:
//!
//!   nucula:  `--features wifi-vpcd,backend-pn7160,board-nucula`
//!            (ESP32-C3, PN7160 @ I2C 0x28, build via `./build.sh wifi-vpcd`)
//!   m5stick: `--features wifi-vpcd,backend-mfrc522,board-m5stick`
//!            (ESP32, MFRC522 on the Grove I2C port, build via
//!            `./build.sh m5stick-vpcd` — computer-USB powered)

pub fn run() -> ! {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();
    crate::install_platform_clock();

    // #294 WS2: serve-proven OTA validity — arm the gate; the kernel
    // marks this image valid on the first SENT vpcd answer (an empty
    // no-card ATR counts: reader-alive is the proof, not card state).
    crate::ota_validity::arm();

    crate::netlog::init();

    log::warn!(
        "FWID wifi-vpcd rev={} build={} transport=vpcd-reversed",
        env!("FW_GIT_REV"),
        env!("FW_BUILD_TS")
    );

    let peripherals =
        esp_idf_hal::peripherals::Peripherals::take().expect("peripherals already taken");
    let nvs = esp_idf_svc::nvs::EspDefaultNvsPartition::take().expect("nvs partition");
    let ip = crate::wifi_common::station_bring_up(peripherals.modem, nvs);

    // Connect to the host's vpcd listener. The gateway IP is derived from
    // our own address (same /24 subnet — bench AP topology). Override with
    // the NUCULA_VPCD_HOST build-time env if needed.
    let gw = ip
        .rsplit_once('.')
        .map(|(prefix, _)| format!("{}.1", prefix))
        .unwrap_or_else(|| "192.168.13.1".to_string());
    let host = option_env!("NUCULA_VPCD_HOST").unwrap_or(&gw);
    // Per-board slot: the stock vpcd driver listens on :35963 (slot 00 00)
    // and :35964 (slot 00 01). Boards default to the first; a second board
    // on the bench builds with NUCULA_VPCD_PORT=35964 for a deterministic
    // board↔reader mapping instead of an accept race.
    let port: u16 = option_env!("NUCULA_VPCD_PORT")
        .and_then(|p| p.parse().ok())
        .unwrap_or(crate::vpcd::HOST_VPCD_PORT);
    crate::wifi_common::log_reset_reason();
    log::warn!(
        "wifi-vpcd: station up at {} — connecting to vpcd at {}:{}",
        ip,
        host,
        port
    );

    // NFC backend per feature composition — construction flows through
    // the board tables (t_a0ebcf54, boards-as-data): the typed
    // gpio4/5/6/7 (pn7160/nucula) and the hand-typed Grove pin numbers
    // (mfrc522) that used to live here are gone; board_ctor resolves
    // every pin/bus fact from the table row the feature selected.
    #[cfg(feature = "backend-pn7160")]
    let (driver, _nfc_ok) = crate::board_ctor::pn7160::bring_up(
        crate::board_ctor::pn7160::nucula_bus_pins(peripherals.i2c0, peripherals.pins),
    );
    #[cfg(all(feature = "backend-mfrc522", feature = "board-m5stick"))]
    let grove = crate::board_ctor::NfcBusBinding::from_board(&boards::M5STICK);
    #[cfg(all(feature = "backend-mfrc522", feature = "board-m5atom"))]
    let grove = crate::board_ctor::NfcBusBinding::from_board(&boards::M5ATOM);
    #[cfg(feature = "backend-mfrc522")]
    let driver = crate::wifi_common::mfrc522_bring_up(grove.scl, grove.sda);

    crate::vpcd::spawn(driver, host, port);
    loop {
        esp_idf_hal::delay::FreeRtos::delay_ms(10_000);
    }
}
