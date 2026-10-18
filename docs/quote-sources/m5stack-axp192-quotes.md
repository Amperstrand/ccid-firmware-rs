# M5Stack AXP192 Arduino references — verbatim quote source

Provenance (both fetched 2026-10-18 via the GitHub contents API,
transcribed verbatim — whitespace only as shown):

- m5stack/M5StickC @ `0490c6a925cfc71f89f2f780d7fc65f060634ee4`,
  `src/AXP192.cpp` (blob `513b074b4d171c3fd5ac30d921efbd215fb0585c`),
  MIT license. https://github.com/m5stack/M5StickC
- m5stack/M5StickC-Plus @ `4c87db9b3e0e2c6309f6e72c596032084542e7fa`,
  `src/AXP192.cpp` (blob `9068e45678f9a8d4b96b3edca798e777baea869d`),
  MIT license. https://github.com/m5stack/M5StickC-Plus
- sipeed/MaixPy-v1 @ `d8901fd2272f000226f1c1037c1eb7c412b88e66`
  (the MaixPy-Twatch AXP192/AXP202 port),
  `projects/maixpy_twatch/builtin_py/axp202.py` (blob
  `b3449cc4c24d957608104e69b75dbe3d8c47db0a`) and
  `projects/maixpy_twatch/builtin_py/axp_constants.py` (blob
  `32ebb7654aef35b5159aecc0fa46cb45e95978d4`), Apache-2.0.
  https://github.com/sipeed/MaixPy-v1

The AXP192 datasheet itself is a PDF with no tracked text source in
this repo; the OFFICIAL M5Stack Arduino implementations are the
reviewer-cited behavioral reference for the m5stick_lcd driver
(issue #275: ADC nibble + VBUS register). Where the datasheet register
map is referenced in prose (§8.2 power status, §8.26 battery voltage)
the code comment says so, but the verbatim-checked authority for the
decode forms below is the code quoted here.

Quoted by `specquotes.toml` → `[sources.M5AXP]` (marker `M5AXP`) from
`firmware/esp32-ccid/src/m5stick_lcd.rs`.

## Battery voltage — 12-bit read of reg 0x78/0x79 (M5StickC, identical in M5StickC-Plus)

`Read12Bit` defines the reference nibble convention: the low byte
(reg 0x79) is ADDED whole — its low bits are the low bits of the
12-bit result (issue #275 defect 1: the old driver shifted them out).

> uint16_t AXP192::Read12Bit(uint8_t Addr) {
>     uint16_t Data = 0;
>     uint8_t buf[2];
>     ReadBuff(Addr, 2, buf);
>     Data = ((buf[0] << 4) + buf[1]);
>     return Data;
> }

> float AXP192::GetBatVoltage() {
>     float ADCLSB    = 1.1 / 1000.0;
>     uint16_t ReData = Read12Bit(0x78);
>     return ReData * ADCLSB;
> }

## Battery presence — reg 0x01 bit 0x20 (M5StickC, identical in M5StickC-Plus)

GetBatState reads the charge-status register (0x01) bit 0x20 for
"battery exist" — this is the bit the old driver mistook for VBUS
(issue #275 defect 2). (M5Stack's own `|`-instead-of-`&` typo makes
the function always-true; quoted verbatim — the register/bit intent
is what is cited, the typo is why this is a presence-PRECEDENT quote,
not copied logic.)

> // Return True = Battery Exist
> bool AXP192::GetBatState() {
>     if (Read8bit(0x01) | 0x20)
>         return true;
>     else
>         return false;
> }

## VBUS presence — reg 0x00 bit 5 (sipeed MaixPy-v1 AXP192 port)

The M5StickC libraries expose the input-power register only as a raw
byte (`GetInputPowerStatus`); the MaixPy-Twatch AXP192 port is the
reference that reads it for VBUS presence: `AXP202_STATUS = 0x00`,
bit mask 5.

> // Input Power Status
> uint8_t AXP192::GetInputPowerStatus() {
>     return Read8bit(0x00);
> }

> AXP202_STATUS = 0x00
> AXP202_MODE_CHGSTATUS = 0x01

>     def isVBUSPlug(self):
>         data = self.read_byte(AXP202_STATUS)
>         return data & self.__BIT_MASK(5)
