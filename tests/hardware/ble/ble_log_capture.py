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

BlueZ dual-address trap (bench 2026-10-16, bluez 5.72-0ubuntu5.8 — the
2026-10-15 distro upgrade changed peer typing): for an address bluetoothd
has typed BR/EDR (the m5stick's 4C:75:25… carries SPP-era classic
history on this adapter), Device1.Connect() PAGES over classic —
Create Connection (0x0005) + Page Timeout in btmon — while the
peripheral (BLE-only firmware) keeps advertising fine. Passing the
discovery object alone stopped being enough after that upgrade. This
tool therefore connects with the FRESHEST possible LE-typed object:
remove any existing peer object, then discover-and-connect in one
process with the scanner still running when Connect is issued. If
connects still time out, check `btmon` for the classic-page signature —
the remedy is then host-side (pin bluez 5.72-0ubuntu5.7, or use a
central adapter with no classic memory of the address, e.g. a USB BLE
dongle as hci1).
"""

import argparse
import asyncio
import datetime
import subprocess
import sys

NUS_SERVICE = "6e400001-b5a3-f393-e0a9-e50e24dcca9e"
NUS_RX_CHAR = "6e400002-b5a3-f393-e0a9-e50e24dcca9e"  # central → device (write)
NUS_TX_CHAR = "6e400003-b5a3-f393-e0a9-e50e24dcca9e"  # device → central (notify)

DEFAULT_NAME_PREFIX = "ESP32-CCID-Debug"


def stamp() -> str:
    return datetime.datetime.now().strftime("%H:%M:%S.%f")[:-3]


def drop_stale_peer(address: str) -> None:
    """Drop any existing BlueZ Device object for the peer so the next
    LE advertisement recreates it LE-typed (see module docstring)."""
    r = subprocess.run(["bluetoothctl", "remove", address],
                       capture_output=True, text=True, timeout=10)
    # "not available" simply means no stale object existed.
    print(f"[{stamp()}] bluetoothctl remove {address}: "
          f"{(r.stdout + r.stderr).strip().splitlines()[-1] if (r.stdout + r.stderr).strip() else 'ok'}",
          file=sys.stderr)


async def discover_target(bleak, prefix: str, timeout: float = 8.0):
    devices = await bleak.BleakScanner.discover(timeout=timeout)
    return devices, [d for d in devices if d.name and d.name.startswith(prefix)]


async def scan_and_pick(bleak, prefix: str, list_only: bool):
    devices, matches = await discover_target(bleak, prefix)
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
    return chosen


async def connect_fresh(bleak, prefix: str, connect_timeout: float = 15.0):
    """Scan → (re)discover the peer on a FRESH LE-typed object → connect
    while a scan is still live, so bluetoothd sees an in-scan LE peer at
    Connect time (dual-address trap, module docstring).

    Returns (client_context_manager, device); the caller enters the
    context and stops the scanner once connected.
    """
    # First pass: find the address (also serves --list semantics upstream).
    _, matches = await discover_target(bleak, prefix)
    if not matches:
        return None, None
    address = matches[0].address
    drop_stale_peer(address)

    # Second pass with a LIVE scanner: fresh object from the LE
    # advertisement, Connect issued before the scan stops. Match the
    # address saved from the first pass — scan ordering is not stable
    # and multiple boards share the ESP32-CCID-Debug name (Codex P2 on
    # #212): a prefix rematch could pick the WRONG board and connect to
    # it without dropping ITS stale object.
    target_address = matches[0].address
    scanner = bleak.BleakScanner()
    await scanner.start()
    deadline = asyncio.get_event_loop().time() + 10.0
    fresh = None
    try:
        while fresh is None and asyncio.get_event_loop().time() < deadline:
            for d in scanner.discovered_devices:  # bleak 3.x property
                if d.address == target_address:
                    fresh = d
                    break
            if fresh is None:
                await asyncio.sleep(0.3)
    finally:
        pass  # scanner kept running through Connect; caller stops it
    if fresh is None:
        await scanner.stop()
        return None, None
    print(f"[{stamp()}] connecting to {fresh.name} @ {fresh.address} "
          f"(live-scan fresh object)", file=sys.stderr)
    return fresh, scanner


async def follow(device, scanner, send_line: str | None) -> None:
    from bleak import BleakClient

    def on_notify(_char_spec, data: bytearray):
        sys.stdout.write(data.decode("utf-8", errors="replace"))
        sys.stdout.flush()

    disconnected = asyncio.get_running_loop().create_future()

    def on_disconnect(client):
        if not disconnected.done():
            disconnected.set_result(None)

    try:
        async with BleakClient(device, disconnected_callback=on_disconnect,
                               timeout=15.0) as client:
            if scanner is not None:
                await scanner.stop()
            print(f"[{stamp()}] connected; MTU={client.mtu_size}; "
                  f"notifications on {NUS_TX_CHAR}", file=sys.stderr)
            await client.start_notify(NUS_TX_CHAR, on_notify)
            if send_line:
                await client.write_gatt_char(NUS_RX_CHAR, (send_line + "\n").encode())
            print(f"[{stamp()}] following (Ctrl-C to stop)", file=sys.stderr)
            await disconnected
            print(f"\n[{stamp()}] device disconnected", file=sys.stderr)
    except Exception as e:
        # bleak 3.x surfaces BlueZ's classic-page failure as
        # BleakDBusError("br-connection-page-timeout") when bluetoothd
        # answers before our timer — catch broadly so the scanner stops
        # and the diagnostic prints for the trap this tool exists to
        # explain (Codex P2 on #212), then re-raise.
        if scanner is not None:
            await scanner.stop()
        name = getattr(getattr(e, "error_name", None), "split", lambda _s: [])()
        is_page_timeout = (isinstance(e, (asyncio.TimeoutError, TimeoutError))
                           or "page-timeout" in str(name).lower()
                           or "page-timeout" in str(e).lower())
        if is_page_timeout:
            print(f"\n[{stamp()}] connect failed "
                  f"({type(e).__name__}). If btmon shows "
                  f"'Create Connection (0x0005)' + 'Page Timeout', bluetoothd is "
                  f"PAGING the peer over classic BR/EDR (the bluez 5.72-0ubuntu5.8 "
                  f"dual-address trap — see module docstring). Remedy host-side: "
                  f"pin bluez 5.72-0ubuntu5.7 or use a different central adapter.",
                  file=sys.stderr)
        raise


async def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--name", default=DEFAULT_NAME_PREFIX,
                    help=f"device-name prefix (default {DEFAULT_NAME_PREFIX})")
    ap.add_argument("--list", action="store_true", help="scan and exit")
    ap.add_argument("--send", metavar="LINE", help="write LINE to the NUS RX characteristic")
    args = ap.parse_args()

    import bleak  # deferred so --help works without bleak installed

    if args.list:
        await scan_and_pick(bleak, args.name, True)
        return
    device, scanner = await connect_fresh(bleak, args.name)
    if device is None:
        print(f"no BLE peripheral named {args.name}* found (is the -ble build "
              f"flashed and advertising?)", file=sys.stderr)
        sys.exit(1)
    await follow(device, scanner, args.send)


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
