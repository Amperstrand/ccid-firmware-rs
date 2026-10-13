//! VPCD firmware main for the nucula — the vsmartcard-aligned wifi path
//! (REVERSED connection: the nucula connects OUT to the host's vpcd listener).
//!
//! Feature: `wifi-vpcd` (the primary wifi path; wifi-ccid is legacy).

pub fn run() -> ! {
    esp_idf_sys::link_patches();
    esp_idf_hal::sys::link_patches();

    crate::netlog::init();

    log::warn!(
        "FWID wifi-vpcd rev={} build={} transport=vpcd-reversed",
        env!("FW_GIT_REV"),
        env!("FW_BUILD_TS")
    );

    let (ip, driver) = crate::wifi_common::bring_up();

    // Connect to the host's vpcd listener. The gateway IP is derived from
    // our own address (same /24 subnet — bench AP topology). Override with
    // the NUCULA_VPCD_HOST build-time env if needed.
    let gw = ip
        .rsplit_once('.')
        .map(|(prefix, _)| format!("{}.1", prefix))
        .unwrap_or_else(|| "192.168.13.1".to_string());
    let host = option_env!("NUCULA_VPCD_HOST").unwrap_or(&gw);
    log::warn!("wifi-vpcd: station up at {} — connecting to vpcd at {}:{}", ip, host, crate::vpcd::HOST_VPCD_PORT);

    crate::vpcd::spawn(driver, host);
    loop {
        esp_idf_hal::delay::FreeRtos::delay_ms(10_000);
    }
}
