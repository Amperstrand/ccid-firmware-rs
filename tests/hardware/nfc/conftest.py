"""Conftest for the NFC reader differential HIL suite.

Sequential same-field discipline: exactly ONE reader is connected at any
moment (the bench readers share the 13.56 MHz field — parallel polling
causes RF collisions and flaky detection). Every test requests readers
through the `one_reader_at_a_time` gate.
"""

import time

import pytest

from smartcard.System import readers
from smartcard.Exceptions import NoCardException, CardConnectionException

READER_MATCHES = {
    "nucula": "Nucula CCID",
    "m5stick": "GemPCTwin serial",
    "acr1252": "ACR1252 Dual Reader [ACR1252 Dual Reader PICC]",
}

# Read-only AID probes (SELECT only — no app commands that mutate state)
AID_PROBES = {
    "OpenPGP": "D27600012401",
    "Satochip": "5361746F43686970",
    "SeedKeeper": "5361746F43686970417070",
    "FIDO/U2F": "A0000006472F0001",
    "FIDO2/CTAP": "A0000006472F0002",
    "PIV": "A000000308000000000100",
    "ISO-DEP AID-less (PPSE)": "325041592E5359532E4444463031",
}


def pytest_addoption(parser):
    parser.addoption("--hil", action="store_true", default=False,
                     help="Run hardware-in-the-loop tests")
    parser.addoption("--readers", action="store", default="nucula,m5stick,acr1252",
                     help="Comma-separated subset: nucula,m5stick,acr1252")


def pytest_configure(config):
    config.addinivalue_line("markers", "hil: hardware-in-the-loop test")


def pytest_collection_modifyitems(config, items):
    if not config.getoption("--hil"):
        skip = pytest.mark.skip(reason="need --hil option to run")
        for item in items:
            if "hil" in item.keywords:
                item.add_marker(skip)


def find_reader(key: str):
    matches = [r for r in readers() if READER_MATCHES[key] in str(r)]
    return matches[0] if matches else None


def connect_reader(reader, attempts: int = 3, delay: float = 1.5):
    """Connect with retries; returns (conn, atr_bytes) or (None, None)."""
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


@pytest.fixture(params=["nucula", "m5stick", "acr1252"])
def reader_key(request):
    wanted = request.config.getoption("--readers").split(",")
    if request.param not in wanted:
        pytest.skip(f"reader not selected: {request.param}")
    return request.param


@pytest.fixture
def card_session(request, reader_key):
    """One card connection per test per reader — sequential by design."""
    reader = find_reader(reader_key)
    if reader is None:
        pytest.fail(f"reader not present in pcscd: {reader_key} "
                    f"(looked for '{READER_MATCHES[reader_key]}')")
    conn, atr = connect_reader(reader)
    if conn is None:
        pytest.skip(f"no card detected on {reader_key} (RF coupling?)")
    yield {"key": reader_key, "conn": conn, "atr": atr}
    try:
        conn.disconnect()
    except Exception:
        pass
    time.sleep(0.5)  # field settle before the next reader touches the card
