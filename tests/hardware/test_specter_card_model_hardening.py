#!/usr/bin/env python3
"""Adversarial + soak + fuzz legs for the virtual MemoryCard model.

The e2e positive path (test_specter_card_model.py) proves the model
against the UNMODIFIED wallet's own verifiers. This module pushes the
surfaces the happy path never touches:

- ee mode (OPEN_EE B0B5): card-ephemeral channel, hashed-x derivation
- tampering: every MAC / signature / ciphertext the model emits must be
  REJECTED by the wallet's verifiers (a model that accepted tampered
  traffic would be modeling the wrong card)
- channel lifecycle: use-after-close, IV advance on the wire
- soak: many open/close cycles in both modes, state-leak detection
- fuzz: random APDUs never crash the model, never raise, and never
  leave it unable to serve a fresh channel
"""
import os
import random
import sys
import types
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "host-tools"))

pytest.importorskip("Crypto")
pytest.importorskip("coincurve")
import specter_card_model as M  # noqa: E402

SPECTER_DIR = Path("/root/src/specter-diy")


def _tree_ok() -> bool:
    try:
        return SPECTER_DIR.is_dir()
    except OSError:
        return False


def _load_wallet():
    """Wallet classes with CPython shims (mirrors the e2e module)."""
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
    return load("jk.applets.memorycard",
                str(jc / "applets" / "memorycard.py"))


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
    """Mutates model responses whose C-APDU header matches `match`
    (installed from construction — the wallet's applet keeps its own
    conn reference, so swapping conn later would tamper nothing)."""

    def __init__(self, model, match, mutator):
        super().__init__(model)
        self.match = bytes(match)
        self.mutator = mutator

    def transmit(self, data):
        r = super().transmit(data)
        if bytes(data[:len(self.match)]) == self.match:
            r = self.mutator(r)
        return r


class CaptureConn(DirectConn):
    def __init__(self, model):
        super().__init__(model)
        self.pairs = []

    def transmit(self, data):
        r = super().transmit(data)
        self.pairs.append((bytes(data), bytes(r)))
        return r


def _flip(data, pos):
    d = bytearray(data)
    d[pos % len(d)] ^= 0x01
    return bytes(d)


def _wallet_on(conn):
    mc = _load_wallet()
    return mc.MemoryCardApplet(conn)


@pytest.fixture()
def wallet():
    if not _tree_ok():
        pytest.skip("specter-diy tree absent (bench host only)")
    w = _wallet_on(DirectConn(M.MemoryCardModel()))
    w.select()
    return w


# --- ee mode ---------------------------------------------------------------

def test_ee_mode_channel_and_ops(wallet):
    wallet.sc.open("ee")
    assert wallet.sc.is_open
    assert wallet.sc.request(b"\x00\x00ee") == b"ee"
    assert len(wallet.sc.request(b"\x01\x00")) == 32
    wallet.save_secret(b"ee-secret")
    assert wallet.get_secret() == b"ee-secret"
    wallet.sc.close()
    assert not wallet.sc.is_open


def test_es_and_ee_keys_differ(wallet):
    wallet.sc.open("es")
    k1 = wallet.sc.card_mac_key
    wallet.sc.close()
    wallet.sc.open("ee")
    assert wallet.sc.card_mac_key != k1
    wallet.sc.close()


# --- tampering: the wallet's verifiers must reject --------------------------

OPEN_SE = b"\xB0\xB4"
OPEN_EE = b"\xB0\xB5"
SECURE_MSG = b"\xB0\xB6"


def test_tampered_open_se_mac_rejected():
    if not _tree_ok():
        pytest.skip("specter-diy tree absent (bench host only)")
    # nonce(32) + mac(14): flip inside the mac region
    w = _wallet_on(TamperConn(
        M.MemoryCardModel(), OPEN_SE, lambda r: _flip(r, 40)))
    w.select()
    with pytest.raises(Exception, match="HMAC"):
        w.sc.open("es")


def test_tampered_open_se_signature_rejected():
    if not _tree_ok():
        pytest.skip("specter-diy tree absent (bench host only)")
    w = _wallet_on(TamperConn(
        M.MemoryCardModel(), OPEN_SE, lambda r: _flip(r, -3)))
    w.select()
    with pytest.raises(Exception, match="[Ss]ignature"):
        w.sc.open("es")


