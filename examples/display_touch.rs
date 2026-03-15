// Only compile for ARM embedded targets (not for x86_64 tests)
//
//! Run: `cargo run --release --example display_touch`

#![cfg_attr(all(target_arch = "arm", target_os = "none"), no_main)]
#![cfg_attr(all(target_arch = "arm", target_os = "none"), no_std)]
#![cfg_attr(all(target_arch = "arm", target_os = "none"), deny(warnings))]

#[cfg(all(target_arch = "arm", target_os = "none"))]
use cortex_m::peripheral::Peripherals;
#[cfg(all(target_arch = "arm", target_os = "none"))]
use cortex_m_rt::entry;

#[cfg(all(target_arch = "arm", target_os = "none"))]
use defmt_rtt as _;
#[cfg(all(target_arch = "arm", target_os = "none"))]
use panic_probe as _;

#[cfg(all(target_arch = "arm", target_os = "none"))]
use stm32f469i_disc as board;

#[cfg(all(target_arch = "arm", target_os = "none"))]
use board::hal::ltdc::{Layer, PixelFormat};
#[cfg(all(target_arch = "arm", target_os = "none"))]
use board::hal::{pac, prelude::*, rcc};
#[cfg(all(target_arch = "arm", target_os = "none"))]
use board::lcd;
#[cfg(all(target_arch = "arm", target_os = "none"))]
use board::sdram::{alt, sdram_pins, Sdram};
#[cfg(all(target_arch = "arm", target_os = "none"))]
use board::touch;

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
fn main() {}
