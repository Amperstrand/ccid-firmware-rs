# BLE debug-console capture (issue #66)

Reads firmware logs from any esp32-ccid `-ble` build (BLE NUS GATT
notifications) while the CCID wire stays untouched. For testers: this is
the debug channel for `c3-ccid-ble` (nucula) and `m5stick-ble` boards —
their USB/serial port carries CCID only, so do not expect log output
there. Flashing and pcscd verification are unchanged.

```bash
# one-time per boot of the bench host
sudo rfkill unblock bluetooth && sudo hciconfig hci0 up
pip install bleak

python3 ble_log_capture.py             # scan for ESP32-CCID-Debug*, connect, follow
python3 ble_log_capture.py --list      # just scan
python3 ble_log_capture.py --send 'x'  # write a line to the NUS RX characteristic
```

What you will see on attach: an `=== BLE log attached; N older lines
dropped ===` banner (the device retains the newest 32 log lines while
nobody is connected), then retained history, then live lines.

Gotchas (see also AGENTS.md "BLE Debug Console"):

- If connects time out while the device keeps advertising, check `btmon`:
  `Create Connection (0x0005)` + `Page Timeout` means bluetoothd is PAGING
  the peer over classic BR/EDR instead of opening the LE link. Since the
  2026-10-15 bluez upgrade (5.72-0ubuntu5.7 → 5.72-0ubuntu5.8), passing
  the discovery object to BleakClient is no longer sufficient for an
  address with classic history on the adapter (the m5stick's SPP-era
  address). The capture tool now drops the stale peer object and connects
  against a fresh live-scan discovery; if that regresses again, the remedy
  is host-side — pin bluez 5.72-0ubuntu5.7 or use a central adapter with
  no classic memory of the address (e.g. a USB BLE dongle as hci1).
- A phone with nRF Connect (or any NUS terminal app) works as an
  alternative central: service `6e400001-…`, notify characteristic
  `6e400003-…`.

HIL note: automated tests never read BLE log lines — verification
stays on standard (non-ble) builds via FWID console markers, pcscd
queries, and coredumps for crash forensics.
