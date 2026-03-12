# CCID Reader — External research request

This document is for **external research only**: web, datasheets, app notes, forums. Someone with no access to our repo can use this file and public sources to research and report back. For a consolidated view of what we know and what we found, see [RESEARCH_CONSOLIDATED.md](RESEARCH_CONSOLIDATED.md).

---

## Context (short)

- **Project:** STM32F469 USB CCID smartcard reader firmware emulating OMNIKEY 3021 (VID:PID 076B:3021). USART2 smartcard mode; pins: PA4 = USART2_CK (card CLK), PA2 = I/O, PG10 = RST, PC5 = PWR, PC2 = card detect.
- **Current issue:** Software sets CLKEN (CR2 = 0x3C00), NACK off during ATR, USART errors cleared; still **zero ATR bytes** (timeout, no TS). Defmt: SR = 0x01C0 on timeout (no framing/parity/overrun/noise errors) → line likely never toggled. Card (SatoChip Seedkeeper) works in Gemalto reader when pcscd is running; in our reader we get "Card is unpowered (0x80100067)".
- **Goal:** Identify hardware/wiring/standards causes of "smartcard sends no ATR at all" so we can fix activation (get at least TS, then full ATR).

---

## What we need you to research externally

### Hardware / wiring

- **STM32F469 (or F4) USART2 smartcard mode:** Which pin is SCLK/CK output (PA4 vs others)? Typical schematic for connecting this to ISO 7816 contact **C3 (CLK)**. Any requirement for alternate function (AF7) or pull/push configuration?
- **Common causes of "smartcard sends no ATR at all" (zero bytes)** when VCC, RST, and I/O are present: clock not reaching C3, wrong voltage, C3/C7 swapped, level shifters, series resistors, or other typical pitfalls. Forum posts or app notes that describe "no ATR" / "zero bytes" and fixes.
- **Specter-DIY or STM32F469-DISCO smartcard / SIM slot:** Pinout (C1–C8) and which MCU pins they connect to. Any known issues with clock or ATR on these boards.

### Reference readers

- **OMNIKEY 3021 / Gemalto** (e.g. PC Twin Reader): Pinout or interface (C1–C8) and voltage (3 V / 5 V). How they drive CLK and RST (timing, levels). Datasheet or public app notes if available.

### Standards / app notes

- **ISO 7816-3 activation:** Minimum clock cycles after RST before first character; any requirement on CLK being present **before** RST. Typical IWT (Initial Waiting Time) and guard time values.
- **ST (STMicroelectronics) app notes or community posts** on STM32 smartcard mode "no ATR" or "zero bytes" and fixes (e.g. GPIO config, clock enable, board layout, CR2/CR3 settings).

### Card damage / overvoltage

- **Slot voltage vs card rating:** If a reader supplies **5 V** at C1 (VCC) and the card is **3 V only**, what damage is typical? Symptoms (e.g. card no longer powers up or responds in a known-good reader). Whether "card doesn't light up" in a reference reader indicates permanent damage.
- **SatoChip Seedkeeper (or similar secure element cards):** Datasheet or public info on max VCC (3 V vs 5 V tolerant). So we know whether our board must be 3 V at the slot before using such cards.

### Seedkeeper applet verification (Gemalto-first goal)

- **We must get the Seedkeeper working in the Gemalto reader first** (connect, unlock with PIN, read a secret). Then we can compare behavior with the STM32 reader.
- **SatoChip Seedkeeper ATR:** What is the typical ATR (or historical bytes) for a SatoChip Seedkeeper card? Our card in the Gemalto returns ATR with historical bytes "JTaxCoreV1" (4A 54 61 78 43 6F 72 65 56 31); SELECT by AID "SeedKeeper" (53 65 65 64 4B 65 65 70 65 72) succeeds. Can a Java Card host both SeedKeeper and another applet (e.g. JTaxCore) and show the other applet’s name in the ATR? How do we confirm from ATR/docs that this card is a Seedkeeper?
- **VERIFY_PIN status word 9C20:** For the SeedKeeper applet (CLA=0xB0, INS_VERIFY_PIN=0x42), what does status word **SW=9C20** mean? Known in our codebase: 9000=success, 63CX=wrong PIN (X attempts left), 9C0C=PIN blocked, 6983=blocked. Please search pysatochip, Satochip-Utils, Toporin/Seedkeeper-Tool, or Satochip/Seedkeeper applet documentation for 9C20 (or 0x9C20) and report: wrong PIN, PIN not initialized, or other.

---

## How to report back

