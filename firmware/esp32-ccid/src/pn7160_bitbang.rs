//! Bit-bang I2C probe — the wire-level ground-truth instrument for the
//! #63/#80 mystery. Drives START + 0x28-write + STOP with raw GPIO
//! open-drain bit-banging (no I2C peripheral, no driver), samples the
//! ACK bit, and also reports idle line levels. Run BEFORE any driver
//! claims the bus. Outcomes:
//!   idle SDA/SCL both high + bit-bang ACK  → chip reachable; BOTH hw
//!     drivers misdrive the bus in this binary → driver/peripheral layer
//!   idle lines low                         → bus held (parasite)
//!   idle ok + bit-bang NAK                 → electrical environment of
//!     THIS binary is faulty (compare against wallet fw which ACKs)

use esp_idf_hal::peripherals::Peripherals;

const SDA: i32 = 4;
const SCL: i32 = 5;

const HALF_US: u32 = 5; // 100 kHz

fn gpio_output_od(pin: i32) {
    unsafe {
        let mut cfg: esp_idf_sys::gpio_config_t = core::mem::zeroed();
        cfg.pin_bit_mask = 1u64 << pin;
        cfg.mode = esp_idf_sys::gpio_mode_t_GPIO_MODE_OUTPUT_OD;
        esp_idf_sys::gpio_config(&cfg);
    }
}

fn sda(v: i32) {
    unsafe { esp_idf_sys::gpio_set_level(SDA, v) };
}
fn scl(v: i32) {
    unsafe { esp_idf_sys::gpio_set_level(SCL, v) };
}
fn read_sda() -> i32 {
    unsafe { esp_idf_sys::gpio_get_level(SDA) }
}
fn read_scl() -> i32 {
    unsafe { esp_idf_sys::gpio_get_level(SCL) }
}
fn half() {
    unsafe { esp_idf_sys::esp_rom_delay_us(HALF_US) };
}

fn bitbang_probe(addr7: u8) -> bool {
    // idle
    sda(1);
    scl(1);
    half();
    half();
    // START: SDA falls while SCL high
    sda(0);
    half();
    scl(0);
    half();
    // 8 address bits, MSB first (write = R/W 0)
    let byte = (addr7 as u8) << 1;
    for i in (0..8).rev() {
        sda(((byte >> i) & 1) as i32);
        half();
        scl(1);
        half();
        scl(0);
        half();
    }
    // 9th clock: release SDA, sample ACK
    sda(1);
    half();
    scl(1);
    half();
    let ack = read_sda() == 0;
    scl(0);
    half();
    // STOP: SDA rises while SCL high
    sda(0);
    half();
    scl(1);
    half();
    sda(1);
    half();
    ack
}

pub fn run() -> ! {
    esp_idf_sys::link_patches();
    log::warn!("bitbang: wire-level probe — no I2C peripheral involved");

    let _peripherals = Peripherals::take().expect("peripherals");

    gpio_output_od(SDA);
    gpio_output_od(SCL);
    sda(1);
    scl(1);
    unsafe { esp_idf_sys::esp_rom_delay_us(10_000) };

    let sda_idle = read_sda();
    let scl_idle = read_scl();
    log::warn!(
        "bitbang: idle levels SDA={} SCL={} (1/1 expected; 0 = bus held)",
        sda_idle,
        scl_idle
    );

    for addr in [0x20u8, 0x28, 0x7C] {
        let ack = bitbang_probe(addr);
        log::warn!("bitbang: probe 0x{:02X} -> {}", addr, if ack { "ACK" } else { "NAK" });
        unsafe { esp_idf_sys::esp_rom_delay_us(50_000) };
    }
    log::warn!("bitbang: done — parking");

    loop {
        unsafe { esp_idf_sys::esp_rom_delay_us(1_000_000) };
    }
}
