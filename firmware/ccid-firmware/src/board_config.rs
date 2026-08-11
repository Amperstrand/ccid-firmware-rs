//! Board-specific hardware configuration for smartcard reader.
//!
//! Each MCU target has a `setup()` function that takes the raw PAC GPIO
//! ports + hardware peripherals, configures pins, constructs the smartcard
//! driver, and extracts USB pins — all in one place.
//!
//! Adding a new board: add a new `#[cfg]` module here, add one line to
//! main.rs. Zero changes to driver code.
//!
//! See: https://github.com/Amperstrand/ccid-firmware-rs/issues/18

#[cfg(all(feature = "stm32f469", target_arch = "arm", target_os = "none"))]
pub mod f469 {
    use crate::smartcard_common::SMARTCARD_CONFIG_DEFAULT;
    use crate::SmartcardUart;
    use crate::SmartcardWrapper;
    use stm32f4xx_hal::gpio::*;
    use stm32f4xx_hal::pac;
    use stm32f4xx_hal::rcc::Rcc;

    pub type UsbDmPin = PA11<Alternate<10>>;
    pub type UsbDpPin = PA12<Alternate<10>>;

    pub struct Hardware {
        pub wrapper: SmartcardWrapper,
        pub usb_dm: UsbDmPin,
        pub usb_dp: UsbDpPin,
    }

    pub fn setup(
        gpioa: pac::GPIOA,
        gpioc: pac::GPIOC,
        gpiog: pac::GPIOG,
        usart2: pac::USART2,
        rcc: &mut Rcc,
    ) -> Hardware {
        let mut gpioa = gpioa.split(rcc);
        let mut gpioc = gpioc.split(rcc);
        let mut gpiog = gpiog.split(rcc);

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

        defmt::info!("Smartcard GPIO OK (F469 USART)");

        let usb_dm: UsbDmPin = gpioa.pa11.into_alternate::<10>();
        let usb_dp: UsbDpPin = gpioa.pa12.into_alternate::<10>();

        let uart = SmartcardUart::new(
            usart2,
            io_pin,
            clk_pin,
            rst_pin,
            pres_pin,
            pwr_pin,
            &rcc.clocks,
            SMARTCARD_CONFIG_DEFAULT,
        );
        defmt::info!("Smartcard UART OK");

        Hardware {
            wrapper: SmartcardWrapper::new(uart),
            usb_dm,
            usb_dp,
        }
    }
}

#[cfg(all(feature = "stm32f746", target_arch = "arm", target_os = "none"))]
pub mod f746 {
    use crate::SmartcardBitbang;
    use crate::SmartcardWrapper;
    use stm32f7xx_hal::gpio::*;
    use stm32f7xx_hal::pac;
    use stm32f7xx_hal::rcc::Clocks;

    pub type UsbDmPin = PA11<Alternate<10>>;
    pub type UsbDpPin = PA12<Alternate<10>>;

    pub struct Hardware {
        pub wrapper: SmartcardWrapper,
        pub usb_dm: UsbDmPin,
        pub usb_dp: UsbDpPin,
    }

    pub fn setup(
        gpioa: pac::GPIOA,
        gpiof: pac::GPIOF,
        gpioi: pac::GPIOI,
        gpiok: pac::GPIOK,
        clocks: &Clocks,
    ) -> Hardware {
        let mut gpioa = gpioa.split();
        let mut gpiof = gpiof.split();
        let mut gpioi = gpioi.split();
        let mut gpiok = gpiok.split();

        let _backlight: PK3<Output<PushPull>> = gpiok
            .pk3
            .into_push_pull_output_in_state(stm32f7xx_hal::gpio::PinState::Low);

        let io_pin: PI0<Output<OpenDrain>> = gpioi
            .pi0
            .into_open_drain_output()
            .internal_pull_up(true)
            .set_speed(stm32f7xx_hal::gpio::Speed::High);

        let clk_pin: PF6<Output<PushPull>> = gpiof
            .pf6
            .into_push_pull_output_in_state(stm32f7xx_hal::gpio::PinState::Low)
            .set_speed(stm32f7xx_hal::gpio::Speed::VeryHigh);

        let rst_pin: PI2<Output<PushPull>> = gpioi
            .pi2
            .into_push_pull_output_in_state(stm32f7xx_hal::gpio::PinState::High);

        let pres_pin = gpiof.pf10.into_floating_input();

        let pwr_pin: PF7<Output<PushPull>> = gpiof
            .pf7
            .into_push_pull_output_in_state(stm32f7xx_hal::gpio::PinState::High);

        defmt::info!("Smartcard GPIO OK (F746 bitbang)");

        let usb_dm: UsbDmPin = gpioa.pa11.into_alternate::<10>();
        let usb_dp: UsbDpPin = gpioa.pa12.into_alternate::<10>();

        let sysclk_hz = clocks.sysclk().raw();
        let bitbang = SmartcardBitbang::new(io_pin, clk_pin, rst_pin, pres_pin, pwr_pin, sysclk_hz);
        defmt::info!("Smartcard bitbang OK");

        Hardware {
            wrapper: SmartcardWrapper::new(bitbang),
            usb_dm,
            usb_dp,
        }
    }
}
