#!/usr/bin/env python3
"""Golden-trace capture via a reference reader (ACR1252/OmniKey).

Records ATR + APDU exchanges as JSONL for diffing custom-reader behavior
against commercial readers (owner request 2026-10-28). Read-only by
default; --script runs a JSON list of APDU hex strings.
Usage: golden_trace.py "ACR1252" [--script script.json] [--out trace.jsonl]
"""
import sys, json, time, argparse

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("reader_substr")
    ap.add_argument("--script", default=None, help="JSON file: list of APDU hex strings")
    ap.add_argument("--out", default="-")
    args = ap.parse_args()
    from smartcard.System import readers
    from smartcard.Exceptions import NoCardException
    r = next((x for x in readers() if args.reader_substr.lower() in str(x).lower()), None)
    if not r:
        print("reader not found:", args.reader_substr); sys.exit(2)
    out = sys.stdout if args.out == "-" else open(args.out, "a")
    def rec(**kw):
        kw["t"] = round(time.time(), 3); out.write(json.dumps(kw) + "\n"); out.flush()
    try:
        c = r.createConnection(); c.connect()
        rec(ev="connect", reader=str(r), atr=' '.join(f'{b:02x}' for b in c.getATR()))
    except NoCardException:
        rec(ev="nocard", reader=str(r)); sys.exit(1)
    apdus = []
    if args.script:
        apdus = json.load(open(args.script))
    else:
        apdus = ["00a4040007d276000085010100", "00ca006e00"]  # NDEF AID + GET DATA
    for hx in apdus:
        a = bytes.fromhex(hx)
        t0 = time.time()
        try:
            d, s1, s2 = c.transmit(list(a), c.getProtocol())
            rec(ev="apdu", cmd=hx, sw=f"{s1:02x}{s2:02x}", resp=bytes(d).hex(), ms=round((time.time()-t0)*1000, 1))
        except Exception as e:
            rec(ev="error", cmd=hx, err=type(e).__name__)
    c.disconnect()
    rec(ev="disconnect")

if __name__ == "__main__":
    main()