- Add a section **"External research results"** in this file (or in RESEARCH_CONSOLIDATED.md) with:
  - **Source:** URL or document name, and date consulted.
  - **Summary:** What you found (e.g. "PA4 must be AF7 with no other load on C3", or "typical cause of zero ATR is …").
  - **Impact on next steps:** How it affects our firmware or hardware (e.g. "measure VCC at slot before using 3 V cards", or "try X in software").
- **Optional:** Compare our pin list (PA4 = CLK, PA2 = I/O, PG10 = RST, PC5 = PWR, PC2 = card detect) with any official ST or board schematic you find and note any mismatch.

---

## Our pin list (for comparison)

| Contact | Our MCU pin | Function |
|---------|-------------|----------|
| C1 (VCC) | PC5 (PWR, low = power on) | Card power |
| C2 (RST) | PG10 | Reset (active low) |
| C3 (CLK) | PA4 (USART2_CK) | Clock |
| C7 (I/O) | PA2 (USART2_TX, open-drain) | Data |
| PRES | PC2 | Card detect (high = present) |

C4, C5, C6, C8 (GND, VPP, etc.) are board-dependent; not driven by our firmware. Voltage at C1 is supplied by the board; we do not switch it in software.



---

# External Research Results

**Date consulted:** 2026-03-08
**Sources:** ST Community forums, GitHub (Toporin/pysatochip, Toporin/SatochipApplet, specter-diy), ISO 7816-3 specification references, ST Application Note AN4800, Open-ISO7816-Stack, canokey-stm32

---

## 1. STM32F4 USART2 Smartcard Pin Configuration

### Source: STM32F469 Reference Manual (RM0386), specter-diy implementation

**Findings:**
- **USART2_CK (SCLK output)** on STM32F469 is available on **PA4** with **AF7** (alternate function 7)
- **USART2_TX (I/O)** is on **PA2** with **AF7**
- The specter-diy reference implementation configures:
  - **I/O pin (PA2):** `MP_HAL_PIN_MODE_ALT_OPEN_DRAIN` with `MP_HAL_PIN_PULL_UP`
  - **CLK pin (PA4):** `MP_HAL_PIN_MODE_ALT` (push-pull) with `MP_HAL_PIN_PULL_UP`

**Critical finding from specter-diy `scard_io.c`:**
```c
// I/O pin: open-drain alternate function with pull-up
ok = mp_hal_pin_config_alt(pin_find(io_pin), MP_HAL_PIN_MODE_ALT_OPEN_DRAIN, 
                           MP_HAL_PIN_PULL_UP, AF_FN_USART, usart_id);
// CLK pin: push-pull alternate function with pull-up
ok = ok && mp_hal_pin_config_alt(pin_find(clk_pin), MP_HAL_PIN_MODE_ALT, 
                                  MP_HAL_PIN_PULL_UP, AF_FN_USART, usart_id);
```

**Impact on next steps:**
1. Verify PA4 is configured as AF7 push-pull (not open-drain)
2. Verify PA2 is configured as AF7 **open-drain** with pull-up (critical for half-duplex)
3. Ensure external 10kΩ-20kΩ pull-up on I/O line if internal pull-up is too weak

---

## 2. ISO 7816-3 Activation Sequence & Timing

### Source: ISO/IEC 7816-3:2006, ST Community, Open-ISO7816-Stack

**Standard Activation Sequence (from Open-ISO7816-Stack `reader_hal.c`):**
```c
// Cold reset sequence:
READER_HAL_Delay(50);                    // 50ms delay first
READER_HAL_SetPwrLine(READER_HAL_STATE_ON);   // VCC ON
READER_HAL_Delay(1);                     // 1ms delay
READER_HAL_SetClkLine(READER_HAL_STATE_ON);   // CLK ON
READER_HAL_Delay(1);                     // 1ms delay  
READER_HAL_SetRstLine(READER_HAL_STATE_ON);   // RST ON
```

**Critical timing requirements:**
- **CLK MUST be present BEFORE RST goes HIGH** (not simultaneous)
- RST must be held LOW for at least **40,000 clock cycles** after CLK starts before going HIGH
- At 3.57 MHz, 40,000 cycles = **11.2ms** minimum CLK-before-RST delay
- ATR must begin between **400 and 40,000 clock cycles** after RST rising edge
- **Initial Waiting Time (IWT):** Maximum 9,600 etu between consecutive ATR characters
- **Guard Time:** 12 etu (10 data bits + 2 guard bits)

