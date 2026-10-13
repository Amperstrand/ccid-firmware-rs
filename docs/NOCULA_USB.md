# Nucula USB — hardware limitation and workarounds

*Bench note 2026-10-13. Scope: why the nucula (ESP32-C3 + PN7160) cannot be a wired USB smartcard reader for pcscd, what software can and cannot do about it, and what it would take in hardware. The m5stick (ESP32 classic + CP210x bridge, GemPC serial CCID over UART0, `libccidtwin`-served) is the working contrast case. Method: ESP-IDF v5.5.5 register headers (vendored tree + GitHub), Espressif docs, pcscd serial-driver source, this repo's bench log (#117).*

## 1. Root cause — silicon, not software

The ESP32-C3 has **no USB-OTG / USB-device controller peripheral**. Its only USB hardware is the fixed-function **USB Serial/JTAG controller** on GPIO18/19. Espressif's own wording: *"a fixed-function USB device … implemented entirely in hardware … it cannot be reconfigured to perform any function other than a serial port and JTAG debugging functionality. This is in contrast to the USB OTG controllers in some ESP chips"* (S2/S3) — [ESP-IDF USB Serial/JTAG Console guide](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-guides/usb-serial-jtag-console.html). The ESP-IDF SoC tree confirms it structurally: `esp32c3/register/soc/` contains only `usb_serial_jtag_*` headers, no OTG/`usb_wrap` register file ([esp-idf v5.5.5 tree](https://github.com/espressif/esp-idf/tree/v5.5.5/components/soc/esp32c3/register/soc)). Datasheet: [ESP32-C3 datasheet](https://www.espressif.com/sites/default/files/documentation/esp32-c3_datasheet_en.pdf).

