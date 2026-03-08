//! STM32F469 CCID Smartcard Reader Firmware
//!
//! This firmware implements a USB CCID class smartcard reader using:
//! - USART2 in smartcard mode for ISO 7816-3 communication
//! - USB OTG FS for CCID protocol communication with host
//!
//! Pin assignments:
//! - PA2: Smartcard IO (USART2_TX, AF7, open-drain)
//! - PA4: Smartcard CLK (USART2_CK, AF7, push-pull)
//! - PG10: Smartcard RST (GPIO output, active LOW)
//! - PC2: Smartcard PRES (GPIO input, HIGH = card present)
//! - PC5: Smartcard PWR (GPIO output, LOW = power ON)
//! - PA11: USB DM
//! - PA12: USB DP

#![no_std]
#![no_main]

use defmt_rtt as _;
use panic_probe as _;

mod smartcard;
mod t1_engine;
mod usb_identity;

use cortex_m_rt::entry;
use stm32f4xx_hal::gpio::{
    gpioa::{PA2, PA4},
    gpioc::{PC2, PC5},
    gpiog::PG10,
    Alternate, Input, OpenDrain, Output, PushPull,
};
use stm32f4xx_hal::otg_fs::{UsbBus, USB};
use stm32f4xx_hal::pac;
use stm32f4xx_hal::prelude::*;
use stm32f4xx_hal::rcc::Config;
use usb_device::endpoint::In;
use usb_device::prelude::*;

mod ccid;

use ccid::{CcidClass, SmartcardDriver as CcidSmartcardDriver};
use smartcard::{SmartcardError, SmartcardUart};
use usb_identity::{
    USB_MANUFACTURER, USB_PRODUCT, USB_PRODUCT_ID, USB_SERIAL_NUMBER, USB_VENDOR_ID,
};

/// USB endpoint memory buffer (required by USB OTG driver)
static mut USB_EP_MEMORY: [u32; 1024] = [0; 1024];

/// Wrapper to adapt SmartcardUart to the ccid::SmartcardDriver trait
///
/// The smartcard module defines its own SmartcardDriver trait that returns &Atr,
/// while ccid::SmartcardDriver expects &[u8]. This wrapper bridges the gap.
struct SmartcardWrapper {
    uart: SmartcardUart,
}

impl SmartcardWrapper {
    fn new(uart: SmartcardUart) -> Self {
        Self { uart }
    }
}

impl CcidSmartcardDriver for SmartcardWrapper {
    type Error = SmartcardError;

    fn power_on(&mut self) -> core::result::Result<&[u8], Self::Error> {
        // Power on the smartcard and get the ATR
        let atr = self.uart.power_on()?;
        // Return the raw ATR bytes
        Ok(&atr.raw[..atr.len])
    }

    fn power_off(&mut self) {
        self.uart.power_off()
    }

    fn is_card_present(&self) -> bool {
        self.uart.is_card_present()
    }

    fn transmit_apdu(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        self.uart.transmit_apdu(command, response)
    }

    fn transmit_raw(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        self.uart.transmit_raw(data, response)
    }

    fn set_protocol(&mut self, protocol: u8) {
        self.uart.set_protocol(protocol)
    }

    fn set_clock(&mut self, enable: bool) {
        self.uart.set_clock(enable)
    }

    fn set_clock_and_rate(
        &mut self,
        clock_hz: u32,
        rate_bps: u32,
    ) -> core::result::Result<(u32, u32), Self::Error> {
        self.uart.set_clock_and_rate(clock_hz, rate_bps)
    }
}

#[entry]
fn main() -> ! {
    defmt::info!("CCID Reader starting...");

    // =========================================================================
    // Step 1: Take peripherals
    // =========================================================================
    let dp = pac::Peripherals::take().unwrap();

    // =========================================================================
    // Step 2: Configure clocks (HSI -> 168MHz sysclk, 48MHz USB)
    // =========================================================================
    defmt::info!("Configuring clocks...");
    let mut rcc = dp.RCC.freeze(
        Config::hsi()
            .sysclk(168.MHz())
            .pclk1(42.MHz())
            .pclk2(84.MHz())
            .require_pll48clk(),
    );
    defmt::info!(
        "Clocks OK: sys={}MHz",
        rcc.clocks.sysclk().raw() / 1_000_000
    );

    // Step 3: Configure all GPIO from same ports
    let mut gpioa = dp.GPIOA.split(&mut rcc);
    let mut gpioc = dp.GPIOC.split(&mut rcc);
    let mut gpiog = dp.GPIOG.split(&mut rcc);

    // Smartcard pins (PA2, PA4)
    let io_pin: PA2<Alternate<7, OpenDrain>> = gpioa
        .pa2
        .into_alternate_open_drain::<7>()
        .internal_pull_up(true)
        .speed(stm32f4xx_hal::gpio::Speed::High);

    let clk_pin: PA4<Alternate<7, PushPull>> = gpioa
        .pa4
        .into_alternate::<7>()
        .speed(stm32f4xx_hal::gpio::Speed::High);

    let rst_pin: PG10<Output<PushPull>> = gpiog
        .pg10
        .into_push_pull_output_in_state(stm32f4xx_hal::gpio::PinState::High);

    let pres_pin: PC2<Input> = gpioc.pc2.into_input();
    let pwr_pin: PC5<Output<PushPull>> = gpioc
        .pc5
        .into_push_pull_output_in_state(stm32f4xx_hal::gpio::PinState::High);

    defmt::info!("Smartcard GPIO OK");

    // USB pins (PA11, PA12) - same gpioa port
    let usb_dm = gpioa.pa11.into_alternate::<10>();
    let usb_dp = gpioa.pa12.into_alternate::<10>();

    // Step 4: Smartcard UART
    let smartcard_uart = SmartcardUart::new(
        dp.USART2,
        io_pin,
        clk_pin,
        rst_pin,
        pres_pin,
        pwr_pin,
        &rcc.clocks,
    );
    defmt::info!("Smartcard UART OK");

    // Step 5: USB OTG FS
    let usb_otg = USB::new(
        (dp.OTG_FS_GLOBAL, dp.OTG_FS_DEVICE, dp.OTG_FS_PWRCLK),
        (usb_dm, usb_dp),
        &rcc.clocks,
    );
    let usb_bus = UsbBus::new(usb_otg, unsafe { &mut USB_EP_MEMORY });
    defmt::info!("USB bus OK");

    // Step 6: Allocate interrupt endpoint for NotifySlotChange, then CCID class
    let ep_int = usb_bus.interrupt::<In>(8, 10);
    let smartcard_wrapper = SmartcardWrapper::new(smartcard_uart);
    let mut ccid_class = CcidClass::new(&usb_bus, smartcard_wrapper, ep_int);
    defmt::info!("CCID class OK");

    let mut usb_device = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(USB_VENDOR_ID, USB_PRODUCT_ID))
        .strings(&[StringDescriptors::default()
            .manufacturer(USB_MANUFACTURER)
            .product(USB_PRODUCT)
            .serial_number(USB_SERIAL_NUMBER)])
        .unwrap()
        .device_class(0x00)
        .build();

    defmt::info!("USB device OK - entering main loop");

    // Step 8: Main loop
    loop {
        usb_device.poll(&mut [&mut ccid_class]);
    }
}
