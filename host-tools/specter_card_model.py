#!/usr/bin/env python3
"""Specter MemoryCard virtual card — the positive-path card model.

Implements the card side of the specter-diy javacard keystore protocol
exactly as the WALLET CLIENT defines it (src/keystore/javacard/applets/
{applet,secureapplet,securechannel,memorycard}.py — the client is the
contract):

  plain:   SELECT / GET_PUBKEY(B0B2) / OPEN_SE(B0B4, default "es" mode:
           nonce + HMAC(card_mac_key,nonce)[:14] + DER-sig) /
           SECURE_MSG(B0B6: AES-CBC, counter IV, HMAC-SHA256) / CLOSE
  secure:  ECHO 0000 / SECURE_RANDOM 0100 / PIN ops 03xx /
           GET_SECRET 0500 / SET_SECRET 0501

Key schedule (securechannel.derive_keys): sha256(b"host_aes"|"card_aes"
|"host_mac"|"card_mac" + secret_with_nonces), where
secret_with_nonces = sha256(shared_x + nonce_card) and shared_x is the
compressed-x ECDH point (host eph key * card static key).

The model is a CardBackend: feed it to specter_sim_relay
(--backend model) and the unmodified wallet simulator gets a working
card — SELECT 9000, real channel handshake, real encrypted ops — with
zero hardware and zero Java. Crypto: coincurve (secp256k1 ECDH/ECDSA)
+ pycryptodome (AES-CBC, HMAC).
"""
from __future__ import annotations

import hashlib
import hmac as hmac_mod
import os

from Crypto.Cipher import AES

import coincurve

AES_BLOCK = 16
IV_SIZE = 16
MAC_SIZE = 14

AID = bytes.fromhex("B00B5111CB01")

#: ATR of the physical card this model stands in for: the bench's
#: ComSign eID behind the Cherry ST-2xxx (AGENTS.md host table —
#: T=1, IFSC=254, TCK correct). Presented over vpcd so pcscd runs its
#: real T=1 stack against the model exactly as it does against the
#: card (vpcd_card_server picks this up via the backend's `atr`).
COMSIGN_EID_ATR = bytes.fromhex("3BD518FF8191FE1FC38073C821100A")

INS_GET_PUBKEY = 0xB2
INS_OPEN_EE = 0xB5
INS_OPEN_SE = 0xB4
INS_OPEN_EE = 0xB5
INS_SECURE_MSG = 0xB6
INS_CLOSE = 0xB7

OP_SUCCESS = b"\x90\x00"


class CardModelError(Exception):
    pass


