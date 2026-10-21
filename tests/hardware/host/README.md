# Host-side libifdvpcd patch (issue #326 — wifi-vpcd host half)

Status: bench-proven (2026-10-21). Deployed on ai-legion for the
`vpcd-nucula-35967` slot only; the boltcard vpcd slots keep the stock
library.

## The two upstream bugs

Upstream: frankmorgner/vsmartcard, ifd-vpcd/vpcd (checked out at
`93286a90d2411120ae17299f9902e1fdd8954185`; the installed Ubuntu
`vsmartcard-vpcd` 3.3 package behaves identically).

1. **Empty-ATR answer tears down the connection.** `recvFromVICC`
   calls `recvall(sock, buf, 0)` for a zero-length answer, which
   returns 0; `vicc_transmit` treats `r <= 0` as a transport error and
   calls `vicc_eject()`. But a zero-length ATR is the vpcd protocol's
   explicit "no card" signal — exactly what a firmware answers while
   its parked discovery re-arm warms up. Result: every early presence
   probe kills the TCP connection, and the 0 ms re-accept window in
   `vicc_present` keeps missing the firmware's reconnect, so pcscd
   never sees the card. Wire evidence: probe bursts at enumeration,
   silence after, while the firmware console proves
   `ATR req → present, power_on ok (12 B)` was answered every time.
2. **Unbounded receive + backlog-0 listener = permanent wedge.**
   `recvall` blocks forever on an unanswered probe (firmware rebooting
   mid-conversation), wedging pcscd's EH thread; because the listener
   runs with `listen(sock, 0)`, every subsequent firmware SYN is
   dropped until pcscd restarts. Reproduced: SCardConnect hung 60 s
   with 1 packet out / 0 in, firmware console showing
   `connect failed (connection timed out) — retrying`.

The patch (`libifdvpcd-amperstrand.patch`): empty answers return as
valid, eject only on hard errors (`r < 0`), and every vpicc socket
gets `SO_RCVTIMEO = 2 s`.

## Build / deploy / restore

```
cd tests/hardware/host
./build_libifdvpcd_amperstrand.sh build
./build_libifdvpcd_amperstrand.sh deploy [/etc/reader.conf.d/vpcd-nucula-35967]
# restart pcscd (under the bench flock)
./build_libifdvpcd_amperstrand.sh restore   # back to stock
```

`VPCDSLOTS=2` matches the packaged build — 8 slots (upstream default)
would rename the reader set to `00 00..00 07` and churn every
pinned-reader script.

## Bench result (2026-10-21)

```
WIFI E2E: CONNECTED (attempt 0), ATR=3b858131fe458073c8211084
```

First card-present + ATR + APDU round-trip over the wifi-vpcd path on
this bench. Full evidence chain: issue #326.

## Open items

- APDU response echoed the ATR once (response pairing in the
  ON→ATR→APDU sequence) — pin with a wire capture on the next wifi
  bench window (#326).
- Post-disconnect re-detection decays (EH poll cadence vs firmware
  parked re-arm) — expected to be closed by the time-based re-arm
  rework (#349/#350 line).
- Upstream the patch to frankmorgner/vsmartcard — both bugs reproduce
  with the stock driver and any slow or reconnecting vpicc.
