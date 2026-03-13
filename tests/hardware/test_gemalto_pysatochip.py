#!/usr/bin/env python3
"""
Verify secure channel, unlock, and read secret on SeedKeeper using pysatochip with
the Gemalto reader. Use this to confirm the full flow works on a reference reader
before debugging the STM32 CCID reader.

Requires: pcscd running, pysatochip and pyscard installed, SeedKeeper in Gemalto reader.
Usage:
  python3 test_gemalto_pysatochip.py              # use Gemalto reader (patch), PIN 1234
  python3 test_gemalto_pysatochip.py 1234         # PIN 1234
  python3 test_gemalto_pysatochip.py --no-patch   # use first available reader (any)
  python3 test_gemalto_pysatochip.py --log-file /path/to/log.txt  # write step log to file

If Gemalto is not found, the script exits. With --no-patch, the first reader that
reports a card will be used (ensure only Gemalto has the card if you want Gemalto).

Important: The script removes the CardMonitor observer after CardConnector init so the
observer cannot call card_select() after we initiate the secure channel (SELECT resets
the secure channel on the card and would cause 0x9C23 wrong MAC).
"""
from __future__ import print_function
import sys
import time
from datetime import datetime, timezone

# Collect step log for LEARNINGS (no secrets)
STEP_LOG = []

def _log_step(msg, detail=None):
    STEP_LOG.append((msg, detail))
    if detail:
        print("{} | {}".format(msg, detail))
    else:
        print(msg)

def _force_gemalto_reader(reader_filter="Gemalto"):
    """Patch CardRequest to use only the matched reader so CardConnector connects to it."""
    from smartcard.System import readers
    rlist = [r for r in readers() if reader_filter in str(r)]
    if not rlist:
        print("Reader matching '{}' not found. Available:".format(reader_filter),
              [str(r) for r in readers()], file=sys.stderr)
        sys.exit(1)
    gemalto_name = str(rlist[0])
    from smartcard.CardRequest import CardRequest
    _orig = CardRequest.__init__

    def _patched_init(self, timeout=0, cardType=None, readers=None, **kwargs):
        if readers is None:
            readers = [gemalto_name]
        _orig(self, timeout=timeout, cardType=cardType, readers=readers, **kwargs)

    CardRequest.__init__ = _patched_init
    return gemalto_name