**Impact on next steps:**
1. Ensure RST is held LOW during VCC and CLK ramp-up
2. Add delay of at least 11.2ms (at 3.57MHz) or 40,000 CLK cycles between CLK ON and RST HIGH
3. Current code may have RST and CLK timing wrong - verify sequence

---

## 3. Common Causes of "Zero ATR" / "No Bytes Received"

### Source: ST Community forums, ISO 7816-3 references, Stack Overflow

**Identified causes (ranked by likelihood):**

1. **CLK not reaching card (C3)** - Most common cause
   - PA4 not properly configured as AF output
   - Wrong AF number (should be AF7 for USART2)
   - CLK pin not toggling at all

2. **CLK not present before RST** - Violates ISO 7816-3
   - Card expects CLK running before RST transition
   - Solution: Ensure CLK ON → wait → RST HIGH sequence

3. **Insufficient I/O pull-up**
   - STM32 internal pull-up (typically 40kΩ) may be too weak
   - ISO 7816-3 requires <1μs rise time
   - Solution: Add external 10kΩ-20kΩ pull-up resistor on I/O line

4. **Baud rate mismatch**
   - Initial baud rate = CLK_freq / 372 (e.g., 3.57MHz / 372 ≈ 9600 bps)
   - Wrong BRR register value causes missed TS byte

5. **NACK not disabled during ATR**
   - NACK should be OFF during ATR reception
   - If enabled, reader may pull line low and disrupt card transmission

6. **Insufficient card current (ICC)**
   - JavaCards have high inrush current during first 40,000 cycles
   - LDO or power switch current limit may cause VCC dip
   - Card resets before ATR completes

**Impact on next steps:**
1. Use oscilloscope to verify CLK (PA4) is toggling at expected frequency
2. Verify I/O line has proper pull-up and clean rise times
3. Add delay between CLK ON and RST HIGH
4. Verify USART BRR calculation for initial baud rate

---

## 4. OMNIKEY 3021 / Gemalto Reference Reader

### Source: HID Global datasheet, USB CCID spec

**Specifications:**
- **VID:PID:** 076B:3021
- **Voltage:** Auto-detects Class A (5V), B (3V), C (1.8V) cards
- **CLK frequency:** Up to 12 MHz (typically 4.8 MHz or 5 MHz)
- **RST timing:** Held LOW for at least 400 clock cycles before transition to HIGH
- **Protocols:** T=0, T=1

**USB descriptor requirements for emulation:**
- **Class:** 0x0B (Smart Card)
- **Subclass:** 0x00
- **Protocol:** 0x00

**Impact on next steps:**
- Verify voltage at card slot (should be 3V for SatoChip/Seedkeeper)
- Ensure CLK frequency is in 1-5 MHz range

---

## 5. SatoChip/Seedkeeper Card & SW=9C20 Status Word

### Source: GitHub - Toporin/SatochipApplet (src/org/satochip/applet/CardEdge.java)

### **CRITICAL FINDING: SW=9C20 = SECURE CHANNEL REQUIRED**

Found in `CardEdge.java` line 253:
```java
/** Secure channel */
private final static short SW_SECURE_CHANNEL_REQUIRED = (short) 0x9C20;
private final static short SW_SECURE_CHANNEL_UNINITIALIZED = (short) 0x9C21;
private final static short SW_SECURE_CHANNEL_WRONG_IV = (short) 0x9C22;
private final static short SW_SECURE_CHANNEL_WRONG_MAC = (short) 0x9C23;
```

**Meaning:** SW=9C20 does **NOT** mean wrong PIN or PIN not initialized. It means **a secure encrypted channel must be established** before VERIFY_PIN can be executed.

**Other status words from pysatochip:**
- `0x9000` = Success
- `0x63C0` to `0x63CF` = Wrong PIN (X = remaining attempts)
- `0x9C02` = Authentication failed
- `0x9C04` = Setup not done
- `0x9C06` = Authentication required
- `0x9C0C` = PIN blocked
- `0x6983` = Blocked

**ATR Historical Bytes:**
- "JTaxCoreV1" in ATR indicates the card has the JTaxCore applet (different from SeedKeeper)
- A Java Card can host multiple applets
- SELECT by AID "SeedKeeper" (53 65 65 64 4B 65 65 70 65 72) to switch to SeedKeeper applet

**Impact on next steps:**
1. Before VERIFY_PIN, must establish secure channel (ECDH key exchange)
2. The card is likely working correctly - the error is expected behavior without secure channel
3. Reference pysatochip library for secure channel implementation

---

## 6. Card Voltage / Damage Assessment

### Source: ISO 7816-3, Satochip documentation

