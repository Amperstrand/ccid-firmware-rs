# Voltage / schematic investigation prompt

Copy the block below to an external LLM or use it yourself to drive schematic or measurement work for slot voltage (C1) and card safety.

---

I have a smartcard reader built with an STM32F469 (likely STM32F469-DISCO, possibly with a Specter or similar smartcard shield). The reader connects to a smartcard via ISO 7816 contacts; firmware drives:

- **C1 (VCC):** via pin PC5 — logic low = power on, high = power off. The actual voltage at the card contact C1 is supplied by the board (we do not switch 3V/5V in software).
- **C2 (RST), C3 (CLK), C7 (I/O):** PA2, PA4, PG10 as per STM32 USART2 smartcard mode.

I need to determine:

1. What voltage does the smartcard slot actually supply at contact C1 (VCC)? Is it 3 V, 5 V, or configurable? Where in the schematic or BOM is this defined (e.g. LDO output, power rail name)?

2. If there is no schematic available: what is the standard or typical VCC for (a) STM32F469-DISCO expansion headers, and (b) common smartcard shields (e.g. Specter-DIY) that plug into such boards?

3. The Satochip Seedkeeper card is documented as 3 V only (not 5 V tolerant). What are the risks and typical symptoms if a 3 V–only card was briefly powered at 5 V? Does "card no longer lights up or responds in a known-good reader" usually indicate permanent damage?

4. What should I do in software once I know the slot voltage? (e.g. If slot is fixed 3 V: set CCID descriptor bVoltageSupport to 0x02 and reject bPowerSelect for 5V/1.8V. If 5 V: document "do not use 3 V–only cards" and optionally set bVoltageSupport to 0x01.)

Please answer in a short, actionable form and cite any schematic section, datasheet, or app note you rely on.

---

See also [RESEARCH_AGENDA.md](RESEARCH_AGENDA.md) §5.7 (card damage from overvoltage) and [IMPLEMENTATION_COMPARISON.md](IMPLEMENTATION_COMPARISON.md) §7.2 (voltage / safety).
