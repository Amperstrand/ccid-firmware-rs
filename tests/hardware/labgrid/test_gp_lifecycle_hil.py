"""GP applet lifecycle HIL on the STM32F469 DUT (#132/#139 milestone).

Proves the reader-role end state through OUR firmware: GlobalPlatform
secure channel (SCP02) + full CAP lifecycle — install, use (SELECT /
PUT / GET round-trip), uninstall, and the card power-cycle reset flow.

Card-gated by CPLC serial: this test ONLY runs against the factory
Teapot card (50796920, OP_READY, AUTH-triple keys — the dedicated
install/delete target). Any other card in the STM32 slot SKIPs loudly:
install/delete are state-changing and must never touch fixture cards
(card-handling doctrine, AGENTS.md).
"""

import subprocess
from pathlib import Path

import pytest

from bench import ledger

pytestmark = pytest.mark.hil

TEAPOT_CARD_CPLC = "50796920"
TEAPOT_AID = "B00B5111CA01"
TEAPOT_PKG = "B00B5111CA"
GP_KEYS = (
    "--key-enc 5A9E63D03BADBC2A240FE8F534709EDF "
    "--key-mac 7CCC1E79D64FC5FA263B8F2955282998 "
    "--key-dek B040703EC3DE23EE8AE4CFB6D632AA80"
)
CAP = Path(__file__).parent / "caps" / "TeapotApplet.cap"
# Provenance: specter-javacard CI run 34037943401 cap-files artifact
# (sha256 d0cf72bd5a16715d14a5d04ff73a76de8b783f67a1e41c093575a913f105e084).


def _gp(reader_name: str, args: str, timeout: int = 120) -> subprocess.CompletedProcess:
    return subprocess.run(
        f'gp --reader "{reader_name}" {GP_KEYS} {args}',
        shell=True, capture_output=True, text=True, timeout=timeout,
    )


def _teapot_card_in_slot() -> bool:
    """CPLC gate — install/delete only on the dedicated factory card."""
    from smartcard.System import readers
    from smartcard.util import toBytes

    rs = [r for r in readers() if "ST2XXX-001" in str(r)]
    if not rs:
        return False
    c = rs[0].createConnection()
    try:
        c.connect()
        d, s1, s2 = c.transmit(toBytes("80CA9F7F00"))
        cplc = bytes(d).hex().upper()
        c.disconnect()
        return s1 == 0x90 and TEAPOT_CARD_CPLC in cplc
    except Exception:
        return False


def test_gp_lifecycle_through_stm32(labgrid_place, pcscd_running):
    reader = "Cherry GmbH SmartTerminal ST-2xxx (ST2XXX-001) 01 00"
    if not _teapot_card_in_slot():
        pytest.skip(
            f"factory Teapot card (CPLC {TEAPOT_CARD_CPLC}) not in STM32 slot — "
            "install/delete never run against fixture cards (AGENTS.md doctrine)"
        )

    from smartcard.System import readers
    from smartcard.util import toBytes

    # 1. Secure channel + listing through OUR reader (#132 proof)
    r = _gp(reader, "-l")
    assert r.returncode == 0 and "ISD: A000000151000000" in r.stdout, \
        f"gp -l failed through Cherry:\n{r.stdout}\n{r.stderr}"

    # 2. Clean pre-state (idempotency: a failed earlier run may have left
    # the package loaded — INSTALL onto an existing package is 6985).
    pre = _gp(reader, "-l")
    if TEAPOT_PKG in pre.stdout:
        _gp(reader, f"--delete {TEAPOT_PKG} --force")

    # 3. INSTALL
    r = _gp(reader, f"--install {CAP}")
    assert r.returncode == 0 and "loaded" in r.stdout.lower(), \
        f"install failed:\n{r.stdout}\n{r.stderr}"

    # 4. USE: SELECT / PUT / GET byte-exact round-trip
    rs = [x for x in readers() if "ST2XXX-001" in str(x)]
    c = rs[0].createConnection()
    c.connect()
    secret = b"gp-lifecycle-hil"
    d, s1, s2 = c.transmit(toBytes(f"00A4040006{TEAPOT_AID}00"))
    assert (s1, s2) == (0x90, 0x00), f"Teapot SELECT: {s1:02X}{s2:02X}"
    d, s1, s2 = c.transmit(toBytes("B0A20000" + f"{len(secret):02X}" + secret.hex().upper()))
    assert (s1, s2) == (0x90, 0x00), f"PUT: {s1:02X}{s2:02X}"
    d, s1, s2 = c.transmit(toBytes("B0A10000"))
    assert (s1, s2) == (0x90, 0x00) and secret in bytes(d), f"GET round-trip: {s1:02X}{s2:02X} {bytes(d)!r}"
    atr = bytes(c.getATR())
    c.disconnect()

    # 5. Card reset flow: full VCC unpower through pcscd, then fresh
    # power-on — ATR identical, ISD reachable (the "reset cards" leg).
    import time as _t

    from smartcard.scard import (
        SCARD_LEAVE_CARD, SCARD_PROTOCOL_T1, SCARD_SCOPE_USER,
        SCARD_SHARE_SHARED, SCARD_UNPOWER_CARD, SCardConnect,
        SCardDisconnect, SCardEstablishContext, SCardReleaseContext,
        SCardStatus, SCardTransmit,
    )

    hres, hctx = SCardEstablishContext(SCARD_SCOPE_USER)
    hres, hcard, proto = SCardConnect(hctx, reader, SCARD_SHARE_SHARED, SCARD_PROTOCOL_T1)
    SCardDisconnect(hcard, SCARD_UNPOWER_CARD)
    _t.sleep(1.0)
    hres, hcard, proto = SCardConnect(hctx, reader, SCARD_SHARE_SHARED, SCARD_PROTOCOL_T1)
    hres, _, _, _, atr2 = SCardStatus(hcard)
    hres, resp = SCardTransmit(hcard, proto, list(toBytes("00A4040008A000000151000000")))
    assert bytes(atr2) == atr, "ATR changed across reset"
    assert (resp[-2], resp[-1]) == (0x90, 0x00), f"ISD unreachable after reset: {resp[-2]:02X}{resp[-1]:02X}"
    SCardDisconnect(hcard, SCARD_LEAVE_CARD)
    SCardReleaseContext(hctx)

    # 6. UNINSTALL + verify gone
    r = _gp(reader, f"--delete {TEAPOT_PKG} --force")
    assert r.returncode == 0, f"delete failed:\n{r.stdout}\n{r.stderr}"
    r = _gp(reader, "-l")
    assert TEAPOT_PKG not in r.stdout, "package still listed after delete"

    ledger("gp_lifecycle", place="stm32-ccid", result="install-use-uninstall OK")
