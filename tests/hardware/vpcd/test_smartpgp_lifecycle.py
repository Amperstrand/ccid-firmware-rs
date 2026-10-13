"""DESTRUCTIVE SmartPGP lifecycle over the vpcd reader — the crown jewel.

This module WIPES the bench card's OpenPGP applet (gp --delete) and
reinstalls it from the CAP file. Bench dev card ONLY — it re-encodes the
delete -> verify(6A82) -> install -> gpg cycle proven twice on
2026-10-13 (runbook "Bench evidence", M8).

Run as a WHOLE module — the tests are one lifecycle chain in file order:
    pytest tests/hardware/vpcd/test_smartpgp_lifecycle.py -v -s

Operational rules encoded (docs/VPCD_WIFI_RUNBOOK.md):
- "GP": reader passed to gp by NAME (--reader "Nucula VPCD 00 00") —
  Java SmartcardIO indices differ from pyscard order.
- "GP": --bs 200 is REQUIRED — NCI DATA packets carry a 1-byte length;
  APDUs > ~245 B fail SW 6400 today. A 117 KB CAP installs in ~4 min.
- "GP" + "GPG": keys come from env vars, never from the repo.
- "GPG": temp GNUPGHOME with scdaemon.conf = disable-ccid + reader-port
  <index from SCardListReaders>; reader-port takes the numeric INDEX
  (names with spaces do not match).
- "Operational rules" #1: scdaemon is killed ONLY with
  `gpgconf --kill scdaemon` — SIGKILL leaves stale exclusive locks in
  pcscd ("No such device" until restart). Enforced in a finally block.
- ~2 s/APDU: subprocess timeouts are generous (install: 15 min).
"""

import os
import re
import subprocess
import tempfile

import pytest

from .conftest import GP_KEY_ENV

#: OpenPGP application AID on the bench card (instance of package D27600012401).
OPENPGP_AID = "D276000124010304AFAF000000000000"
#: SmartPGP package AID (RID + OpenPGP applet prefix).
PKG_AID = "D27600012401"
#: ISD of the bench SmartPGP card.
ISD_AID = "A000000151000000"
#: Bench CAP location (caps are never committed — prompt-known bench path,
#: override with NUCULA_SMARTPGP_CAP).
DEFAULT_CAP = ("/tmp/opencode/caps/smartpgp/"
               "SmartPGPApplet-v1.23.2.0-javacard-3.0.4-rsa_up_to_4096.cap")

_keys_missing = [n for n in GP_KEY_ENV if not os.environ.get(n)]

pytestmark = pytest.mark.skipif(
    _keys_missing,
    reason=f"destructive lifecycle needs GP keys in env: {', '.join(_keys_missing)}",
)


@pytest.fixture(scope="module")
def gp(card_slot, gp_keys):
    """gp runner bound to the card's reader BY NAME (runbook "GP")."""
    name, _idx, _atr = card_slot

    def _gp(*args, timeout=300):
        cmd = ["gp", "--reader", name,
               "--key-enc", os.environ["NUCULA_GP_KEY_ENC"],
               "--key-mac", os.environ["NUCULA_GP_KEY_MAC"],
               "--key-dek", os.environ["NUCULA_GP_KEY_DEK"],
               "--bs", "200", *args]
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        out = (p.stdout + "\n" + p.stderr).strip()
        print(f"$ {' '.join(args)}\n{out}\n", flush=True)
        return p.returncode, out

    return _gp


@pytest.fixture(scope="module")
def cap_path():
    path = os.environ.get("NUCULA_SMARTPGP_CAP", DEFAULT_CAP)
    if not os.path.isfile(path):
        pytest.skip(f"SmartPGP CAP not found at {path} "
                    "(caps are not committed; set NUCULA_SMARTPGP_CAP)")
    return path


@pytest.fixture(scope="module", autouse=True)
def _kill_scdaemon_final():
    """Runbook rule 1: never leave scdaemon holding the reader; graceful
    gpgconf kill only, even when tests fail mid-chain."""
    yield
    subprocess.run(["gpgconf", "--kill", "scdaemon"],
                   capture_output=True, timeout=30)


