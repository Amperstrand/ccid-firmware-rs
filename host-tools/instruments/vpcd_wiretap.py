#!/usr/bin/env python3
"""vpcd wiretap — canonical instrument for wifi-vpcd board sessions.

Speaks the REAL protocol (2-byte BE length prefix each direction — the
framing a hand-probe got wrong on 2026-10-28) and logs every exchange
as JSONL: {t, dir, n, hex}. Two modes:

  tap     : accept the board, log passively, drive optional ATR probes
  relay   : accept board on --listen, forward to a real listener on
            --upstream (pcscd re-pointed there), log BOTH directions

Use during instrumented windows (pcscd stopped for 'tap'; relay needs a
reader.conf port change). Restore pcscd via pcscd-ctl afterwards.
"""
import socket, struct, time, argparse, json, sys

def frame(sock, payload):
    sock.sendall(struct.pack(">H", len(payload)) + payload)

def read_frame(sock, timeout=10.0):
    sock.settimeout(timeout)
    def exact(n):
        buf = b""
        while len(buf) < n:
            c = sock.recv(n - len(buf))
            if not c:
                raise EOFError("peer closed")
            buf += c
        return buf
    (n,) = struct.unpack(">H", exact(2))
    return exact(n) if n else b""

def log(out, d, tag, data):
    rec = {"t": round(time.time(), 3), "dir": d, "tag": tag, "n": len(data), "hex": data.hex()}
    out.write(json.dumps(rec) + "\n"); out.flush()
    print(f"{time.strftime('%H:%M:%S')} {d:6s} {tag:8s} {len(data):4d}B {data[:24].hex()}{'...' if len(data)>24 else ''}", flush=True)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--listen-port", type=int, default=35967)
    ap.add_argument("--listen-ip", default="192.168.13.208")
    ap.add_argument("--upstream-port", type=int, default=0, help="relay mode: forward here")
    ap.add_argument("--probes", type=int, default=0, help="ATR ctrl probes (tap mode)")
    ap.add_argument("--probe-gap", type=float, default=2.0)
    ap.add_argument("--out", default="-")
    args = ap.parse_args()

    out = sys.stdout if args.out == "-" else open(args.out, "a")
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((args.listen_ip, args.listen_port)); srv.listen(1)
    srv.settimeout(90)
    print(f"listening {args.listen_ip}:{args.listen_port} (board reconnects <=3s)", flush=True)
    conn, addr = srv.accept()
    print("board:", addr, flush=True)
    log(out, "rx", "connect", b"")

    up = None
    if args.upstream_port:
        up = socket.create_connection(("127.0.0.1", args.upstream_port))

    conn.settimeout(10.0)
    t0 = time.time()
    deadline = t0 + max(300, args.probes * args.probe_gap + 30)
    probe_i = 0
    next_probe = t0 + 1.0
    while time.time() < deadline:
        if args.probes and probe_i < args.probes and time.time() >= next_probe:
            try:
                frame(conn, b"\x04"); log(out, "tx", "ctrl-atr", b"\x04")
                a = read_frame(conn, timeout=12.0)
                log(out, "rx", "atr", a)
            except (socket.timeout, EOFError, ConnectionResetError) as e:
                log(out, "err", type(e).__name__, str(e).encode()[:64])
                if isinstance(e, (EOFError, ConnectionResetError)):
                    log(out, "rx", "reset", b""); break
            probe_i += 1
            next_probe = time.time() + args.probe_gap
        try:
            conn.settimeout(0.5)
            d = conn.recv(4096)
            if not d:
                log(out, "rx", "eof", b""); break
            log(out, "rx", "data", d)
            if up:
                up.sendall(d)
        except socket.timeout:
            pass
        except (ConnectionResetError, OSError):
            log(out, "rx", "reset", b""); break
        if up:
            try:
                up.settimeout(0.1)
                d = up.recv(4096)
                if d:
                    log(out, "tx", "host", d); conn.sendall(d)
            except socket.timeout:
                pass
    print("session end", flush=True)

if __name__ == "__main__":
    main()
