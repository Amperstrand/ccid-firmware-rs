"""Wire-protocol conformance for the vpicc_table mock — no pcscd needed.

The vpcd protocol table (frankmorgner.github.io/vsmartcard, "Creating a
Virtual Smart Card") answers only Get ATR (0x04) and APDUs; OFF/ON/RESET
are one-way announcements. Regression guard for the SW=1084 desync class
(bolty-rs #226): answering RESET puts a stale ATR in the driver's
uncorrelated TCP stream; a 0-length reply instead signals "ICC absent"
and ejects the slot.
"""

import vpicc_table
from vpicc_table import DEFAULT_ATR, TableVpicc


class FakeSock:
    def __init__(self):
        self.sent = []

    def sendall(self, data):
        self.sent.append(bytes(data))


def _frame(payload: bytes) -> bytes:
    return len(payload).to_bytes(2, "big") + payload


CTRL_OFF, CTRL_ON, CTRL_RESET, CTRL_ATR = b"\x00", b"\x01", b"\x02", b"\x04"


def test_get_atr_is_answered_with_the_atr():
    card = TableVpicc("127.0.0.1", 1)
    sock = FakeSock()
    card._handle(sock, CTRL_ATR)
    assert sock.sent == [_frame(bytes.fromhex(DEFAULT_ATR))]
    assert card.answered_probe.is_set()


def test_reset_is_true_silence():
    card = TableVpicc("127.0.0.1", 1)
    sock = FakeSock()
    card._handle(sock, CTRL_RESET)
    assert sock.sent == [], "RESET must get NO bytes — not even a 0-length frame"


def test_power_on_and_off_are_silent():
    card = TableVpicc("127.0.0.1", 1)
    for ctrl in (CTRL_ON, CTRL_OFF):
        sock = FakeSock()
        card._handle(sock, ctrl)
        assert sock.sent == []


def test_apdus_still_answer_from_the_table():
    card = TableVpicc("127.0.0.1", 1)
    sock = FakeSock()
    capdu = bytes.fromhex("00A4040007") + vpicc_table.ISD_AID
    card._handle(sock, capdu)
    assert len(sock.sent) == 1
    assert vpicc_table.MARKER in sock.sent[0][2:], "self-marking doctrine (#261)"


def test_reset_then_atr_sequence_stays_aligned():
    """pcscd's real flow: RESET followed by Get ATR (ifd-vpcd.c)."""
    card = TableVpicc("127.0.0.1", 1)
    sock = FakeSock()
    card._handle(sock, CTRL_RESET)
    card._handle(sock, CTRL_ATR)
    assert sock.sent == [_frame(bytes.fromhex(DEFAULT_ATR))], (
        "exactly one ATR for exactly one Get ATR — the desync invariant"
    )
