#!/usr/bin/env python3
"""The unmodified specter wallet against the virtual SecureApplet model.

The MemoryCard suites (test_specter_card_model*.py) drive the wallet's
MemoryCardApplet subclass. This module drives the BASE applet the
wallet calls SecureApplet (secureapplet.py) — the full PIN surface
the MemoryCard tests never touch — through host-tools/
specter_secure_applet.py with the same CPython shim technique:

- positive: SELECT, channel open in both modes (the wallet VERIFIES
  our HMAC + ECDSA signature), ping, secure random, the whole PIN
  lifecycle (set -> wrong-pin 0502 with attempt burn -> unlock ->
  lock -> change -> unlock with the new PIN);
- adversarial: the 3rd wrong PIN bricks (0503 — the code the
  keystore maps to CriticalErrorWipeImmediately), no recovery with
  the correct PIN, tampered channel bytes rejected by the wallet's
  own verifiers, attempt budget surviving a reconnect;
- the real uscard stub + specter_sim_relay path (hardcoded port
  21111, owned and released in finally).
"""
import os
import random
import socket
import sys
import threading
import time
import types
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "host-tools"))

pytest.importorskip("Crypto")
pytest.importorskip("coincurve")
import specter_secure_applet as M  # noqa: E402

SPECTER_DIR = Path("/root/src/specter-diy")


def _tree_ok() -> bool:
    try:
        return SPECTER_DIR.is_dir()
    except OSError:
        return False


def _load_wallet():
    """The real wallet SecureApplet class under CPython (stdlib shims)."""
    import importlib.util
    for name, mk in [
        ("micropython", lambda m: setattr(m, "const", lambda x: x)),
        ("rng", lambda m: setattr(
            m, "get_random_bytes", lambda n: os.urandom(n))),
        ("ucryptolib", None),
        ("secp256k1", None),
    ]:
        if name in sys.modules:
            continue
        mod = types.ModuleType(name)
        if mk:
            mk(mod)
        sys.modules[name] = mod
    uc = sys.modules["ucryptolib"]

    class _AES:
        def __init__(self, key, mode, iv=None):
            from Crypto.Cipher import AES as _A
            self._c = _A.new(key, _A.MODE_ECB if mode == 0
                             else _A.MODE_CBC, iv=iv)

        def encrypt(self, d):
            return self._c.encrypt(d)

        def decrypt(self, d):
            return self._c.decrypt(d)
    uc.aes = _AES
    uc.MODE_ECB = 0
    uc.MODE_CBC = 1

    import coincurve as _cc

    class _P:
        """Mutable point holder: micropython-secp256k1 MUTATES pubkey
        objects in tweak_mul (statement form, return discarded) while
        coincurve returns a new object — without the holder the wallet
        derives the shared secret from the UNTWEAKED point."""

        def __init__(self, pub):
            self._pub = pub

        def format(self, compressed=False):
            return self._pub.format(compressed=compressed)

        def verify(self, der, digest, hasher=None):
            return self._pub.verify(der, digest, hasher=hasher)
    cc = sys.modules["secp256k1"]
    cc.EC_UNCOMPRESSED = 0
    cc.EC_COMPRESSED = 1
    cc.ec_pubkey_parse = lambda b: _P(_cc.PublicKey(bytes(b)))
    cc.ec_pubkey_create = lambda s: _P(_cc.PrivateKey(bytes(s)).public_key)
    cc.ec_pubkey_serialize = lambda p, fmt=0: p.format(
        compressed=(fmt == 1))

    def _tweak(pub, sec):
        pub._pub = pub._pub.multiply(bytes(sec))
    cc.ec_pubkey_tweak_mul = _tweak

    class _Sig:
        def __init__(self, der):
            self.der = bytes(der)
    cc.ecdsa_signature_parse_der = _Sig
    cc.ecdsa_signature_normalize = lambda s: s
    # coincurve's PublicKey.verify takes the DER form directly
    cc.ecdsa_verify = lambda sig, dg, pub: pub.verify(
        sig.der, dg, hasher=None)

    sys.path.insert(0, str(SPECTER_DIR / "f469-disco" / "libs" / "unix"))
    jc = SPECTER_DIR / "src" / "keystore" / "javacard"
    pkg = types.ModuleType("jk")
    pkg.__path__ = [str(jc)]
    sys.modules["jk"] = pkg
    ap = types.ModuleType("jk.applets")
    ap.__path__ = [str(jc / "applets")]
    sys.modules["jk.applets"] = ap

    def load(name, path):
        spec = importlib.util.spec_from_file_location(name, path)
        mod = importlib.util.module_from_spec(spec)
        sys.modules[name] = mod
        spec.loader.exec_module(mod)
        return mod
    load("jk.util", str(jc / "util.py"))
    load("jk.applets.applet", str(jc / "applets" / "applet.py"))
    return load("jk.applets.secureapplet",
                str(jc / "applets" / "secureapplet.py"))


