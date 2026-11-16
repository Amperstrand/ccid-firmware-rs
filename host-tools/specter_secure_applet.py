#!/usr/bin/env python3
"""Specter SecureApplet virtual card — the PIN-managing base applet.

specter_card_model.py models the wallet's MemoryCardApplet (the
subclass). This module models the BASE applet the wallet calls
SecureApplet (src/keystore/javacard/applets/secureapplet.py — the
client is the contract): the identical secure-channel transport
(SELECT / GET_PUBKEY / OPEN_SE / OPEN_EE / SECURE_MSG / CLOSE —
IMPORTED from the MemoryCard model via inheritance, not duplicated)
carrying the base applet's own op set and PIN state machine:

  secure ops (plaintext inside SECURE_MSG):
    ECHO 0000 || payload       SECURE_RANDOM 0100
    PIN_STATUS 0300            UNLOCK 0301 + sha256(pin)
    LOCK 0302                  CHANGE_PIN 0303 + encode(old)+encode(new)
    SET_PIN 0304 + sha256(pin)

  PIN_STATUS reply: attempts_left || attempts_max || status, the
  order the client unpacks ((left, max, status) = list(reply)), with
  status UNSET=0 LOCKED=1 UNLOCKED=2 BRICKED=3.

  Applet error codes the wallet maps behavior on (keystore/
  memorycard.py _unlock): 0502 wrong PIN -> PinError,
  0503 bricked -> CriticalErrorWipeImmediately. 0500/0501 (PIN not
  set / already set) are model choices for guards the wallet only
  enforces client-side.

State machine (JavaCard OwnerPIN semantics):
  - power-up with a set PIN is LOCKED — validation never survives a
    reset, but the attempt budget DOES (brick risk survives
    reinsertion; connect() re-locks without restoring attempts);
  - a wrong PIN (UNLOCK or the old-PIN leg of CHANGE_PIN) burns one
    attempt; the attempt that exhausts the budget BRICKS the card —
    no recovery, exactly the wipe path the keystore wires to 0503;
  - SET_PIN only from UNSET and leaves the card LOCKED (update()
    never validates); CHANGE_PIN requires the UNLOCKED state;
  - the applet has NO secret ops (0500/0501 are MemoryCard's) —
    they answer 6D00 like any unknown op.

The SecureApplet AID is the next slot in the specter-diy applet
block (MemoryCard owns CB01); selecting the MemoryCard AID here
fails with 6A82 — this is a different applet.

Local virtual card only (card-safety doctrine): a CardBackend for
specter_sim_relay / in-process DirectConn tests — never wired to a
physical reader. Crypto comes with the imported channel machinery:
coincurve (secp256k1) + pycryptodome (AES-CBC, HMAC).
"""
from __future__ import annotations

import hashlib
import hmac as hmac_mod
import os

import specter_card_model as _memory

# next AID in the specter-diy applet block — MemoryCard owns CB01
AID = bytes.fromhex("B00B5111CB02")

# secure-op codes (SecureApplet client class attributes)
OP_ECHO = b"\x00\x00"
OP_SECURE_RANDOM = b"\x01\x00"
OP_PIN_STATUS = b"\x03\x00"
OP_UNLOCK = b"\x03\x01"
OP_LOCK = b"\x03\x02"
OP_CHANGE_PIN = b"\x03\x03"
OP_SET_PIN = b"\x03\x04"

# PIN status codes (client: PIN_UNSET..PIN_BRICKED)
PIN_UNSET = 0
PIN_LOCKED = 1
PIN_UNLOCKED = 2
PIN_BRICKED = 3

# applet-level error codes inside the secure channel (the client
# hexlifies them into SecureError("0502") etc)
ERR_PIN_NOT_SET = b"\x05\x00"
ERR_PIN_ALREADY_SET = b"\x05\x01"
ERR_WRONG_PIN = b"\x05\x02"
ERR_BRICKED = b"\x05\x03"

SW_WRONG_LENGTH = b"\x6a\x80"


def _take_field(buf: bytes, i: int) -> tuple[bytes, int]:
    """Inverse of the client's encode(): one length-prefixed field."""
    n = buf[i]
    return buf[i + 1:i + 1 + n], i + 1 + n


