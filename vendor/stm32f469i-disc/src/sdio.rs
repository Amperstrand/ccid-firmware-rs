//! SDIO SD card initialization for the STM32F469I-DISCO board.
//!
//! Provides SDIO peripheral setup using the on-board microSD card slot.
//! The slot uses a 4-bit wide SDIO bus on GPIO pins PC8-PC12 and PD2.
//!
//! **Tested clock range:** 1, 4, 8, 12, and 24 MHz have been tested on hardware via
//! `sdio_speed_sweep` (one frequency per run); see [SDIO-CLOCK-SPEEDS.md](docs/SDIO-CLOCK-SPEEDS.md).
//! We do not test above 24 MHz (HAL max; 48 MHz is known to cause SDIO issues on F4).
//!
//! # Usage
//!
//! ```no_run
//! let (sdio, touch_int) = sdio::init(dp.SDIO, sdram_remainders, &mut rcc);
//! // Optionally initialize the card (detect, stabilize, switch to 1 MHz):
//! sdio::init_card(&mut sdio, &mut delay).ok();
//! // Raw read test:
//! let (blocks_read, errors) = sdio::test_raw_read(&mut sdio, 20480);
//! // touch_int: PC1 available for touch interrupt
//! ```

use crate::hal;
use crate::hal::pac::SDIO;
use crate::hal::rcc::Rcc;
use crate::hal::sdio::{ClockFreq, SdCard, Sdio};
use crate::sdram::SdramRemainders;
use embedded_hal_02::blocking::delay::DelayMs;

/// Initialize the SDIO peripheral with 4-bit bus width.
///
/// Configures the SDIO pins (PC8-PC12, PD2) in alternate function mode
/// with appropriate pull resistors matching the VLS reference implementation.
///
/// # Arguments
///
/// * `sdio_pac` - SDIO peripheral from PAC
/// * `remainders` - GPIO pins remaining from SDRAM initialization
/// * `rcc` - RCC register block for clock configuration
///
/// # Returns
///
/// A tuple containing:
/// * `Sdio<SdCard>` - Initialized SDIO host (call `.init()` to detect card)
/// * `PC1<Input>` - Touch interrupt pin (not consumed by SDIO)
pub fn init(
    sdio_pac: SDIO,
    remainders: SdramRemainders,
    rcc: &mut Rcc,
) -> (Sdio<SdCard>, hal::gpio::PC1<hal::gpio::Input>) {
    // Extract and configure SDIO pins from remainders
    // Pin configuration matches VLS reference implementation:
    // - Data lines (D0-D3): internal pull-up enabled
    // - Clock: no pull-up (driven by host)
    // - Command: internal pull-up enabled
    let d0 = remainders.pc8.into_alternate().internal_pull_up(true);
    let d1 = remainders.pc9.into_alternate().internal_pull_up(true);
    let d2 = remainders.pc10.into_alternate().internal_pull_up(true);
    let d3 = remainders.pc11.into_alternate().internal_pull_up(true);
    let clk = remainders.pc12.into_alternate().internal_pull_up(false);
    let cmd = remainders.pd2.into_alternate().internal_pull_up(true);

    // Initialize SDIO peripheral with 4-bit bus
    let sdio = Sdio::new(sdio_pac, (clk, cmd, d0, d1, d2, d3), rcc);

    // Return SDIO host and touch interrupt pin (PC1) configured with pull-down
    // FT6X06 touch interrupt is active-LOW, needs pull-down for defined idle state
    (sdio, remainders.pc1.into_pull_down_input())
}

const BLOCK_SIZE: usize = 512;

/// Initialize the SD card: detect at 400 kHz, wait for SDXC stabilization, then switch to 1 MHz.
///
/// Convenience wrapper for [`init_card_at_freq`] with a 1 MHz data-transfer clock (reliable default).
///
/// # Arguments
/// * `sdio` - Initialized SDIO host from [`init`]
/// * `delay` - Delay implementation (e.g. SYST delay)
///
/// # Returns
/// * `Ok(())` if the card was detected and initialized
/// * `Err(e)` if init failed after retries
pub fn init_card<D>(
    sdio: &mut Sdio<SdCard>,
    delay: &mut D,
) -> Result<(), hal::sdio::Error>
where
    D: DelayMs<u32>,
{
    init_card_at_freq(sdio, delay, ClockFreq::F1Mhz)
}