class DirectConn:
    """Wallet connection straight to the model (no sockets)."""

    T1_protocol = 1

    def __init__(self, model):
        self.m = model
        self.ok = True

    def isCardInserted(self):
        return self.ok

    def connect(self, p):
        pass

    def disconnect(self):
        pass

    def getATR(self):
        return b"atr"

    def transmit(self, data):
        return self.m.exchange(bytes(data))


class TamperConn(DirectConn):
    """Mutates model responses whose C-APDU header matches `match`."""

    def __init__(self, model, match, mutator):
        super().__init__(model)
        self.match = bytes(match)
        self.mutator = mutator

    def transmit(self, data):
        r = super().transmit(data)
        if bytes(data[:len(self.match)]) == self.match:
            r = self.mutator(r)
        return r


def _flip(data, pos):
    d = bytearray(data)
    d[pos % len(d)] ^= 0x01
    return bytes(d)


def _wallet_on(conn):
    sa = _load_wallet()
    return sa.SecureApplet(conn, M.AID)


@pytest.fixture()
def wallet():
    if not _tree_ok():
        pytest.skip("specter-diy tree absent (bench host only)")
    w = _wallet_on(DirectConn(M.SecureAppletModel()))
    w.select()
    return w


# --- positive path ----------------------------------------------------------

def test_wallet_pin_lifecycle_positive(wallet):
    """Full applet surface: channel handshake (wallet-verified), echo,
    random, PIN set/unlock/lock/change — the exact op set of the
    wallet's SecureApplet class."""
    sa = sys.modules["jk.applets.secureapplet"]
    pub = wallet.sc.get_card_pubkey()
    assert pub is not None
    wallet.sc.open("es")            # wallet VERIFIES our HMAC + ECDSA sig
    assert wallet.sc.is_open
    wallet.ping()                   # asserts echo == b"ping" internally
    assert len(wallet.get_random()) == 32
    ra, rb = wallet.get_random(), wallet.get_random()
    assert ra != rb  # fresh randomness per call (2^-256 collision)
    # fresh card: no PIN
    assert wallet.get_pin_status() == (3, 3, sa.SecureApplet.PIN_UNSET)
    assert not wallet.is_pin_set and not wallet.is_locked
    # set -> LOCKED (OwnerPIN: update never validates), full budget
    wallet.set_pin("1234-0000")
    assert wallet.is_pin_set and wallet.is_locked
    assert wallet.pin_attempts_left == 3
    # wrong PIN: exact code the keystore maps to PinError, budget burns
    with pytest.raises(sa.SecureError) as ei:
        wallet.unlock("not-the-pin")
    assert str(ei.value) == "0502"
    assert wallet.is_locked and wallet.pin_attempts_left == 2
    # right PIN: unlocked, budget restored
    wallet.unlock("1234-0000")
    assert not wallet.is_locked and wallet.pin_attempts_left == 3
    wallet.lock()
    assert wallet.is_locked
    wallet.unlock("1234-0000")
    # change while unlocked, then only the NEW pin works
    wallet.change_pin("1234-0000", "4321-9999")
    assert not wallet.is_locked
    wallet.lock()
    wallet.unlock("4321-9999")
    assert not wallet.is_locked
    wallet.lock()
    with pytest.raises(sa.SecureError) as ei:
        wallet.unlock("1234-0000")  # the old PIN is dead
    assert str(ei.value) == "0502"
    # the base applet has NO secret ops (those are MemoryCard's 05xx)
    with pytest.raises(sa.SecureError) as ei:
        wallet.sc.request(b"\x05\x00")
    assert str(ei.value) == "6d00"
    wallet.sc.close()
    assert not wallet.sc.is_open


def test_wallet_ee_mode_channel(wallet):
    """The SecureApplet rides the same channel machinery — ee mode
    (card-ephemeral) handshake + ops."""
    wallet.sc.open("ee")
    assert wallet.sc.is_open
    wallet.ping()
    assert wallet.get_pin_status() == (3, 3, 0)
    wallet.set_pin("ee-pin")
    wallet.unlock("ee-pin")
    assert not wallet.is_locked
    wallet.sc.close()