**SatoChip/Seedkeeper cards:**
- Operate at **3V (Class B)** - NOT 5V tolerant
- Supplying 5V to a 3V-only card can cause permanent damage

**Symptoms of overvoltage damage:**
- Card no longer responds in known-good reader
- No ATR at all (complete silence)
- Card may appear completely dead

**Testing recommendation:**
- Measure voltage at C1 (VCC) contact with multimeter
- Ensure it's 3.0V-3.3V, not 5V
- If card was exposed to 5V, it may be permanently damaged

**Impact on next steps:**
1. **CRITICAL:** Verify slot voltage is 3V before using SatoChip/Seedkeeper
2. If card works in Gemalto but not STM32 reader, voltage is likely OK
3. If card doesn't work in Gemalto either, it may be damaged

---

## 7. Reference Implementation: specter-diy STM32 Smartcard

### Source: /specter-diy/f469-disco/usermods/scard/

**Key implementation details from `scard_io.c`:**

```c
// USART Smartcard configuration
sc_handle->Init = (SMARTCARD_InitTypeDef) {
  .WordLength  = SMARTCARD_WORDLENGTH_9B,
  .StopBits    = SMARTCARD_STOPBITS_1_5,
  .Parity      = SMARTCARD_PARITY_EVEN,
  .Mode        = SMARTCARD_MODE_TX_RX,
  .BaudRate    = baudrate,           // card_clk / 372
  .CLKPolarity = SMARTCARD_POLARITY_LOW,
  .CLKPhase    = SMARTCARD_PHASE_1EDGE,
  .CLKLastBit  = SMARTCARD_LASTBIT_ENABLE,
  .Prescaler   = prescaler,          // clk_in / (2 * 5MHz)
  .GuardTime   = 16U,
  .NACKState   = SMARTCARD_NACK_DISABLE  // Important: NACK off during ATR!
};
```

**Half-duplex handling:**
```c
// In smartcard mode, TX and RX are internally connected
// Must enable only one at a time to prevent echo
if(dir == hd_dir_tx) {
  usart->CR1 &= ~USART_CR1_RE;  // Disable RX
  usart->CR1 |= USART_CR1_TE;   // Enable TX
} else if(dir == hd_dir_rx) {
  // Wait for TX complete
  if(usart->CR1 & USART_CR1_TE) { while(!(usart->SR & USART_SR_TC)) { } }
  usart->CR1 &= ~USART_CR1_TE; // Disable TX
  usart->CR1 |= USART_CR1_RE;  // Enable RX
}
```

**Key constants:**
```c
#define SCARD_MAX_CLK_FREQUENCY_HZ  (5000000LU)  // 5MHz max
#define SCARD_ETU                   (372U)        // Initial ETU
```

---

## 8. Summary of Recommended Actions

### Hardware checks:
1. **Verify PA4 (CLK) is toggling** with oscilloscope - should see clock output
2. **Verify I/O pull-up** - add external 10kΩ-20kΩ if needed
3. **Measure slot voltage** at C1 - must be 3V for SatoChip
4. **Check pin configuration:**
   - PA4: AF7, push-pull, pull-up
   - PA2: AF7, **open-drain**, pull-up

### Firmware changes:
1. **Fix activation sequence:** VCC ON → 50ms delay → CLK ON → 11ms delay → RST HIGH
2. **Ensure NACK is disabled** during ATR reception
3. **Verify baud rate calculation:** BRR = PCLK / (16 * baudrate), baudrate = CLK_freq / 372
4. **Implement half-duplex switching** (disable TX before RX)

### For SW=9C20 issue:
1. **Not a PIN error** - secure channel required
2. Must implement ECDH secure channel establishment before VERIFY_PIN
3. Reference pysatochip library for implementation details

---

## 9. Files & Resources Referenced

| Resource | URL/Path |
|----------|----------|
| SatochipApplet source | https://github.com/Toporin/SatochipApplet |
| pysatochip library | https://github.com/Toporin/pysatochip |
| specter-diy smartcard | /specter-diy/f469-disco/usermods/scard/ |
| Open-ISO7816-Stack | /Open-ISO7816-Stack/src/reader_hal.c |
| ST Community: STM32F4 ISO7816 | https://community.st.com/t5/stm32-mcus-products/connect-stm32f4-with-smart-card-iso7816/td-p/361766 |
| ST AN4800 | https://www.st.com/resource/en/application_note/an4800-smartcard-interface-based-on-stm32cube-firmware-stmicroelectronics.pdf |
| ISO 7816-3 reference | https://cardwerk.com/iso-7816-part-3/ |