class SecureAppletModel(_memory.MemoryCardModel):
    """Stateful virtual SecureApplet: the MemoryCard model's channel
    machinery (imported, not duplicated) + the base applet's PIN
    state machine and op set. One instance = one card."""

    name = "secure-applet-model"

    def __init__(self, card_key: bytes | None = None,
                 pin: str | None = None,
                 pin_attempts_max: int = 3):
        super().__init__(card_key=card_key)
        self.pin_attempts_max = pin_attempts_max
        self.pin_attempts_left = pin_attempts_max
        # the client always hashes before sending -> we store the hash
        self.pin_hash = (hashlib.sha256(pin.encode()).digest()
                         if pin is not None else None)
        self.pin_status = PIN_LOCKED if pin is not None else PIN_UNSET

    # --- CardBackend surface -------------------------------------------

    def connect(self) -> None:
        # power-up: validation never survives a reset, the attempt
        # budget does (OwnerPIN: tries remaining is persistent state)
        if self.pin_hash is not None and self.pin_status != PIN_BRICKED:
            self.pin_status = PIN_LOCKED
        super().connect()

    def status(self) -> dict:
        st = super().status()
        st.update({"pin_status": self.pin_status,
                   "pin_attempts_left": self.pin_attempts_left,
                   "pin_attempts_max": self.pin_attempts_max})
        return st

    def exchange(self, capdu: bytes) -> bytes:
        """Route SELECT to OUR aid, everything else is the shared
        channel machinery. Selecting any other AID (e.g. the
        MemoryCard's CB01) fails — this applet is not installed
        there."""
        if len(capdu) >= 4 and capdu[0] == 0x00 and capdu[1] == 0xA4:
            body = capdu[5:5 + capdu[4]] if len(capdu) > 5 else b""
            if body == AID:
                self.selected = True
                return _memory.OP_SUCCESS
            return b"\x6a\x82"
        return super().exchange(capdu)

    # --- PIN state machine ----------------------------------------------

    def _burn_attempt(self) -> bytes:
        self.pin_attempts_left -= 1
        if self.pin_attempts_left <= 0:
            self.pin_status = PIN_BRICKED
            return ERR_BRICKED
        return ERR_WRONG_PIN

    def _handle_op(self, plain: bytes) -> bytes:
        """Secure op -> plaintext answer (SUCCESS || data, or error)."""
        op, data = plain[:2], plain[2:]
        if op == OP_ECHO:
            return _memory.OP_SUCCESS + data
        if op == OP_SECURE_RANDOM:
            return _memory.OP_SUCCESS + os.urandom(32)
        if op == OP_PIN_STATUS:
            return _memory.OP_SUCCESS + bytes(
                [self.pin_attempts_left, self.pin_attempts_max,
                 self.pin_status])
        if op == OP_SET_PIN:
            return self._op_set_pin(data)
        if op == OP_UNLOCK:
            return self._op_unlock(data)
        if op == OP_LOCK:
            return self._op_lock(data)
        if op == OP_CHANGE_PIN:
            return self._op_change_pin(data)
        return b"\x6d\x00"  # unknown op (includes MemoryCard 05xx)

    def _op_set_pin(self, data: bytes) -> bytes:
        if self.pin_hash is not None:
            return ERR_PIN_ALREADY_SET
        if len(data) != 32:
            return SW_WRONG_LENGTH
        self.pin_hash = bytes(data)
        self.pin_attempts_left = self.pin_attempts_max
        self.pin_status = PIN_LOCKED  # update() never validates
        return _memory.OP_SUCCESS

    def _op_unlock(self, data: bytes) -> bytes:
        if self.pin_status == PIN_BRICKED:
            return ERR_BRICKED
        if self.pin_hash is None:
            return ERR_PIN_NOT_SET
        if len(data) != 32 or not hmac_mod.compare_digest(
                self.pin_hash, bytes(data)):
            return self._burn_attempt()
        self.pin_attempts_left = self.pin_attempts_max
        self.pin_status = PIN_UNLOCKED
        return _memory.OP_SUCCESS

    def _op_lock(self, data: bytes) -> bytes:
        if self.pin_hash is None:
            return ERR_PIN_NOT_SET
        self.pin_status = PIN_LOCKED
        return _memory.OP_SUCCESS

    def _op_change_pin(self, data: bytes) -> bytes:
        if self.pin_hash is None:
            return ERR_PIN_NOT_SET
        old, i = _take_field(data, 0)
        new, i = _take_field(data, i)
        if i != len(data) or len(old) != 32 or len(new) != 32:
            return SW_WRONG_LENGTH
        if self.pin_status != PIN_UNLOCKED or not hmac_mod.compare_digest(
                self.pin_hash, old):
            # a failed old-PIN check burns an attempt like a failed
            # unlock — the keystore's lock()/unlock()/change() dance
            # means it only ever arrives while unlocked
            return self._burn_attempt()
        self.pin_hash = new
        self.pin_attempts_left = self.pin_attempts_max
        return _memory.OP_SUCCESS