def _select_openpgp_sw(card_slot):
    """SELECT the OpenPGP AID by raw pyscard; return SW as hex string."""
    name = card_slot[0]
    from smartcard.System import readers
    r = [x for x in readers() if str(x) == name][0]
    conn = r.createConnection()
    try:
        conn.connect()
        _data, sw1, sw2 = conn.transmit(list(bytes.fromhex(
            f"00A40400{len(OPENPGP_AID)//2:02X}{OPENPGP_AID}")))
        return f"{sw1:02X}{sw2:02X}"
    finally:
        try:
            conn.disconnect()
        except Exception:
            pass


def test_01_gp_list_applet_selectable_and_isd_op_ready(gp):
    """Step 1: card answers GP registry dumps — APP D276* SELECTABLE and
    ISD OP_READY (precondition of the M8 cycle)."""
    rc, out = gp("--list")
    assert rc == 0, out
    assert f"APP: {OPENPGP_AID} (SELECTABLE)" in out, out
    assert f"ISD: {ISD_AID} (OP_READY)" in out, out


def test_02_pyscard_select_openpgp_aid(card_slot):
    """Step 2: raw SELECT of the OpenPGP AID over the vpcd chain -> 9000."""
    sw = _select_openpgp_sw(card_slot)
    assert sw == "9000", f"SELECT {OPENPGP_AID} returned SW {sw}"


def test_03_gp_delete_applet_then_package(gp):
    """Step 3: DESTRUCTIVE — gp --delete applet instance, then package.

    Transport failures (SW 6400, 'Could not SELECT') fail here; a
    'not found' from an earlier partial run is tolerated — step 4 is the
    authoritative absence check.
    """
    rc1, out1 = gp("--delete", OPENPGP_AID)
    rc2, out2 = gp("--delete", PKG_AID)
    combined = out1 + out2
    assert "0x6400" not in combined, f"vpcd transport failure (SW 6400): {combined}"
    assert "Could not SELECT" not in combined, combined


def test_04_deletion_verified(card_slot, gp):
    """Step 4: applet gone — SELECT answers 6A82 (file not found) and the
    GP registry shows no D276* entries."""
    sw = _select_openpgp_sw(card_slot)
    assert sw == "6A82", f"SELECT after delete returned SW {sw} (expected 6A82)"
    rc, out = gp("--list")
    assert rc == 0, out
    assert "D27600012401" not in out, out


def test_05_gp_install_cap(gp, cap_path):
    """Step 5: install the SmartPGP CAP with --bs 200 (NCI single-packet
    cap; ~4 min for the 117 KB CAP — runbook 'GP')."""
    rc, out = gp("--install", cap_path, timeout=900)
    assert rc == 0, out


def test_06_gp_list_shows_applet_back(gp):
    """Step 6: registry shows the reinstalled applet SELECTABLE."""
    rc, out = gp("--list")
    assert rc == 0, out
    assert f"APP: {OPENPGP_AID} (SELECTABLE)" in out, out


def test_07_gpg_card_status(card_slot):
    """Step 7: full host stack — gpg --card-status through the vpcd reader
    in an isolated GNUPGHOME (runbook 'GPG'), reporting the Nucula reader
    name and OpenPGP application. scdaemon killed gracefully in finally
    (runbook rule 1)."""
    name, idx, _atr = card_slot
    with tempfile.TemporaryDirectory(prefix="vpcd-gnupg-") as home:
        with open(os.path.join(home, "scdaemon.conf"), "w") as f:
            f.write("disable-ccid\n"        # pcsc backend for virtual readers
                    f"reader-port {idx}\n")  # numeric index; names with spaces don't match
        env = dict(os.environ, GNUPGHOME=home)
        try:
            p = subprocess.run(["gpg", "--card-status"], env=env,
                               capture_output=True, text=True, timeout=240)
            out = p.stdout + p.stderr
            print(out, flush=True)
            assert p.returncode == 0, out
            assert re.search(r"Reader\s+\.+:\s*" + re.escape(name), out), \
                f"reader name {name!r} not reported by gpg:\n{out}"
            assert re.search(r"Application type\s+\.+:\s*OpenPGP", out), out
        finally:
            # runbook rule 1: graceful kill only — SIGKILL wedges pcscd
            # with stale exclusive locks ("No such device" until restart)
            subprocess.run(["gpgconf", "--kill", "scdaemon"],
                           env=env, capture_output=True, timeout=30)
