"""NFC suite card helpers (uniquely-named module — NOT conftest).

Test modules import from here, never `from conftest import ...`: a bare
`conftest` module name is claimed by whichever suite's conftest.py is
imported first when multiple suite directories are collected together
(e.g. `pytest tests/hardware/labgrid/ tests/hardware/nfc/`), which breaks
collection with ImportErrors. Same pattern as labgrid/bench.py.

pyscard is imported INSIDE the helpers, not at module scope: this module
loads during pytest collection, and the CI collection gate runs in a
minimal env (pytest + stdlib only) — the documented no-hardware
`pytest tests/hardware/ -v` path must stay collectable there.
"""

import time


READER_MATCHES = {
    "nucula": "Nucula CCID",
    "m5stick": "GemPCTwin serial",
    "acr1252": "ACR1252 Dual Reader [ACR1252 Dual Reader PICC]",
}

# Read-only AID probes (SELECT only — no app commands that mutate state)
AID_PROBES = {
    # GlobalPlatform 2.1.1 ISD RID prefix — the P71 JCOP twins on the
    # bench answer this with 9000 (bench 2026-10-12: m5stick + nucula
    # both; the ISO A000000003000000 prefix is 6A82 on these cards).
    "GP ISD": "A000000151000000",
    "OpenPGP": "D27600012401",
    "Satochip": "5361746F43686970",
    "SeedKeeper": "5361746F43686970417070",
    "FIDO/U2F": "A0000006472F0001",
    "FIDO2/CTAP": "A0000006472F0002",
    "PIV": "A000000308000000000100",
    "ISO-DEP AID-less (PPSE)": "325041592E5359532E4444463031",
}


def find_reader(key: str):
    from smartcard.System import readers

    matches = [r for r in readers() if READER_MATCHES[key] in str(r)]
    return matches[0] if matches else None


def connect_reader(reader, attempts: int = 3, delay: float = 1.5):
    """Connect with retries; returns (conn, atr_bytes) or (None, None)."""
    from smartcard.Exceptions import CardConnectionException, NoCardException

    for _ in range(attempts):
        try:
            conn = reader.createConnection()
            conn.connect()
            atr = bytes(conn.getATR())
            return conn, atr
        except NoCardException:
            time.sleep(delay)
        except CardConnectionException:
            # readers that report 'unpowered' on first try usually power
            # up on the second connect attempt
            time.sleep(delay)
    return None, None


def select_aid(conn, aid_hex: str):
    aid = bytes.fromhex(aid_hex)
    apdu = bytes([0x00, 0xA4, 0x04, 0x00, len(aid)]) + aid
    data, sw1, sw2 = conn.transmit(list(apdu))
    return bytes(data), (sw1 << 8) | sw2