What the port exposes: one CDC-ACM interface (data + notification endpoints) plus the JTAG pair, descriptors burned into ROM — enumeration as `Espressif USB JTAG/serial debug unit` (VID:PID `0x303A:0x1001`, [esptool docs](https://docs.espressif.com/projects/esptool/en/latest/esp32c3/esptool/advanced-options.html)). **No CCID class, no way to add one.** App code *can* push arbitrary bytes through the CDC data endpoints — the `usb_serial_jtag` driver is a sanctioned byte pipe (confirmed by Espressif's igrr in [esp-idf #11955](https://github.com/espressif/esp-idf/issues/11955), demonstrated by the [usb_serial_jtag_echo example](https://github.com/espressif/esp-idf/tree/master/examples/peripherals/usb_serial_jtag/usb_serial_jtag_echo)) — which is exactly what our firmware does (`UsbSerialDriver` in `firmware/esp32-ccid/src/pn7160_ccid.rs`).

The second half of the wall: the peripheral **decodes host DTR/RTS into chip reset** (`rst:0x15 USB_UART_CHIP_RESET`; *"the peripheral interprets the RTS serial control signal as a core reset"* — [esptool advanced options](https://docs.espressif.com/projects/esptool/en/latest/esp32c3/esptool/advanced-options.html)). And pcscd's serial CCID driver (`libccidtwin`) does line-control chatter on **every** reader open: `open()`, then `TIOCMSET` clearing RTS ("plug-and-play inhibition"), then `tcsetattr` to 115200 ([ccid_serial.c, `OpenSerialByName`](https://github.com/LudovicRousseau/CCID/blob/master/src/ccid_serial.c)). Each open therefore chip-resets the board; the boot banner races the GemPC handshake; the reader error-loops — the reset vortex, bench-verified end-to-end 2026-10-11 (#117).

Crucially, the C3 reset decoder has **no software kill-switch**: the C3 register file has no `CHIP_RST_DIS` / threshold register of any kind ([esp32c3 `usb_serial_jtag_reg.h`](https://github.com/espressif/esp-idf/blob/v5.5.5/components/soc/esp32c3/register/soc/usb_serial_jtag_reg.h) — verified by grep in the vendored v5.5.5 tree), and ESP-IDF Kconfig offers only `USJ_ENABLE_USB_SERIAL_JTAG` and `USJ_NO_AUTO_LS_ON_CONNECTION` — nothing that touches reset behavior. The disable bit exists only on **C6/C61 silicon** (`USB_SERIAL_JTAG_USB_UART_CHIP_RST_DIS`, [esp32c6 `usb_serial_jtag_reg.h`](https://github.com/espressif/esp-idf/blob/v5.5.5/components/soc/esp32c6/register/soc/usb_serial_jtag_reg.h); community write-up: [esp32.com t=43163](https://esp32.com/viewtopic.php?t=43163)).

## 2. Workaround A — GemPC serial CCID over the CDC port

**(a) Direct pcscd → `/dev/ttyACM0`: dead.** The DTR/RTS reset is decoded in fixed hardware before software sees anything; no driver callback, Kconfig, or register write on C3 can dodge it. Verdict: **infeasible on C3** (bench: reset vortex, `/etc/reader.conf.d/nucula-ccid.disabled`).

**(b) Host-side PTY broker: works.** `host-tools/nucula-ccid-broker.py` (#117) is the sole CDC opener: pyserial with `dtr=False; rts=False` set *before* `open()`, boot-settle drain, a framed GetSlotStatus serving-gate, then pcscd's `libccidtwin` talks to a PTY where DTR/RTS is a no-op. Bench-verified: enumerate, ATR, SELECT sessions, >5.5 min stability, ~0.5 ms broker overhead, ~300 ms/command CDC floor. Log contention is solved in firmware: `log::set_max_level(Off)` + `esp_log_level_set(NONE)` (clean-wire ceiling, issue #131), diagnostics on netlog UDP/BLE. Verdict: **feasible, bench-proven, wired** — cost is one host daemon.

## 3. Workaround B — external USB device controller

- **USB-UART bridge (CP210x/FT231X/CH340) on a nucula UART** — the m5stick pattern, proven in this repo (`/dev/ttyUSB0`, `libccidtwin`-served). Bridge DTR is a wire you simply don't route to EN. ~$2 BOM, board rework. Verdict: **works, changes the board**.
- **True CCID gadget on OTG-capable silicon (S2/S3, or MAX3421E-style SPI device controller):** no mainline support anywhere — Linux gadget has no `f_ccid` (verified in `drivers/usb/gadget/function/`), TinyUSB mainline `src/class/` has no CCID class ([tinyusb tree](https://github.com/hathach/tinyusb/tree/master/src/class)). Custom class firmware, weeks. Verdict: **possible, disproportionate**. CH375/CH376 CCID precedent: none found — *unverified*.

## 4. Workaround C — vendor commands / alternate functions

None exist. Control requests and descriptors are handled entirely by the fixed ROM/hardware logic; the JTAG endpoints feed the TAP, not software ([fixed-function statement](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-guides/usb-serial-jtag-console.html)). Community practice reduces to "open the port without touching DTR/RTS" (e.g. [Arduino forum](https://forum.arduino.cc/t/software-reset-via-usb-incorrect-with-lolin-esp32-c3-pico/1208646), [r/esp32](https://www.reddit.com/r/esp32/comments/1wioi0t/esp32_resets_every_time_you_open_the_serial/)) — i.e. workaround A's broker. USB/IP is equally dead: no device controller to export (transport-research-154.md §2.3). Verdict: **no trick exists on C3**.

## 5. Recommendation

For USB-style usage today: **WiFi, vpcd reversed connection** — `docs/VPCD_WIFI_RUNBOOK.md`, bench-verified 2026-10-13 (ATR, 7,914-read soak, GPG ×3, GP install). No host daemon; the broker stays as the wired fallback.

## What works / what doesn't

| Path | Verdict |
|---|---|
| USB CCID device class on C3 | ✗ impossible — no OTG, fixed-function ROM |
| pcscd → `/dev/ttyACM0` direct | ✗ reset vortex (`rst:0x15`), no C3 disable bit |
| CDC + host PTY broker (#117) | ✓ bench-proven, wired, one daemon, ~300 ms/cmd |
| WiFi vpcd reversed (runbook) | ✓ bench-proven, recommended |
| USB-UART bridge rework (m5stick pattern) | ✓ works, hardware change |
| S2/S3/MAX3421 CCID gadget | ~ possible, no mainline CCID stack, weeks |
| DTR-reset disable on C3 (sw) | ✗ C6/C61 silicon only (`CHIP_RST_DIS`) |

## Sources

1. [ESP-IDF — USB Serial/JTAG Console (C3)](https://docs.espressif.com/projects/esp-idf/en/stable/esp32c3/api-guides/usb-serial-jtag-console.html) · 2. [esp-idf #11955](https://github.com/espressif/esp-idf/issues/11955) · 3. [C3 register header](https://github.com/espressif/esp-idf/blob/v5.5.5/components/soc/esp32c3/register/soc/usb_serial_jtag_reg.h) / [C6 `CHIP_RST_DIS`](https://github.com/espressif/esp-idf/blob/v5.5.5/components/soc/esp32c6/register/soc/usb_serial_jtag_reg.h) · 4. [esptool — advanced options](https://docs.espressif.com/projects/esptool/en/latest/esp32c3/esptool/advanced-options.html) · 5. [ccid_serial.c](https://github.com/LudovicRousseau/CCID/blob/master/src/ccid_serial.c) · 6. [esp32.com t=43163](https://esp32.com/viewtopic.php?t=43163) · 7. [esp-idf #18996](https://github.com/espressif/esp-idf/issues/18996) · 8. [TinyUSB classes](https://github.com/hathach/tinyusb/tree/master/src/class) · 9. [ESP32-C3 datasheet](https://www.espressif.com/sites/default/files/documentation/esp32-c3_datasheet_en.pdf) · 10. Repo: `AGENTS.md` (hw wall, bench), `docs/VPCD_WIFI_RUNBOOK.md`, `docs/transport-research-154.md`, `host-tools/nucula-ccid-broker.py`, issue #117.
