#!/usr/bin/env python3
"""The unmodified specter wallet against the virtual MemoryCard model.

The strongest dry proof available: the REAL wallet client code
(MemoryCardApplet + SecureChannel with its HMAC/signature verifiers)
runs against host-tools/specter_card_model.py through the REAL uscard
stub and specter_sim_relay — SELECT, key exchange, secure-channel
open (the wallet VERIFIES our HMAC and ECDSA signature), encrypted
ECHO/SECURE_RANDOM, and a SET/GET secret roundtrip.
"""
import sys
import threading
import time
import socket
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "host-tools"))

import specter_sim_relay  # noqa: E402

SPECTER_DIR = Path("/root/src/specter-diy")

# guards BEFORE the model import (it needs Crypto+coincurve at module
# scope) — the CI collect gate runs without them
pytest.importorskip("Crypto")
pytest.importorskip("coincurve")
import specter_card_model  # noqa: E402


def _start_relay(backend, port):
    relay = specter_sim_relay.SimRelay(backend)
    t = threading.Thread(target=relay.serve_forever, args=(port,),
                         daemon=True)
    t.start()
    for _ in range(50):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=1).close()
            return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("relay did not come up")


def _load_wallet():
    """The real wallet applet classes under CPython (stdlib shims)."""
    import types as T
    import importlib
    import importlib.util
    if "micropython" not in sys.modules:
        mp = T.ModuleType("micropython")
        mp.const = lambda x: x
        sys.modules["micropython"] = mp
    if "rng" not in sys.modules:
        r = T.ModuleType("rng")
        r.get_random_bytes = lambda n: __import__("os").urandom(n)
        sys.modules["rng"] = r
    if "ucryptolib" not in sys.modules:
        uc = T.ModuleType("ucryptolib")

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
        sys.modules["ucryptolib"] = uc
    if "secp256k1" not in sys.modules:
        # the wallet's unix build maps micropython-secp256k1; shim the
        # exact functions SecureChannel uses onto coincurve
        import coincurve as _cc

        class _P:
            """Mutable point holder: micropython-secp256k1 MUTATES pubkey
            objects in tweak_mul (statement form, return discarded) while
            coincurve returns a new object — without the holder the wallet
            derives the shared secret from the UNTWEAKED point (the exact
            HMAC-mismatch class this shim survived)."""

            def __init__(self, pub):
                self._pub = pub

            def format(self, compressed=False):
                return self._pub.format(compressed=compressed)

            def verify(self, compact, digest, hasher=None):
                return self._pub.verify(compact, digest, hasher=None)

        cc = T.ModuleType("secp256k1")
        cc.EC_UNCOMPRESSED = 0
        cc.EC_COMPRESSED = 1
        cc.ec_pubkey_parse = lambda b: _P(_cc.PublicKey(bytes(b)))
        cc.ec_pubkey_create = lambda sec: _P(
            _cc.PrivateKey(bytes(sec)).public_key)
        cc.ec_pubkey_serialize = lambda pub, fmt=0: pub.format(
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
        cc.ecdsa_verify = lambda sig, digest, pub: pub.verify(
            sig.der, digest, hasher=None)
        sys.modules["secp256k1"] = cc
    sys.path.insert(0, str(SPECTER_DIR / "f469-disco" / "libs" / "unix"))
    jc = SPECTER_DIR / "src" / "keystore" / "javacard"

    def load(name, path):
        spec = importlib.util.spec_from_file_location(name, path)
        m = importlib.util.module_from_spec(spec)
        sys.modules[name] = m
        spec.loader.exec_module(m)
        return m

    pkg = T.ModuleType("jk")
    pkg.__path__ = [str(jc)]
    sys.modules["jk"] = pkg
    ap = T.ModuleType("jk.applets")
    ap.__path__ = [str(jc / "applets")]
    sys.modules["jk.applets"] = ap
    load("jk.util", str(jc / "util.py"))
    load("jk.applets.applet", str(jc / "applets" / "applet.py"))
    return importlib.import_module("jk.applets.memorycard")


@pytest.mark.skipif(not SPECTER_DIR.exists(),
                    reason="specter-diy tree absent (bench host only)")
def test_wallet_full_positive_path_against_model():
    mc = _load_wallet()
    from uscard import Reader
    _start_relay(specter_card_model.MemoryCardModel(), 21111)  # uscard stub hardcodes 21111
    conn = Reader().createConnection()
    assert conn.isCardInserted()
    conn.connect(1)
    card = mc.MemoryCardApplet(conn)
    card.select()                       # SELECT -> 9000
    pub = card.sc.get_card_pubkey()     # GET_PUBKEY parses
    assert pub is not None
    card.sc.open("es")                  # OPEN_SE: wallet VERIFIES our
    #                                    HMAC + ECDSA signature
    assert card.sc.is_open
    # encrypted round-trips
    echo = card.sc.request(b"\x00\x00hello")
    assert echo == b"hello", f"echo mismatch: {echo!r}"
    rnd = card.sc.request(b"\x01\x00")
    assert len(rnd) == 32
    # MemoryCard secret roundtrip over the secure channel
    card.save_secret(b"my-test-secret")
    assert card.get_secret() == b"my-test-secret"
    card.sc.close()
    assert not card.sc.is_open


def test_model_raw_wire_shapes():
    m = specter_card_model.MemoryCardModel()
    m.connect()
    bad_sel = bytes.fromhex("00A40400021122")
    assert m.exchange(bad_sel) == bytes.fromhex("6a82")
    pub = m.exchange(bytes.fromhex("00A4040006B00B5111CB01"))
    assert pub == bytes.fromhex("9000")
    r = m.exchange(bytes.fromhex("B0B20000"))
    assert len(r) == 65 + 2 and r[-2:] == bytes.fromhex("9000")
    assert m.exchange(bytes.fromhex("B0B70000")) == bytes.fromhex("9000")
    # SECURE_MSG before channel open -> security status not satisfied
    assert m.exchange(bytes.fromhex("B0B6000000")) == \
        bytes.fromhex("6982")