def test_wallet_channel_soak_both_modes(wallet):
    for i in range(30):
        mode = "es" if i % 2 == 0 else "ee"
        wallet.sc.open(mode)
        wallet.ping()
        assert wallet.get_pin_status() == (3, 3, 0)
        wallet.sc.close()
    # state-leak check: a full PIN flow still works after the soak
    wallet.set_pin("soak-pin")
    wallet.unlock("soak-pin")
    wallet.lock()
    assert wallet.is_locked


# --- adversarial legs -------------------------------------------------------

def test_wallet_bricked_after_three_wrong_pins(wallet):
    """The 3rd wrong PIN answers 0503 — the exact code the keystore
    maps to CriticalErrorWipeImmediately — and there is NO recovery:
    even the correct PIN is refused afterwards."""
    sa = sys.modules["jk.applets.secureapplet"]
    wallet.set_pin("super-pin")
    for left in (2, 1):
        with pytest.raises(sa.SecureError) as ei:
            wallet.unlock("wrong-%d" % left)
        assert str(ei.value) == "0502"
        assert wallet.pin_attempts_left == left
    with pytest.raises(sa.SecureError) as ei:
        wallet.unlock("wrong-0")            # exhausts the budget
    assert str(ei.value) == "0503"
    assert wallet.is_locked                 # BRICKED counts as locked
    assert wallet.get_pin_status() == (0, 3, sa.SecureApplet.PIN_BRICKED)
    with pytest.raises(sa.SecureError) as ei:
        wallet.unlock("super-pin")          # correct PIN: still refused
    assert str(ei.value) == "0503"
    assert wallet.get_pin_status() == (0, 3, sa.SecureApplet.PIN_BRICKED)


def test_wallet_change_pin_wrong_old_pin_burns_attempt(wallet):
    sa = sys.modules["jk.applets.secureapplet"]
    wallet.set_pin("old-pin")
    wallet.unlock("old-pin")
    with pytest.raises(sa.SecureError) as ei:
        wallet.change_pin("bad-old", "new-pin")
    assert str(ei.value) == "0502"
    assert wallet.get_pin_status() == (2, 3, sa.SecureApplet.PIN_UNLOCKED)
    # the right old PIN still works and restores the budget
    wallet.change_pin("old-pin", "new-pin")
    assert wallet.get_pin_status() == (3, 3, sa.SecureApplet.PIN_UNLOCKED)
    wallet.lock()
    wallet.unlock("new-pin")
    assert not wallet.is_locked


def test_wallet_client_guards_track_model_state(wallet):
    """The wallet's own client-side guards fire from OUR status
    replies — set twice, change when unset, change when locked."""
    sa = sys.modules["jk.applets.secureapplet"]
    with pytest.raises(sa.AppletException, match="PIN is not set"):
        wallet.change_pin("a", "b")
    wallet.set_pin("first-pin")
    with pytest.raises(sa.AppletException, match="Unlock the card first"):
        wallet.change_pin("first-pin", "c")  # LOCKED after set_pin
    with pytest.raises(sa.AppletException, match="PIN is already set"):
        wallet.set_pin("second-pin")


def test_attempt_budget_survives_reconnect(wallet):
    """Power-up clears validation but NOT the attempt budget (real
    OwnerPIN semantics — brick risk survives reinsertion). The wallet
    still believes its channel is open, so the stale session fails on
    the wire (ISOException 6982 — security status not satisfied);
    the wallet's own reconnect sequence (close, select, reopen —
    what keystore.check_card does after reinsertion) recovers."""
    sa = sys.modules["jk.applets.secureapplet"]
    wallet.set_pin("reconnect-pin")
    with pytest.raises(sa.SecureError):
        wallet.unlock("wrong-once")
    assert wallet.pin_attempts_left == 2
    # card out and back in: relay/session boundary does this
    wallet.conn.m.disconnect()
    wallet.conn.m.connect()
    with pytest.raises(sa.ISOException, match="6982"):
        wallet.get_pin_status()          # stale host-side channel
    wallet.sc.close()                    # wallet's reconnect sequence
    wallet.select()
    wallet.sc.open("es")
    assert wallet.get_pin_status() == (2, 3, sa.SecureApplet.PIN_LOCKED)
    wallet.unlock("reconnect-pin")
    assert not wallet.is_locked
    assert wallet.pin_attempts_left == 3


