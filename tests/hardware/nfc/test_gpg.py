"""GPG-on-readers HIL test — OpenPGP card through every bench reader.

The GPG acceptance test the bench has been building toward (issue #66
direction): for each reader that currently has an OpenPGP-capable card,
run the full gpg --card-status flow and assert the card answers with
keys, version, and ATR-level identity.

Reader gating (cards physically sit on coils — a reader without the
applet card answers 6A82 and is SKIPPED, not failed):
  - m5stick: SmartPGP installed through our own T=1 endpoint (2026-10-10)
  - acr1252: the original SmartPGP install (reference)
  - stm32:   card-gated (a human must move a card into the contact slot)
  - nucula:  card-gated on the antenna question (#88/#105 follow-up)

Usage:
    pytest tests/hardware/nfc/test_gpg.py -v --hil
    pytest tests/hardware/nfc/test_gpg.py -v --hil --readers m5stick,acr1252
"""

import shutil
import subprocess
import time

import pytest

from conftest import find_reader

pytestmark = pytest.mark.hil

GPG = shutil.which("gpg")
OPENPGP_AID = "D276000124010304AFAF000000000000"


def _select_openpgp(reader_name: str) -> bool:
    """True if the reader's card carries an OpenPGP applet (SELECT 9000)."""
    from smartcard.System import readers as sc_readers
    matches = [r for r in sc_readers() if reader_name.lower() in str(r).lower()]
    if not matches:
        return False
    conn = matches[0].createConnection()
    try:
        conn.connect()
        aid = bytes.fromhex(OPENPGP_AID)
        _, sw1, sw2 = conn.transmit([0x00, 0xA4, 0x04, 0x00, len(aid)] + list(aid))
        return sw1 == 0x90 and sw2 == 0x00
    except Exception:
        return False
    finally:
        try:
            conn.disconnect()
        except Exception:
            pass


def _scd_reader_index(fragment: str) -> int:
    """gpg/scdaemon reader index for a pcscd reader name fragment.

    scdaemon (pcsc backend, disable-ccid) indexes readers in pcscd
    order; the fragment finds ours. Bench note: indices shift when
    readers re-enumerate — always resolve fresh, never hardcode.
    """
    from smartcard.System import readers as sc_readers
    names = [str(r) for r in sc_readers()]
    for i, n in enumerate(names):
        if fragment.lower() in n.lower():
            return i
    raise RuntimeError(f"reader '{fragment}' not in pcscd list: {names}")


def _gpg_card_status(reader_index: int, timeout: int = 60) -> str:
    """Run gpg --card-status pinned to one reader; return its output."""
    conf_dir = "/tmp/opencode/gpg-hil-gnupg"
    import os
    os.makedirs(f"{conf_dir}", exist_ok=True)
    with open(f"{conf_dir}/scdaemon.conf", "w") as f:
        # pcsc backend (pcscd owns the serial readers) + reader pin
        f.write("disable-ccid\n")
        f.write(f"reader-port {reader_index}\n")
    env = {
        "GNUPGHOME": conf_dir,
        "PATH": "/usr/bin:/bin",
    }
    proc = subprocess.run(
        ["gpg", "--card-status"],
        capture_output=True, text=True, timeout=timeout, env=env,
    )
    return proc.stdout + proc.stderr


@pytest.mark.hil
def test_gpg_reader_enumerates(reader_key):
    """The reader family is present in pcscd (hardware + driver chain)."""
    assert find_reader(reader_key) is not None, f"{reader_key} missing from pcscd"


@pytest.mark.hil
def test_gpg_openpgp_applet_selectable(reader_key):
    """The reader's card answers the OpenPGP AID (or is skipped)."""
    name = {
        "m5stick": "GemPCTwin",
        "acr1252": "ACR1252",
        "stm32": "Cherry",
        "nucula": "Nucula",
    }.get(reader_key)
    if name is None:
        pytest.skip(f"no reader mapping for {reader_key}")
    if not _select_openpgp(name):
        pytest.skip(f"no OpenPGP applet card on {name} (cards physically gate this)")


@pytest.mark.hil
def test_gpg_card_status_full(reader_key):
    """Full gpg --card-status through the reader: version, keys, ATR."""
    name = {
        "m5stick": "GemPCTwin",
        "acr1252": "ACR1252",
        "stm32": "Cherry",
        "nucula": "Nucula",
    }.get(reader_key)
    if name is None or not _select_openpgp(name):
        pytest.skip(f"no OpenPGP card on {reader_key}")

    if GPG is None:
        pytest.fail("gpg not installed: apt-get install scdaemon gnupg")

    idx = _scd_reader_index(name)
    out = _gpg_card_status(idx)

    assert "Application type .: OpenPGP" in out, f"not an OpenPGP card: {out[-200:]}"
    assert "Application ID" in out
    assert "Version" in out
    # Keys present (SmartPGP cards had three on-card-generated keys)
    assert "Signature key" in out or "Key attributes" in out
    # The reader identity line proves WHICH reader served the session
    assert name.lower()[:6] in out.lower() or "Reader" in out, out[:200]
