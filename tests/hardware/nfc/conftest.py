"""Conftest for the NFC reader differential HIL suite.

Sequential same-field discipline: exactly ONE reader is connected at any
moment (the bench readers share the 13.56 MHz field — parallel polling
causes RF collisions and flaky detection). Every test requests readers
through the `one_reader_at_a_time` gate.
"""

import time

import pytest

# Card helpers live in cardlib.py (uniquely named): bare `from conftest
# import ...` in test modules breaks when several suite directories are
# collected together — whichever conftest.py claims the module name first
# wins. pyscard stays lazily imported inside cardlib (CI collection gate
# runs in a minimal env).
from cardlib import (  # noqa: F401  (fixtures + tests consume these)
    AID_PROBES,
    READER_MATCHES,
    connect_reader,
    find_reader,
    select_aid,
)


def pytest_addoption(parser):    parser.addoption("--readers", action="store", default="nucula,m5stick,acr1252",
                     help="Comma-separated subset: nucula,m5stick,acr1252")



def pytest_collection_modifyitems(config, items):
    if not config.getoption("--hil"):
        skip = pytest.mark.skip(reason="need --hil option to run")
        for item in items:
            if "hil" in item.keywords:
                item.add_marker(skip)


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