def test_tampered_channel_rejected_by_wallet():
    """Tampered MAC / signature / ciphertext must be rejected by the
    wallet's OWN verifiers (a model that accepted them would model
    the wrong card)."""
    if not _tree_ok():
        pytest.skip("specter-diy tree absent (bench host only)")
    # OPEN_SE reply: nonce(32) + mac(14) — flip inside the mac region
    w = _wallet_on(TamperConn(
        M.SecureAppletModel(), b"\xB0\xB4", lambda r: _flip(r, 40)))
    w.select()
    with pytest.raises(Exception, match="HMAC"):
        w.sc.open("es")
    # OPEN_SE reply: flip inside the DER signature tail
    w = _wallet_on(TamperConn(
        M.SecureAppletModel(), b"\xB0\xB4", lambda r: _flip(r, -3)))
    w.select()
    with pytest.raises(Exception, match="[Ss]ignature"):
        w.sc.open("es")
    # SECURE_MSG reply: flip ciphertext — decrypt/HMAC must fail
    w = _wallet_on(TamperConn(
        M.SecureAppletModel(), b"\xB0\xB6", lambda r: _flip(r, 4)))
    w.select()
    w.sc.open("es")
    with pytest.raises(Exception):
        w.sc.request(b"\x00\x00x")


def test_fuzz_random_apdus_never_crash():
    m = M.SecureAppletModel()
    m.connect()
    m.exchange(b"\x00\xA4\x04\x00" + bytes([len(M.AID)]) + M.AID)
    rng = random.Random(20261116)
    for _ in range(500):
        n = rng.randrange(4, 80)
        apdu = bytes(rng.randrange(256) for _ in range(n))
        try:
            resp = m.exchange(apdu)
        except Exception as e:  # noqa: BLE001 — the model must never raise
            pytest.fail(f"model raised on fuzz APDU {apdu.hex()}: {e!r}")
        assert len(resp) >= 2, "response must carry a status word"
    if not _tree_ok():
        pytest.skip("specter-diy tree absent (bench host only)")
    # and the model still serves a real wallet session afterwards
    w = _wallet_on(DirectConn(m))
    w.select()
    w.sc.open("es")
    w.ping()
    w.set_pin("after-fuzz")
    w.unlock("after-fuzz")
    assert not w.is_locked


# --- model wire shapes + the real relay path -------------------------------

def test_model_raw_wire_shapes():
    m = M.SecureAppletModel()
    m.connect()
    # this is NOT the MemoryCard applet — its AID must not select
    assert m.exchange(bytes.fromhex("00A4040006B00B5111CB01")) == \
        bytes.fromhex("6a82")
    assert m.exchange(b"\x00\xA4\x04\x00" + bytes([len(M.AID)]) + M.AID) \
        == bytes.fromhex("9000")
    r = m.exchange(bytes.fromhex("B0B20000"))
    assert len(r) == 65 + 2 and r[-2:] == bytes.fromhex("9000")
    # SECURE_MSG before channel open -> security status not satisfied
    assert m.exchange(bytes.fromhex("B0B6000000")) == bytes.fromhex("6982")
    assert m.exchange(bytes.fromhex("B0990000")) == bytes.fromhex("6d00")
    assert m.exchange(bytes.fromhex("B0B70000")) == bytes.fromhex("9000")
    assert m.exchange(b"\xB0") == bytes.fromhex("6d00")  # truncated
    st = m.status()
    assert st["pin_status"] == M.PIN_UNSET and st["pin_attempts_left"] == 3


def test_relay_serves_secure_applet_over_uscard_stub():
    """The model as a CardBackend behind specter_sim_relay, through
    the wallet's REAL uscard stub (hardcoded port 21111 — owned here
    and released in finally)."""
    if not _tree_ok():
        pytest.skip("specter-diy tree absent (bench host only)")
    import specter_sim_relay
    sa = _load_wallet()
    relay = specter_sim_relay.SimRelay(M.SecureAppletModel())
    t = threading.Thread(target=relay.serve_forever, args=(21111,),
                         daemon=True)
    t.start()
    try:
        for _ in range(50):
            try:
                socket.create_connection(
                    ("127.0.0.1", 21111), timeout=1).close()
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise RuntimeError("relay did not come up")
        from uscard import Reader
        conn = Reader().createConnection()
        assert conn.isCardInserted()
        conn.connect(1)
        card = sa.SecureApplet(conn, M.AID)
        card.select()                     # SELECT -> 9000
        card.sc.get_card_pubkey()
        card.sc.open("es")                # wallet verifies HMAC + sig
        card.ping()
        assert card.get_pin_status() == (3, 3, 0)
        card.set_pin("relay-pin")
        assert card.is_locked
        card.unlock("relay-pin")
        assert not card.is_locked
        card.sc.close()
        assert not card.sc.is_open
    finally:
        relay.stop()