class MemoryCardModel:
    """Stateful virtual MemoryCard. One instance = one card session."""

    name = "memorycard-model"

    def __init__(self, card_key: bytes | None = None,
                 secret: bytes | None = None):
        self.card_priv = coincurve.PrivateKey(
            card_key if card_key is not None else os.urandom(32))
        self.secret = secret
        self.pin_status = 0  # PIN_UNSET
        # channel state
        self.chan_open = False
        self.iv = 0
        self.host_aes = self.card_aes = None
        self.host_mac = self.card_mac = None
        self.selected = False

    # --- CardBackend surface -------------------------------------------

    @property
    def atr(self) -> bytes:
        """ATR to present to a reader (vpcd): the ComSign eID shape."""
        return COMSIGN_EID_ATR

    def connect(self) -> None:
        self.selected = False

    def disconnect(self) -> None:
        self._close_channel()

    def status(self) -> dict:
        return {"name": self.name, "channel_open": self.chan_open,
                "iv": self.iv, "has_secret": self.secret is not None}

    def exchange(self, capdu: bytes) -> bytes:
        """One C-APDU -> R-APDU (data || SW)."""
        if len(capdu) < 4:
            return b"\x6d\x00"
        cla, ins, p1, p2 = capdu[:4]
        if cla == 0x00 and ins == 0xA4:
            body = capdu[5:5 + capdu[4]] if len(capdu) > 5 else b""
            if body == AID:
                self.selected = True
                return OP_SUCCESS
            return b"\x6a\x82"
        if cla != 0xB0:
            return b"\x6e\x00"
        if ins == INS_GET_PUBKEY:
            pub = self.card_priv.public_key.format(compressed=False)
            return pub + OP_SUCCESS
        if ins == INS_OPEN_SE:
            return self._open_se(capdu[5:5 + capdu[4]])
        if ins == INS_OPEN_EE:
            return self._open_ee(capdu[5:5 + capdu[4]])
        if ins == INS_SECURE_MSG:
            return self._secure_msg(capdu[5:5 + capdu[4]])
        if ins == INS_CLOSE:
            self._close_channel()
            return OP_SUCCESS
        return b"\x6d\x00"

    # --- channel machinery ----------------------------------------------

    def _derive(self, shared_secret: bytes) -> bytes:
        self.host_aes = hashlib.sha256(b"host_aes" + shared_secret).digest()
        self.card_aes = hashlib.sha256(b"card_aes" + shared_secret).digest()
        self.host_mac = hashlib.sha256(b"host_mac" + shared_secret).digest()
        self.card_mac = hashlib.sha256(b"card_mac" + shared_secret).digest()
        return hashlib.sha256(shared_secret).digest()[:4]

    def _open_se(self, host_pub65: bytes) -> bytes:
        """es mode: shared = ECDH(host_eph_pub, card_static_priv) x-coord;
        secret_with_nonces = sha256(shared || nonce_card); reply =
        nonce || HMAC(card_mac,nonce)[:14] || DER-sig(sha256(nonce||hmac))."""
        try:
            host_pub = coincurve.PublicKey(host_pub65)
        except ValueError:
            return b"\x6a\x80"  # wrong data — not a point
        shared_point = host_pub.multiply(self.card_priv.secret)
        shared = shared_point.format(compressed=True)[1:33]
        nonce = os.urandom(32)
        secret_with_nonces = hashlib.sha256(shared + nonce).digest()
        self._derive(secret_with_nonces)
        mac = hmac_mod.new(self.card_mac, nonce,
                           digestmod="sha256").digest()[:MAC_SIZE]
        data = nonce + mac
        sig = self.card_priv.sign(
            hashlib.sha256(data).digest(), hasher=None)
        self.chan_open = True
        self.iv = 0
        return data + sig + OP_SUCCESS

    def _open_ee(self, host_pub65: bytes) -> bytes:
        """ee mode: the card generates an EPHEMERAL key; shared =
        sha256(x-coord of ECDH(host_eph_pub, card_eph_priv)) — hashed
        x, unlike es mode's raw x + card nonce; reply = card_eph65 ||
        HMAC(card_mac, card_eph65)[:14] || DER-sig(static card key over
        sha256(card_eph65||mac)). Signature stays on the STATIC key —
        the wallet verifies it against get_card_pubkey."""
        try:
            host_pub = coincurve.PublicKey(host_pub65)
        except ValueError:
            return b"\x6a\x80"  # wrong data — not a point
        eph = coincurve.PrivateKey()
        pt = host_pub.multiply(eph.secret)
        shared = hashlib.sha256(
            pt.format(compressed=True)[1:33]).digest()
        self._derive(shared)
        pub65 = eph.public_key.format(compressed=False)
        mac = hmac_mod.new(self.card_mac, pub65,
                           digestmod="sha256").digest()[:MAC_SIZE]
        sig = self.card_priv.sign(
            hashlib.sha256(pub65 + mac).digest(), hasher=None)
        self.chan_open = True
        self.iv = 0
        return pub65 + mac + sig + OP_SUCCESS

    def _secure_msg(self, ct: bytes) -> bytes:
        if not self.chan_open:
            return b"\x69\x82"
        recv_mac, body = ct[-MAC_SIZE:], ct[:-MAC_SIZE]
        iv = self.iv.to_bytes(IV_SIZE, "big")
        exp = hmac_mod.new(self.host_mac, iv + body,
                           digestmod="sha256").digest()[:MAC_SIZE]
        if not hmac_mod.compare_digest(exp, recv_mac):
            return b"\x69\x84"  # MAC check failed
        plain = AES.new(self.host_aes, AES.MODE_CBC, iv).decrypt(body)
        # strip \x80.. padding (the client's padding rule)
        arr = plain.split(b"\x80")
        if len(arr) == 1 or arr[-1].strip(b"\x00"):
            return b"\x69\x84"
        plain = b"\x80".join(arr[:-1])
        # process the op, answer encrypted with the CARD keys at same iv
        resp_plain = self._handle_op(plain)
        d = resp_plain + b"\x80"
        if len(d) % AES_BLOCK:
            d += b"\x00" * (AES_BLOCK - len(d) % AES_BLOCK)
        out_ct = AES.new(self.card_aes, AES.MODE_CBC, iv).encrypt(d)
        out_mac = hmac_mod.new(self.card_mac, iv + out_ct,
                               digestmod="sha256").digest()[:MAC_SIZE]
        self.iv += 1
        return out_ct + out_mac + OP_SUCCESS

    def _handle_op(self, plain: bytes) -> bytes:
        """Secure op -> plaintext answer (SUCCESS || data, or 2B error)."""
        op, data = plain[:2], plain[2:]
        if op == b"\x00\x00":          # ECHO
            return OP_SUCCESS + data
        if op == b"\x01\x00":          # SECURE_RANDOM
            return OP_SUCCESS + os.urandom(32)
        if op == b"\x03\x00":          # PIN_STATUS
            return OP_SUCCESS + bytes([self.pin_status, 3, 3])
        if op == b"\x05\x01":          # SET_SECRET
            self.secret = data
            return OP_SUCCESS
        if op == b"\x05\x00":          # GET_SECRET
            if self.secret is None:
                return b"\x69\x85"
            return OP_SUCCESS + self.secret
        return b"\x6d\x00"             # unknown INS

    def _close_channel(self) -> None:
        self.chan_open = False
        self.iv = 0
        self.host_aes = self.card_aes = None
        self.host_mac = self.card_mac = None
