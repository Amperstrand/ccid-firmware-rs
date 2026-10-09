"""NFC reader differential HIL suite — the same card through every bench
reader, sequentially (one reader connected at a time; shared 13.56 MHz field).

Covers (per reader): pcscd enumeration, card presence, ATR capture, AID
app discovery (read-only SELECTs), a read-only APDU exchange, and — when
two or more readers see the card — cross-reader ATR/response comparison
against the ACR1252 reference.

Run:
    pytest tests/hardware/nfc/ -v --hil
    pytest tests/hardware/nfc/ -v --hil --readers nucula,acr1252
"""

import time

import pytest

from conftest import AID_PROBES, select_aid

pytestmark = pytest.mark.hil


def test_reader_enumerates(reader_key):
    """Each reader family is present in pcscd (hardware + driver chain)."""
    from conftest import find_reader
    assert find_reader(reader_key) is not None, f"{reader_key} missing from pcscd"


def test_card_atr(card_session, record_property):
    """Card detected + powered; ATR captured and sane (TS 0x3B/0x3F, ≥6 bytes)."""
    atr = card_session["atr"]
    record_property("reader", card_session["key"])
    record_property("atr", atr.hex().upper())
    assert len(atr) >= 6, f"ATR too short: {atr.hex()}"
    assert atr[0] in (0x3B, 0x3F), f"bad TS byte: {atr.hex()}"


def test_aid_discovery(card_session, record_property):
    """SELECT each known AID (read-only) — record which apps the card carries."""
    conn = card_session["conn"]
    found = {}
    for name, aid in AID_PROBES.items():
        try:
            _, sw = select_aid(conn, aid)
            found[name] = sw
        except Exception as e:
            found[name] = f"ERR:{type(e).__name__}"
        time.sleep(0.1)
    record_property("aids", str(found))
    # A JavaCard must answer at least one probe cleanly (9000 or 6A82-app-absent)
    clean = [n for n, sw in found.items() if sw in (0x9000, 0x6A82)]
    assert clean, f"no clean AID responses: {found}"


def test_readonly_apdu_exchange(card_session):
    """One ISO-DEP round trip beyond SELECT: the selected app's GET DATA /
    status must return a valid status word (proves APDU relay both ways)."""
    conn = card_session["conn"]
    # try the apps most likely present; only those that SELECTed 9000
    candidates = []
    for name, aid in AID_PROBES.items():
        try:
            _, sw = select_aid(conn, aid)
            if sw == 0x9000:
                candidates.append(name)
                break
        except Exception:
            continue
    assert candidates, "no app selected; cannot run APDU exchange"
    app = candidates[0]
    # read-only follow-ups per app (no state changes)
    if app == "OpenPGP":
        apdu = bytes.fromhex("00CA004F00")        # GET DATA: AID
    elif app in ("Satochip", "SeedKeeper"):
        apdu = bytes.fromhex("00B0000000")        # GET STATUS / read
    elif app.startswith("FIDO"):
        apdu = bytes.fromhex("00A40400085015312E")  # applet info select
    else:
        apdu = bytes.fromhex("00CA006600")        # PIV/PPSE discovery
    data, sw1, sw2 = conn.transmit(list(apdu))
    sw = (sw1 << 8) | sw2
    assert sw in (0x9000, 0x6282, 0x6A86, 0x6E00), \
        f"{app}: unexpected SW {sw:04X} for {apdu.hex()}"


# --- cross-reader differential -------------------------------------------

ATR_CACHE = {}   # filled by the ATR test; module-level so the
                 # differential test can compare across the parametrized runs


def test_atr_capture_for_differential(card_session):
    ATR_CACHE[card_session["key"]] = card_session["atr"]


def test_atr_agreement_across_readers():
    """If 2+ readers saw the card, their ATRs must match EXACTLY (the ACR1252
    is the reference implementation)."""
    if len(ATR_CACHE) < 2:
        pytest.skip(f"only {len(ATR_CACHE)} reader(s) detected the card")
    atrs = {k: v.hex() for k, v in ATR_CACHE.items()}
    unique = set(atrs.values())
    assert len(unique) == 1, f"ATR divergence across readers: {atrs}"
