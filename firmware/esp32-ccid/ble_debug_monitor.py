#!/usr/bin/env python3
"""BLE Debug Monitor for ESP32-CCID reader firmware."""

import asyncio
import sys

from bleak import BleakClient, BleakScanner


DEVICE_NAME = "ESP32-CCID-Debug"
SERVICE_UUID = "8f4211d6-5b44-4e8c-9c7a-7f0f4e8d0001"
LOG_CHAR_UUID = "8f4211d6-5b44-4e8c-9c7a-7f0f4e8d0002"


def handle_notification(_: str, data: bytearray) -> None:
    sys.stdout.write(bytes(data).decode("utf-8", errors="replace"))
    sys.stdout.flush()


async def monitor() -> None:
    while True:
        device = await BleakScanner.find_device_by_name(DEVICE_NAME, timeout=10.0)
        if not device:
            print(f"{DEVICE_NAME} not found; retrying...", file=sys.stderr)
            await asyncio.sleep(2)
            continue

        try:
            async with BleakClient(device) as client:
                print(f"Connected to {device.name} ({device.address})", file=sys.stderr)
                await client.start_notify(LOG_CHAR_UUID, handle_notification)

                while client.is_connected:
                    await asyncio.sleep(1)
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            print(f"BLE monitor error: {exc}", file=sys.stderr)

        print("Disconnected; reconnecting...", file=sys.stderr)
        await asyncio.sleep(2)


def main() -> None:
    try:
        asyncio.run(monitor())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
