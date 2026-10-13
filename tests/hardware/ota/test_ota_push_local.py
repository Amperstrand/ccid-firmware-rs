"""Local unit tests for the push client against a fake board socket.

No hardware: a thread runs a minimal in-process implementation of the
firmware/esp32-ccid/src/ota.rs server side (header read -> 'E' on
rejection, silent close on accept) on localhost, and push_ex() is
exercised against it. Part of the suite self-check.
"""

import socket
import threading

import pytest

from .ota_push import MAGIC, push_ex

MARK = pytest.mark.ota_unit


class FakeBoard(threading.Thread):
    """Speaks just enough of the ota.rs wire protocol for the client
    test: reads the 8-byte header, drains the announced payload, then
    either replies 'E' (validation rejected) or closes silently
    (accepted -> board reboots). Records what it saw."""

    def __init__(self, reject):
        super().__init__(daemon=True)
        self.reject = reject
        self.seen_magic = None
        self.seen_size = None
        self.seen_payload = b""
        self.sock = socket.socket()
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(1)
        self.port = self.sock.getsockname()[1]

    def run(self):
        try:
            conn, _ = self.sock.accept()
            with conn:
                conn.settimeout(10)
                hdr = b""
                while len(hdr) < 8:
                    chunk = conn.recv(8 - len(hdr))
                    if not chunk:
                        return
                    hdr += chunk
                self.seen_magic = hdr[:4]
                self.seen_size = int.from_bytes(hdr[4:], "little")
                remaining = self.seen_size
                while remaining > 0:
                    chunk = conn.recv(min(4096, remaining))
                    if not chunk:
                        return
                    self.seen_payload += chunk
                    remaining -= len(chunk)
                if self.reject:
                    conn.sendall(b"E")
                # accept path: ota.rs reboots without any ack byte -
                # just close.
        except OSError:
            pass
        finally:
            self.sock.close()


@MARK
def test_push_ex_rejected_path():
    fb = FakeBoard(reject=True)
    fb.start()
    out = push_ex("127.0.0.1", b"\xe9payload" * 64, port=fb.port)
    fb.join(timeout=5)
    assert out["connected"] is True
    assert out["rejected"] is True
    assert out["accepted"] is False
    assert out["error"] is None
    assert out["sent"] == out["total"] == 64 * 8
    # the fake board saw the exact protocol framing
    assert fb.seen_magic == MAGIC == b"OTA1"
    assert fb.seen_size == 64 * 8
    assert fb.seen_payload == b"\xe9payload" * 64


@MARK
def test_push_ex_accepted_path():
    fb = FakeBoard(reject=False)
    fb.start()
    payload = bytes(range(256)) * 10
    out = push_ex("127.0.0.1", payload, port=fb.port)
    fb.join(timeout=5)
    assert out["connected"] is True
    assert out["accepted"] is True
    assert out["rejected"] is False
    assert out["error"] is None
    assert fb.seen_payload == payload


@MARK
def test_push_ex_connection_refused():
    # bind then close -> guaranteed-free port
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    out = push_ex("127.0.0.1", b"\xe9img", port=port, connect_timeout=2)
    assert out["connected"] is False
    assert out["error"] and "connect failed" in out["error"]


@MARK
def test_push_ex_refuses_empty_and_oversized():
    out = push_ex("127.0.0.1", b"")
    assert out["error"] == "empty image"
    out = push_ex("127.0.0.1", b"\xe9" * (4 * 1024 * 1024 + 1))
    assert "exceeds server limit" in out["error"]
