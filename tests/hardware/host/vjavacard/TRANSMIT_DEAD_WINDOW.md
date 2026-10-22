# The ~2 min post-pcscd-restart transmit-dead window (root cause + mitigation)

2026-10-22, card `t_57a0ffe2` follow-up lane. Phenomenon first pinned in the
GP-lifecycle handoff (`/root/handoffs/vjavacard-gp-lifecycle-2026-10-22.md`),
raw capture `/root/handoffs/evidence/vjavacard-gp-20261022/bridge-strace.txt`
(strace of the bridge process, filtered to `read`/`write` — no connect/close
lines, fd 9 = the bridge's socket to the driver's vpcd listener).

## Phenomenon

After a pcscd restart, the vJavaCard vpcd slot answers the ATR probe
immediately, but host C-APDU transmits (`SCardTransmit` via pyscard / gp)
return empty ("no valid response") for **~2 minutes**, then self-heal.
Observed twice in the lifecycle lane (once aggravated by a second bridge
racing onto the listener).

## What the strace proves

The 152-line capture covers the window. Reading it cycle by cycle:

1. **The transport is bidirectionally alive for probes.** The driver sends
   1-byte `RESET` (0x04) controls (`read(9, "\x00\x01", 2)` +
   `read(9, "\x04", 1)`); the bridge answers the 12-byte ATR on the same
   socket within the same syscall burst. Dozens of these cycles appear —
   every probe answered, zero failures. So "ATR answers immediately" is
   literally visible: driver→bridge and bridge→driver both work.

2. **Host transmits never reach the bridge.** In the entire capture exactly
   ONE C-APDU arrives (`00 a4 04 00 0a <echo AID>`, lines 86–87). The empty
   transmits the host saw are therefore NOT the bridge failing to answer —
   the C-APDUs are not being delivered to the vpcd socket at all during the
   window. The stall lives on the **pcscd / libifdvpcd side**: the reader
   state machine reports card-present (it owns the ATR exchange) while the
   transmit path does not route APDUs to the VICC socket yet.

3. **The bridge used to churn through the whole window.** After each
   answered probe, the bridge's next blocking read fails with
   `EAGAIN` = the 5 s `SO_TIMEOUT` firing (lines 6, 12, 18, …). The old
   `readFully` let that `SocketTimeoutException` propagate: `serve()`
   aborted, main() closed the socket, slept 500 ms and reconnected. The
   capture's endless probe/timeout cycles plus the recurring
   `vjavacard: connected to 127.0.0.1:35973` stdout writes are that
   connect→probe→timeout→reconnect loop. The bridge was never a stable
   VICC during the window — it re-arrived every ~5.5 s, re-triggering the
   single-client driver's VICC-arrival logic on every cycle and giving any
   late-delivered transmit a moving target.

## Root cause (split)

- **Host-side (the window itself — needs the pcscd-host-stack lane):**
  pcscd restart leaves the driver able to probe the VICC (card-present
  path) while the transmit path stalls for ~2 min. Something in the
  restart path — reader hotplug/rescan racing the driver's per-reader
  state, or pcscd's socket-activation respawn recreating reader contexts —
  defers APDU routing while probes continue. This is inside pcscd +
  libifdvpcd and is NOT observable from the bridge beyond fact (2) above.
  Recommended bounded follow-up for that lane: reproduce with pcscd debug
  logging (`PCSCD_DEBUG`/`-d`) plus an strace of pcscd's driver side,
  watch where SCardTransmit's APDU stops between pcscd and the driver's
  `write()` to the VICC socket, and check whether the patched
  libifdvpcd's reader-reinit on VICC (re)connect serializes against
  pcscd's rescan. Candidate driver-side fix: on VICC reconnect, force a
  full IFD re-init instead of resuming probe state.
- **Bridge-side aggravator (fixed here):** the 5 s idle-timeout reconnect
  churn described in (3).

## Mitigation implemented (bridge-side, this repo)

`VpcdBridge.readFully` now treats `SocketTimeoutException` as "no message
yet" and keeps waiting on the same connection. A quiet socket is no longer
a reason to hang up: like a real card, the bridge stays attached while
silent and only reconnects on a real transport death (EOF/RST — e.g. an
actual pcscd restart killing the listener). Effect during the window: the
driver sees ONE VICC connection instead of ~20 churn events; a transmit
that pcscd finally delivers lands on a live, stable session. This cannot
fix the host-side stall itself — it removes the bridge's contribution and
the loss surface mid-churn. Both bridge modes re-verified green after the
change (8 passed / 2 mode-skips each).

## Operational notes (single-client slot)

- **0x8C85 is single-client.** One bridge process at a time; two bridges
  racing onto the listener wedge the slot (lifecycle lane observed it).
- **Don't TCP-probe the vpcd port while a bridge owns it.** The driver's
  single accept slot has a backlog of 1: a manual `connect()` (e.g. a
  readiness probe) queues as a second client; a *second* prober then times
  out — and the suite's own `_bridge_ready()` gate false-skips everything
  (an all-skip run is a false green; CI greps for `N passed`). If it
  happens: restart the bridge (kill by exact pid, relaunch) — the driver
  drains its accept state on VICC loss — then run pytest as the first
  thing that touches the port. Observed live on 2026-10-22: manual probe +
  pytest probe = 15/15 connect timeouts, 10 skips; after a bridge restart
  the same suite passed in 2.9 s.
- After any pcscd restart, do not fail-fast on the slot: ATR works
  immediately, transmits may not for ~2 min (see the test module
  docstring and the `sleep 60` absorb in `.github/workflows/virtual-card.yml`).
