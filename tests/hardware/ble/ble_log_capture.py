#!/usr/bin/env python3
"""BLE debug-console capture for esp32-ccid `-ble` builds (issue #66).

Connects to the firmware's Nordic UART Service (NUS) debug GATT server
and prints log notifications to stdout while the CCID wire (USB-CDC /
UART0) stays untouched. Uses bleak over the bench's BlueZ adapter:

    pip install bleak
    sudo hciconfig hci0 up  # if the adapter is DOWN

Usage:
    ./ble_log_capture.py                       # scan + first match, follow forever
    ./ble_log_capture.py --name ESP32-CCID-Debug
    ./ble_log_capture.py --list                # just scan and exit
    ./ble_log_capture.py --send 'help'         # write a line to the NUS RX char
"""

import argparse
import asyncio
import datetime
import sys

NUS_SERVICE = "6e400001-b5a3-f393-e0a9-e50e24dcca9e"
NUS_RX_CHAR = "6e400002-b5a3-f393-e0a9-e50e24dcca9e"  # central → device (write)
NUS_TX_CHAR = "6e400003-b5a3-f393-e0a9-e50e24dcca9e"  # device → central (notify)

DEFAULT_NAME_PREFIX = "ESP32-CCID-Debug"


def stamp() -> str:
    return datetime.datetime.now().strftime("%H:%M:%S.%f")[:-3]


async def scan_and_pick(bleak, prefix: str, list_only: bool):
    devices = await bleak.BleakScanner.discover(timeout=8.0)
    matches = [d for d in devices if d.name and d.name.startswith(prefix)]
    if list_only or not matches:
        for d in devices:
            mark = "*" if d in matches else " "
            print(f"{mark} {d.address}  {d.name!r}  rssi={getattr(d, 'rssi', '?')}")
        if list_only:
            sys.exit(0 if matches else 1)
    if not matches:
        print(f"no BLE peripheral named {prefix}* found (is the -ble build "
              f"flashed and advertising?)", file=sys.stderr)
        sys.exit(1)
    if len(matches) > 1:
        print("multiple matches:", file=sys.stderr)
        for d in matches:
            print(f"  {d.address}  {d.name!r}", file=sys.stderr)
    chosen = matches[0]
    print(f"[{stamp()}] connecting to {chosen.name} @ {chosen.address}", file=sys.stderr)
    # BlueZ resolves a bare address to a stale BR/EDR Device object and
    # pages it over classic (Page Timeout); the discovery object carries
    # the LE transport explicitly.
    return chosen


async def follow(device, send_line: str | None) -> None:
    from bleak import BleakClient

    def on_notify(_char_spec, data: bytearray):
        sys.stdout.write(data.decode("utf-8", errors="replace"))
        sys.stdout.flush()

    disconnected = asyncio.get_running_loop().create_future()

    def on_disconnect(client):
        if not disconnected.done():
            disconnected.set_result(None)

    async with BleakClient(device, disconnected_callback=on_disconnect) as client:
        print(f"[{stamp()}] connected; MTU={client.mtu_size}; "
              f"notifications on {NUS_TX_CHAR}", file=sys.stderr)
        await client.start_notify(NUS_TX_CHAR, on_notify)
        if send_line:
            await client.write_gatt_char(NUS_RX_CHAR, (send_line + "\n").encode())
        print(f"[{stamp()}] following (Ctrl-C to stop)", file=sys.stderr)
        await disconnected
        print(f"\n[{stamp()}] device disconnected", file=sys.stderr)


async def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--name", default=DEFAULT_NAME_PREFIX,
                    help=f"device-name prefix (default {DEFAULT_NAME_PREFIX})")
    ap.add_argument("--list", action="store_true", help="scan and exit")
    ap.add_argument("--send", metavar="LINE", help="write LINE to the NUS RX characteristic")
    args = ap.parse_args()

    import bleak  # deferred so --help works without bleak installed

    address = await scan_and_pick(bleak, args.name, args.list)
    await follow(address, args.send)


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
