//! STM32F469 CCID firmware migration skeleton.
//!
//! This entrypoint completes the initial BSP swap from the legacy blocking
//! `stm32f469i-disc` board crate to the async `embassy-stm32f469i-disco` BSP.
//! It brings up the board at 180 MHz, initializes SDRAM, display, and touch,
//! and resets the USB PHY so CCID/USART integration can be re-added on top.

#![cfg_attr(all(target_arch = "arm", target_os = "none"), no_std)]
#![cfg_attr(all(target_arch = "arm", target_os = "none"), no_main)]

#[cfg(all(target_arch = "arm", target_os = "none"))]
use defmt_rtt as _;
#[cfg(all(target_arch = "arm", target_os = "none"))]
use panic_probe as _;

#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
use embassy_stm32::i2c;
#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
use embassy_time::{Duration, Timer};
#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
use embassy_stm32f469i_disco::{
    config_180,
    touch::EdgeFilter,
    BoardHint,
    DisplayCtrl,
    SdramCtrl,
    TouchCtrl,
    SYSCLK_HZ_180,
};

#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
    defmt::info!("ccid-firmware: embassy migration skeleton boot");

    let mut p = embassy_stm32::init(config_180());
    defmt::info!("clock preset applied: 180MHz");

    let sdram = SdramCtrl::new(&mut p, SYSCLK_HZ_180);
    let sdram_base = sdram.base_address();
    let framebuffer = sdram.into_bytes();
    defmt::info!("sdram ready @ {=usize:#x}", sdram_base);

    let mut display = DisplayCtrl::new(
        framebuffer,
        p.LTDC,
        p.DSIHOST,
        p.PJ2,
        p.PH7,
        BoardHint::ForceNt35510,
    );
    let _ = display.fb();
    defmt::info!("display ready");

    let touch_i2c = i2c::I2c::new_blocking(p.I2C1, p.PB8, p.PB9, i2c::Config::default());
    let mut touch = TouchCtrl::new(touch_i2c).with_filter(EdgeFilter::default_ft6x06());
    let vendor_id = touch.read_vendor_id().ok();
    let _ = touch.get_touch();
    defmt::info!("touch ready, vendor_id={=?}", vendor_id);

    embassy_stm32f469i_disco::reset_usb_phy();
    defmt::info!("usb phy reset completed");

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}

#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
compile_error!("stm32f746 firmware path still uses the legacy blocking HAL and needs a separate migration");

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
fn main() {
    println!("host stub");
}
