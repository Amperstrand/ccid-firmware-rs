// Only compile for ARM embedded targets (not for x86_64 tests)
//
//! Run: `cargo run --release --example display_touch`

#![cfg(all(target_arch = "arm", target_os = "none"))]
#![deny(warnings)]
#![no_main]
#![no_std]

use cortex_m::peripheral::Peripherals;
use cortex_m_rt::entry;

use defmt_rtt as _;
use panic_probe as _;

use stm32f469i_disc as board;

use board::hal::ltdc::{Layer, PixelFormat};
use board::hal::{pac, prelude::*, rcc};
use board::lcd;
use board::sdram::{alt, sdram_pins, Sdram};
use board::touch;
