#!/usr/bin/env python3
"""specter_sim_relay — bridge the Specter-DIY unix simulator to our card stack.

The specter-diy unix build stubs its smartcard HAL (`f469-disco/libs/unix/
uscard.py`) as RAW APDUs over TCP port 21111 — upstream pairs it with a
jcardsim listener. This relay answers that port with OUR card backends
(the same CardBackend family, JsonlJournal and card-safety policies as
hce_relay.py), so the wallet simulator can drive:

  - a virtual card in pcscd (vJavaCard sim reader) — zero hardware, the
    full wallet<->applet loop on a laptop;
  - a real reader (ACR/CardMan/Cherry) — policy-gated, journaled.

Wire contract (uscard.py): one TCP connection per session; `transmit()`
sends the APDU bytes raw and does a single recv(); on localhost one
send() arrives as one segment, which both sides rely on. The relay
frames the request the same way: one recv burst (bounded), one send.

Usage:
  # canned card (zero pcscd) — smoke the wallet's SELECT + pin flow:
  python3 specter_sim_relay.py --backend script --script - <<'EOF'
  00A4040006B00B5111CB01=9000
  EOF

  # virtual card behind pcscd's vJavaCard sim slot:
  python3 specter_sim_relay.py --backend pcsc \\
      --reader "vJavaCard sim 00 00" --pcsc-policy jcop-read \\
      --expect-id <CPLC-hex> --journal sim-relay.jsonl
"""
from __future__ import annotations

import argparse
import socket
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import hce_relay  # noqa: E402  (CardBackend family + policies + journal)


def build_backend(args):
    if args.backend == "pcsc":
        return hce_relay.PcscBackend(
            reader_needle=args.reader,
            policy=hce_relay.make_policy(args),
            unsafe_raw=args.unsafe_raw_pcsc)
    if args.backend == "script":
        text = sys.stdin.read() if args.script == "-" else \
            Path(args.script).read_text()
        table = hce_relay.parse_script(text.splitlines(),
                                       source=args.script)
        return hce_relay.ScriptBackend(table)
    return hce_relay.StaticBackend(bytes.fromhex(args.static_response))


class SimRelay:
    """One TCP session at a time on the uscard stub port."""

    def __init__(self, backend, journal=None):
        self.backend = backend
        self.journal = journal

    def serve_forever(self, port):
        srv = socket.socket()
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind(("127.0.0.1", port))
        srv.listen(1)
        print(f"specter_sim_relay: 127.0.0.1:{port} "
              f"<- backend {self.backend.name}", flush=True)
        while True:
            conn, _ = srv.accept()
            conn.settimeout(10)
            try:
                self.backend.connect()
                if self.journal:
                    self.journal.log("sim_session_start",
                                      **self.backend.status())
                self._serve_conn(conn)
            except hce_relay.BackendError as e:
                print(f"relay: backend: {e}", file=sys.stderr, flush=True)
            finally:
                try:
                    self.backend.disconnect()
                except Exception:
                    pass
                conn.close()
                if self.journal:
                    self.journal.log("sim_session_end")

    def _serve_conn(self, conn):
        while True:
            try:
                apdu = conn.recv(1024)
            except socket.timeout:
                continue
            if not apdu:
                return
            if self.journal:
                self.journal.log("sim_capdu", data=apdu)
            try:
                resp = self.backend.exchange(apdu)
            except (hce_relay.PolicyViolation,
                    hce_relay.BackendError) as e:
                # The wallet stub expects ISO bytes back; a denied or
                # failed exchange surfaces as 6F00 — never a socket
                # error, never a silent drop.
                if self.journal:
                    self.journal.log("sim_denied", error=str(e)[:120])
                resp = b"\x6f\x00"
            conn.sendall(resp)
            if self.journal:
                self.journal.log("sim_rapdu", data=resp)


def main() -> int:
    desc = (__doc__ or "").strip().splitlines()[0]
    ap = argparse.ArgumentParser(description=desc)
    ap.add_argument("--port", type=int, default=21111)
    ap.add_argument("--backend", choices=["pcsc", "script", "static"],
                    default="script")
    ap.add_argument("--reader", help="pcscd reader needle")
    ap.add_argument("--pcsc-policy",
                    help="card-safety policy (nt424-read / jcop-read)")
    ap.add_argument("--expect-id",
                    help="expected card identity for the policy gate")
    ap.add_argument("--unsafe-raw-pcsc", action="store_true",
                    help="waive the pcsc policy gate (owner-supervised)")
    ap.add_argument("--script", default="-",
                    help="script file for --backend script ('-' = stdin)")
    ap.add_argument("--static-response", default="9000")
    ap.add_argument("--journal")
    ap.add_argument("--log-hex", action="store_true",
                    help="journal raw hex (default: len+sha8 redaction)")
    args = ap.parse_args()
    journal = hce_relay.JsonlJournal(args.journal, args.log_hex) \
        if args.journal else None
    relay = SimRelay(build_backend(args), journal)
    try:
        relay.serve_forever(args.port)
    except KeyboardInterrupt:
        pass
    finally:
        if journal:
            journal.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
