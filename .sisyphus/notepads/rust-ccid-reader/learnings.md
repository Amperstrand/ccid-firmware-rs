
## 2026-03-06: smartcard.rs Implementation

### stm32f4xx-hal API Patterns
- Register access uses method calls: usart.cr1().modify(...) not usart.cr1.modify(...)
- Parity enable bit is pce() not pe() in CR1
- Stop bits for smartcard: .stop().bits(0b01) gives 0.5 stop bits (becomes 1.5 in smartcard mode)
- Clock enable: .clken().set_bit() in CR2
- Smartcard mode: .scen().set_bit() in CR3
- Guard time/prescaler: .gtpr().write(|w| unsafe { w.gt().bits(16).psc().bits(prescaler) })

### GPIO Pin Types
- Alternate takes const generic: Alternate<7, OpenDrain> for AF7 open-drain
- Input pins: PC2<Input> (no generic param)
- Output pins: PG10<Output<PushPull>>

### Clock Access
- clocks.pclk1().raw() returns raw frequency in Hz
- APB1 is typically 42MHz on STM32F469 when SYSCLK=168MHz

### Smartcard Mode Configuration
- ETU = 372 clock cycles (default)
- Prescaler calculation: ceil(pclk1 / (2 * max_card_clk))
- Baud rate = card_clk / ETU
- Guard time = 16 ETU
- Enable NACK on parity error for T=0 protocol

### Pin Configuration for Shield-Lite
- PA2: IO (USART2_TX, AF7, open-drain, external pull-up)
- PA4: CLK (USART2_CK, AF7, push-pull) - NOTE: PA4 not PA3!
- PG10: RST (GPIO output, active LOW)
- PC2: PRES (GPIO input, external pull-down, HIGH = present)
- PC5: PWR (GPIO output, active LOW = power ON)