def test_tampered_open_ee_mac_rejected():
    if not _tree_ok():
        pytest.skip("specter-diy tree absent (bench host only)")
    # pub65 + mac(14): flip inside the mac region
    w = _wallet_on(TamperConn(
        M.MemoryCardModel(), OPEN_EE, lambda r: _flip(r, 70)))
    w.select()
    with pytest.raises(Exception, match="HMAC"):
        w.sc.open("ee")


def test_tampered_secure_msg_ciphertext_rejected():
    if not _tree_ok():
        pytest.skip("specter-diy tree absent (bench host only)")
    w = _wallet_on(TamperConn(
        M.MemoryCardModel(), SECURE_MSG, lambda r: _flip(r, 4)))
    w.select()
    w.sc.open("es")
    assert w.sc.is_open
    with pytest.raises(Exception):
        w.sc.request(b"\x00\x00x")


def test_use_after_close_auto_reopens(wallet):
    """The wallet's request() AUTO-REOPENS a closed channel (its
    documented "reestablish" path) — the model must survive the fresh
    OPEN_SE mid-conversation and keep answering."""
    wallet.sc.open("es")
    wallet.sc.close()
    assert not wallet.sc.is_open
    # transparently reopens; pubkey refetch included (close clears it)
    assert wallet.sc.request(b"\x00\x00late") == b"late"
    assert wallet.sc.is_open
    wallet.sc.close()


def test_iv_overflow_rekeys_channel(wallet):
    """iv >= 2**16 also triggers the wallet's reestablish branch — the
    model sees a fresh OPEN_SE after 65535 exchanges without any
    close."""
    wallet.sc.open("es")
    wallet.sc.iv = 2 ** 16  # force the counter-max branch
    assert wallet.sc.request(b"\x00\x00after-65536") == b"after-65536"
    wallet.sc.close()


def test_wire_iv_advances_between_requests():
    if not _tree_ok():
        pytest.skip("specter-diy tree absent (bench host only)")
    conn = CaptureConn(M.MemoryCardModel())
    w = _wallet_on(conn)
    w.select()
    w.sc.open("es")
    w.sc.request(b"\x00\x00a")
    w.sc.request(b"\x00\x00a")
    w.sc.close()
    msgs = [r for q, r in conn.pairs if q[:2] == SECURE_MSG]
    assert len(msgs) == 2
    # identical plaintext ops must yield different wire ciphertexts
    # (fresh IV per exchange)
    assert msgs[0][:-2] != msgs[1][:-2]


# --- soak -------------------------------------------------------------------

def test_open_close_soak_both_modes(wallet):
    for i in range(100):
        mode = "es" if i % 2 == 0 else "ee"
        wallet.sc.open(mode)
        assert wallet.sc.is_open
        assert wallet.sc.request(b"\x00\x00n%d" % i) == b"n%d" % i
        wallet.save_secret(b"s%d" % i)
        assert wallet.get_secret() == b"s%d" % i
        wallet.sc.close()
    # state leak check: a fresh channel still opens and serves
    wallet.sc.open("es")
    assert wallet.sc.request(b"\x00\x00final") == b"final"
    wallet.sc.close()


# --- fuzz -------------------------------------------------------------------

def test_fuzz_random_apdus_never_crash():
    m = M.MemoryCardModel()
    m.connect()
    m.exchange(bytes.fromhex("00A4040006B00B5111CB01"))
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
    # and the model still serves a real session afterwards
    w = _wallet_on(DirectConn(m))
    w.select()
    w.sc.open("es")
    assert w.sc.request(b"\x00\x00after-fuzz") == b"after-fuzz"
    w.sc.close()


def test_fuzz_during_channel_keeps_session():
    if not _tree_ok():
        pytest.skip("specter-diy tree absent (bench host only)")
    m = M.MemoryCardModel()
    m.connect()
    m.exchange(bytes.fromhex("00A4040006B00B5111CB01"))
    w = _wallet_on(DirectConn(m))
    w.sc.open("es")
    rng = random.Random(0xC0FFEE)
    for _ in range(200):
        # skip channel-management INS (a random B0B4/B0B5 would
        # legitimately rekey the card — that's a protocol event, not
        # corruption; the plain-fuzz test covers those bytes)
        ins = rng.randrange(256)
        while ins in (0xB4, 0xB5, 0xB7):
            ins = rng.randrange(256)
        apdu = bytes([0xB0, ins]) + bytes(
            rng.randrange(256) for _ in range(rng.randrange(2, 62)))
        m.exchange(apdu)
    assert w.sc.request(b"\x00\x00still-there") == b"still-there"
    w.sc.close()