def main():
    pin = "1234"
    do_patch = True
    log_path = None
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if "--no-patch" in sys.argv:
        do_patch = False
    if "--log-file" in sys.argv:
        i = sys.argv.index("--log-file")
        if i + 1 < len(sys.argv) and not sys.argv[i + 1].startswith("--"):
            log_path = sys.argv[i + 1]
            if sys.argv[i + 1] in args:
                args.remove(sys.argv[i + 1])
    if args:
        pin = args[0]

    reader_filter = "Gemalto"
    if "--reader" in sys.argv:
        ri = sys.argv.index("--reader")
        if ri + 1 < len(sys.argv):
            reader_filter = sys.argv[ri + 1]

    _log_step("START " + datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ") + " PIN=****")

    if do_patch:
        reader_name = _force_gemalto_reader(reader_filter)
        _log_step("Reader (forced)", reader_name)
    else:
        _log_step("Reader", "first with card (--no-patch)")

    from smartcard.Exceptions import CardRequestTimeoutException
    from pysatochip.CardConnector import CardConnector

    try:
        cc = CardConnector(client=None, card_filter=["seedkeeper"])
    except CardRequestTimeoutException:
        _log_step("FAILED", "CardConnector: no card. Ensure SeedKeeper is in Gemalto.")
        if log_path:
            _write_log(log_path, success=False)
        sys.exit(1)

    # Kill observer so it cannot call card_select() after we initiate secure channel
    # (SELECT resets initialized_secure_channel on the card -> wrong MAC 0x9C23).
    cc.cardmonitor.deleteObserver(cc.cardobserver)
    time.sleep(0.5)

    if not cc.card_present or not cc.cardservice:
        _log_step("FAILED", "No card detected.")
        if log_path:
            _write_log(log_path, success=False)
        sys.exit(1)

    # Ensure T0|T1 protocol (observer/waitforcard connection may have wrong protocol on some hosts).
    from smartcard.System import readers
    from smartcard.scard import SCARD_PROTOCOL_T0, SCARD_PROTOCOL_T1
    rlist = [r for r in readers() if reader_filter in str(r)]
    if not rlist:
        _log_step("FAILED", "Reader matching '{}' not found.".format(reader_filter))
        if log_path:
            _write_log(log_path, success=False)
        sys.exit(1)
    try:
        gemalto_conn = rlist[0].createConnection()
        gemalto_conn.connect(SCARD_PROTOCOL_T0 | SCARD_PROTOCOL_T1)
    except Exception as e:
        _log_step("FAILED", "Connect T0|T1 failed: {}.".format(e))
        if log_path:
            _write_log(log_path, success=False)
        sys.exit(1)
    try:
        orig = cc.cardservice.connection
        orig.disconnect()
        orig.release()
    except Exception:
        pass
    cc.cardservice.connection = gemalto_conn

    # Use this connection for ATR and all following steps.
    conn = cc.cardservice.connection

    try:
        try:
            atr = bytes(conn.getATR())
            _log_step("ATR", atr.hex().upper() + " ({} bytes)".format(len(atr)))
        except Exception as e:
            _log_step("ATR", "get failed: {}".format(e))

        # Only run SELECT + GET_STATUS + initiate_secure_channel if observer did not
        # already do it. Re-calling card_select() would reset the secure channel on the card.
        observer_did_sc = getattr(cc, "sc", None) is not None and getattr(cc, "needs_secure_channel", False)

        if observer_did_sc:
            _log_step("[1] SELECT/GET_STATUS/SC", "already done by observer, skip")
        else:
            _log_step("[1] SELECT SeedKeeper", "...")
            cc.card_select()
            _log_step("[1] SELECT SeedKeeper", "OK")

            _log_step("[2] GET_STATUS", "...")
            cc.card_get_status()
            _log_step("[2] GET_STATUS", "needs_secure_channel={}".format(getattr(cc, "needs_secure_channel", None)))

            if getattr(cc, "needs_secure_channel", None):
                _log_step("[3] Initiate secure channel", "...")
                cc.card_initiate_secure_channel()
                _log_step("[3] Initiate secure channel", "OK")
            else:
                _log_step("[3] Secure channel", "not required, skip")

        _log_step("[4] VERIFY_PIN", "...")
        cc.card_verify_PIN_simple(pin)
        _log_step("[4] VERIFY_PIN", "OK")

        _log_step("[5] SeedKeeper GET_STATUS", "...")
        _, _, _, status = cc.seedkeeper_get_status()
        _log_step("[5] GET_STATUS", "nb_secrets={} total_memory={} free_memory={}".format(
            status.get("nb_secrets"), status.get("total_memory"), status.get("free_memory")))

        _log_step("[6] LIST_SECRETS", "...")
        headers = cc.seedkeeper_list_secret_headers()
        _log_step("[6] LIST_SECRETS", "Found {} secret(s)".format(len(headers)))

        if not headers:
            _log_step("EXPORT", "No secrets to export.")
            cc.card_disconnect()
            _log_step("DONE", "Secure channel + unlock OK; no secrets.")
            if log_path:
                _write_log(log_path, success=True)
            return

        sid = headers[0].get("id")
        if sid is None:
            sid = headers[0].get("secret_id", 0)
        stype_h = headers[0].get("type", "")
        label_h = headers[0].get("label", "")
        _log_step("[7] EXPORT first secret", "id={} type=0x{:02X} label={!r}".format(
            sid, stype_h if isinstance(stype_h, int) else 0, label_h))
        secret = cc.seedkeeper_export_secret(sid)
        label = secret.get("label", "")
        stype = secret.get("secret_type", "")
        _log_step("[7] EXPORT", "type={} label={!r}".format(stype, label))
        if secret.get("full_data"):
            hex_data = secret["full_data"]
            _log_step("SECRET_HEX_LEN", "{} chars (not logged)".format(len(hex_data)))

        cc.card_disconnect()
        _log_step("KNOWN_GOOD", "Secure channel + unlock + read secret OK on Gemalto with pysatochip.")
        if log_path:
            _write_log(log_path, success=True)

    except Exception as e:
        _log_step("FAILED", str(e))
        import traceback
        traceback.print_exc()
        if log_path:
            _write_log(log_path, success=False)
        sys.exit(1)


def _write_log(path, success):
    with open(path, "w") as f:
        f.write("known_good={}\n".format(str(success).lower()))
        f.write("timestamp={}\n".format(datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")))
        for msg, detail in STEP_LOG:
            if detail:
                f.write("{} | {}\n".format(msg, detail))
            else:
                f.write("{}\n".format(msg))

if __name__ == "__main__":
    main()
