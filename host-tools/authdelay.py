#!/usr/bin/env python3
"""Bounded NTAG424 auth-delay measurement — the no-sacrifice protocol.

Card-safety rationale (bolty-rs docs/card-safety.md §6): the failed-auth
counter RESETS on successful authentication and no permanent lockout is
observed from auth failures alone. This card is blank with KNOWN K0
(factory zeros), so every failure is guaranteed erasable by a success.

Protocol: for n in 1..3 — n deliberate wrong-key AuthEv2First (91AE),
then poll a correct-K0 auth until it succeeds; the elapsed time is the
observed delay(n). Finish with two consecutive correct auths and assert
the second is instant (reset proven). Total failed auths: 6, bounded.
"""
import binascii
import os
import sys
import time

from Crypto.Cipher import AES

ZEROS = "00000000000000000000000000000000"
WRONG = "0123456789abcdef0123456789abcdef"  # guaranteed-wrong on a blank card
POLL_S = 0.25
CAP_S = 300.0


def h2b(h): return binascii.unhexlify(h)
def b2h(b): return binascii.hexlify(b).decode()


def connect():
    from smartcard.System import readers
    rs = list(readers())
    if not rs:
        sys.exit("no reader")
    conn = rs[0].createConnection()
    conn.connect()
    print("reader:", rs[0])
    return conn


def xchg(conn, hexcmd):
    data, sw1, sw2 = conn.transmit(list(h2b(hexcmd)))
    return b2h(bytes(data)), "%02x%02x" % (sw1, sw2)


def auth(conn, key_hex):
    """Full AuthEv2First + GetCardUid proof. Returns (ok, status, t_seconds)."""
    t0 = time.monotonic()
    _, c = xchg(conn, "00A4040007D276000085010100")
    if c != "9000":
        return False, "select" + c, time.monotonic() - t0
    r, c = xchg(conn, "9071000005" + "00" + "0300000000")
    if c != "91af":
        return False, "step1-" + c, time.monotonic() - t0
    rnd_b = b2h(AES.new(h2b(key_hex), AES.MODE_ECB).decrypt(h2b(r)))
    rnd_a = b2h(os.urandom(16))
    payload = AES.new(h2b(key_hex), AES.MODE_CBC, b"\x00" * 16).encrypt(
        h2b(rnd_a + rnd_b[2:] + rnd_b[:2]))
    r2, c2 = xchg(conn, "90AF000020" + b2h(payload) + "00")
    ok = c2 == "9100"
    return ok, c2, time.monotonic() - t0


def main():
    conn = connect()

    ok, st, t = auth(conn, ZEROS)
    print(f"baseline correct-K0 auth: ok={ok} status={st} t={t:.2f}s")
    if not ok:
        sys.exit("ABORT: card is not at factory zeros — refusing to experiment")

    results = {}
    for n in (1, 2, 3):
        for i in range(n):
            ok, st, t = auth(conn, WRONG)
            print(f"  deliberate fail {i+1}/{n}: status={st} t={t:.2f}s")

        t0 = time.monotonic()
        attempts = 0
        status_seen = set()
        while time.monotonic() - t0 < CAP_S:
            attempts += 1
            ok, st, t = auth(conn, ZEROS)
            status_seen.add(st)
            if ok:
                elapsed = time.monotonic() - t0
                results[n] = elapsed
                print(f"delay(n={n}): recovered after {elapsed:.2f}s "
                      f"({attempts} poll-auths, statuses={sorted(status_seen)})")
                break
            time.sleep(POLL_S)
        else:
            print(f"delay(n={n}): NOT recovered within {CAP_S}s — stopping experiment")
            break

    ok, st, t = auth(conn, ZEROS)
    print(f"reset-proof immediate re-auth: ok={ok} status={st} t={t:.2f}s "
          f"({'RESET CONFIRMED' if ok and t < 2 else 'check!'})")

    print("\n===== CURVE =====")
    for n, d in results.items():
        print(f"  {n} failed auth(s) -> {d:.2f}s effective delay")
    conn.disconnect()


if __name__ == "__main__":
    main()
