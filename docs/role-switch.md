# Role switching — one stick, two firmwares

The M5Stick rig (ai-legion-small) is dual-role hardware:

| Role | Firmware | Serial consumer | Tooling |
|---|---|---|---|
| **bolty** (default) | bolty-rs `bolty-esp32` (board-m5stick) | `bolty-console` daemon → unix socket | bolty-rs `tools/hil/` (`bolty-ctl`, `burn_cycle.py`) |
| **ccid** | this repo's `esp32-ccid` (GemPC-Twin emulator, MFRC522 backend, board-m5stick pins) | pcscd via libccidtwin (`/etc/reader.conf.d/esp32-ccid`) | `host-tools/boltwipe.py`, any PC/SC client |

Both roles talk ISO-14443 through the same MFRC522 on the Grove port
(SDA=GPIO32/SCL=GPIO33). The **only** difference is which side owns protocol
translation: bolty embeds the NTAG424 logic and speaks a line console;
esp32-ccid is a dumb CCID transport so the *host* speaks PC/SC.

## Switching

```bash
tools/switch_role.sh ccid    # → verify prints PC/SC reader name
tools/switch_role.sh bolty   # → verify prints daemon PING
VARIANT=backend-mfrc522,board-m5atom tools/switch_role.sh ccid   # other boards
```

The script orchestrates the full transition (daemon stop, pcscd reader.conf
enable/disable, rebuild, flash, health verification). It exits non-zero if
the post-switch verification fails.

## Why it must be scripted (failure modes it avoids)

1. **Port contention**: the bolty-console daemon holds `/dev/ttyUSB0`; pcscd
   wants the same port. Both roles cannot run simultaneously — the script
   flips ownership atomically with the config files.
2. **FT232 wedge**: flashing with `espflash` toggles DTR/RTS and corrupts
   this USB-UART bridge (esp-rs/espflash#646; 11 bus disconnects in the lab
   kernel log). The script flashes with `esptool @115200 --after no-reset` +
   a manual RTS pulse, and USB-rebinds before flashing to clear stale wedge
   states (bolty-rs lessons B10/B11).
3. **Silent mismatches**: pcscd probing a bolty console produces
   `[FAIL] unknown command` spam; the daemon reading a CCID firmware
   produces silence. The verification step makes the active role explicit.

## Recovery

- Port busy / dead after a failed flash: `echo -n 1-1 | sudo tee
  /sys/bus/usb/drivers/usb/unbind` (then `.../bind`) and re-run the switch.
- pcscd shows no reader after `ccid`: the firmware boots ~6 s — the script
  already waits; re-run `sudo systemctl restart pcscd` if you flashed
  manually.

### Power-cycle policy — a physical unplug is (almost) never needed

The MFRC522 shares the stick's always-on USB power, and every firmware
boot runs `recover_i2c_bus` (9 SCL pulses) + 50 ms settle + a bus probe
before touching the reader. As of 2026-08-25 the rig has run for days with
**zero** reader power cycles while both roles stayed fully functional
(bolty `nfc=ok`, CCID card transactions verified twice each way).

Unplug ONLY if a boot log prints `i2c recovery: SDA still LOW` — that is
the single reader state software cannot clear. Preventive unplugs are a
smell: they indicate advice carried over from a superseded theory (see
bolty-rs docs/lessons-learned.md B13/B14).