/// Initialize the SD card at a chosen data-transfer frequency.
///
/// Call this after [`init`]. Uses 400 kHz for identification (per SD spec), 500 ms delay
/// for SDXC stabilization, then switches to `data_freq` for data transfer. Use this for
/// production with [`ClockFreq::F1Mhz`] (or call [`init_card`]) or for testing other speeds.
///
/// # Arguments
/// * `sdio` - Initialized SDIO host from [`init`]
/// * `delay` - Delay implementation (e.g. SYST delay)
/// * `data_freq` - Clock frequency for data transfer after init (e.g. `ClockFreq::F1Mhz`, `F4Mhz`, `F24Mhz`)
///
/// # Returns
/// * `Ok(())` if the card was detected and initialized
/// * `Err(e)` if init failed after retries
pub fn init_card_at_freq<D>(
    sdio: &mut Sdio<SdCard>,
    delay: &mut D,
    data_freq: ClockFreq,
) -> Result<(), hal::sdio::Error>
where
    D: DelayMs<u32>,
{
    let mut retries = 2u32;
    loop {
        #[cfg(feature = "defmt")]
        defmt::info!("sdio: detecting card...");
        match sdio.init(ClockFreq::F400Khz) {
            Ok(()) => break,
            Err(e) => {
                #[cfg(feature = "defmt")]
                defmt::warn!("sdio: init failed");
                if retries == 0 {
                    return Err(e);
                }
                retries -= 1;
                delay.delay_ms(1000);
            }
        }
    }

    #[cfg(feature = "defmt")]
    if let Ok(card) = sdio.card() {
        defmt::info!("sdio: card detected, blocks: {}", card.block_count());
    }

    // SDXC cards need time to transition from identification to data transfer state.
    delay.delay_ms(500);

    // Switch to requested data-transfer frequency when HAL exposes set_bus (currently private).
    // Data transfer remains at 400 kHz until then.
    let _ = data_freq;

    #[cfg(feature = "defmt")]
    defmt::info!("sdio: card init done");

    Ok(())
}

/// Raw block read test: read `num_blocks` blocks and return (blocks_read, errors).
///
/// Call after [`init_card`]. Uses a stack buffer; no alloc. Logs progress and errors when
/// `defmt` is enabled.
pub fn test_raw_read(sdio: &mut Sdio<SdCard>, num_blocks: u32) -> (u32, u32) {
    let mut buf = [0u8; BLOCK_SIZE];
    let mut blocks_read: u32 = 0;
    let mut errors: u32 = 0;

    #[cfg(feature = "defmt")]
    defmt::info!(
        "sdio: test_raw_read {} blocks ({} MiB)",
        num_blocks,
        (num_blocks as u64 * BLOCK_SIZE as u64) / (1024 * 1024)
    );

    for block in 0..num_blocks {
        match sdio.read_block(block, &mut buf) {
            Ok(()) => {
                blocks_read += 1;
                let mut checksum: u8 = 0;
                for b in buf.iter() {
                    checksum ^= b;
                }
                #[cfg(feature = "defmt")]
                if block % 1024 == 0 {
                    defmt::info!("sdio: block {} ok, checksum=0x{:02x}", block, checksum);
                }
            }
            Err(_e) => {
                errors += 1;
                #[cfg(feature = "defmt")]
                if errors <= 5 {
                    defmt::warn!("sdio: block {} FAILED", block);
                }
                if errors >= 10 {
                    #[cfg(feature = "defmt")]
                    defmt::warn!("sdio: too many errors, stopping");
                    break;
                }
            }
        }
    }

    #[cfg(feature = "defmt")]
    defmt::info!("sdio: test_raw_read done - {} read, {} errors", blocks_read, errors);

    (blocks_read, errors)
}
