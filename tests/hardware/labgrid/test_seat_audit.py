#!/usr/bin/env python3
"""Offline unit tests for seat_audit parsers (no bench, no coordinator)."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from tests.hardware.labgrid.seat_audit import (  # noqa: E402
    parse_who, parse_seats_recent, parse_exporter_identities,
    usb_unclaimed, resource_unregistered,
)

WHO = """User    Host             Place              Changed
root    ai-legion        ccid-nucula        2026-11-04 13:27:10.505214
ubuntu  ai-legion-small  f469-specter-film  2026-11-14 13:55:09.224669
"""

SEATS = """# SEATS
## 2026-11-13 — old section
mentions ccid-nucula once.
## 2026-11-14 ~17:0x — F469 queue proposal
f469-specter-film held; ccid queues behind.
"""

RESOURCES = """
Acquired resource 'SerialPort' (ai-legion-small-nfc/f469-specter-vcp/SerialPort/SerialPort):
  {'params': {'port': '/dev/serial/by-id/usb-STMicroelectronics_STM32_STLink_066FFF505567494867224125-if02', 'speed': 115200}}
Acquired resource 'USBDevice' (ai-legion-small-nfc/s20-camera/USBDevice/USBDevice):
  {'params': {'extra': {'match': 'ID_VENDOR_ID=04e8 ID_MODEL_ID=6860 '
                                 'ID_SERIAL_SHORT=RF8NB01ZYAH'}}}
info: STLink VID:PID 0483:374b
"""


class Parsers(unittest.TestCase):
    def test_parse_who(self):
        holds = parse_who(WHO)
        self.assertEqual(len(holds), 2)
        self.assertEqual(holds[1][2], "f469-specter-film")

    def test_parse_seats_recent_lookback(self):
        # default read path needs the file; exercise the section logic via
        # a direct call on a temp file
        import tempfile, os
        with tempfile.NamedTemporaryFile("w", suffix=".md",
                                         delete=False) as f:
            f.write(SEATS)
            path = f.name
        try:
            text = parse_seats_recent(path, sections=1)
            self.assertIn("queue proposal", text)
            self.assertNotIn("old section", text)
        finally:
            os.unlink(path)

    def test_exporter_identities(self):
        idents = parse_exporter_identities(RESOURCES)
        self.assertIn("0483:374b", idents)
        self.assertIn("RF8NB01ZYAH", idents)
        self.assertIn("066FFF505567494867224125", idents)

    def test_usb_unclaimed_flags_the_pyboard_class(self):
        devices = [
            ("1d6b:0002", "", "xHCI Host Controller"),
            ("17ef:c955", "", "ITE Device(8297)"),
            ("f055:9802", "206635575231", "Pyboard Virtual Comm Port"),
            ("0483:374b", "066FFF505567494867224125", "STM32 STLink"),
        ]
        idents = parse_exporter_identities(RESOURCES)
        flags = usb_unclaimed(devices, idents)
        self.assertEqual(len(flags), 1)
        self.assertIn("f055:9802", flags[0])

    def test_resource_unregistered(self):
        res = "ID_SERIAL_SHORT=RF8NB01ZYAH ID_SERIAL_SHORT=ZZZUNKNOWN01"
        self.assertEqual(resource_unregistered(res, "RF8NB01ZYAH"),
                         ["ZZZUNKNOWN01"])
        self.assertIsNone(resource_unregistered(res, None))


if __name__ == "__main__":
    unittest.main()
