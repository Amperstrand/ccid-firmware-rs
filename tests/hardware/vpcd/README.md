# vpcd-over-WiFi hardware e2e suite

Pytest suite for the **reversed** vpcd path
(`docs/VPCD_WIFI_RUNBOOK.md`):

```
gpg / scdaemon ─ pcscd ─ libifdvpcd.so (SERVER, listens :35963)
                                ▲
                                │ TCP (board connects OUT)
                                ▁▼▁
           nucula wifi-vpcd firmware ─ PN7160 ─ RF ─ SmartPGP card
```

Runs with **plain pytest** — no agent, no CI. The whole directory is
skipped unless `NUCULA_VPCD_E2E=1`, so a bare `pytest` elsewhere is safe.

## Run it

```sh
NUCULA_VPCD_E2E=1 \
NUCULA_GP_KEY_ENC=<hex> NUCULA_GP_KEY_MAC=<hex> NUCULA_GP_KEY_DEK=<hex> \
python3 -m pytest tests/hardware/vpcd/ -v -s
```

Optional additions:

```sh
NUCULA_OTA_BIN=~/.cargo-target/esp32-ccid-wifi-vpcd.bin   # re-enable the OTA test
NUCULA_BOARD_IP=192.168.13.230    # target board (default: bench nucula)
NUCULA_VPCD_PORT=35963            # its vpcd listener slot (00 00)
NUCULA_EXPECT_CARD=0              # bench with an empty reader
NUCULA_SMARTPGP_CAP=<path>        # override the bench CAP location
```

Needs on the host: `python3` (3.12), `pytest`, `pyscard`, `gp`
(GlobalPlatformPro), `gpg`/`gpgconf`, `sudo` (only for the pcscd restart
recovery helper). ~2 s/APDU over this transport — the full run including
the ~4 min CAP install takes roughly 8–10 minutes.

## Files

| File | Covers |
|---|---|
| `conftest.py` | global `NUCULA_VPCD_E2E` gate; board endpoints; **real-SCardConnect** reader discovery (runbook rule 2); GP key env fixtures; netlog listener; runbook pcscd restart recovery helper |
| `test_link.py` | board pingable; OTA TCP :3232 accepts; netlog UDP :4567 receives a WARN from the board; vpcd reader slot present; card ATR `3B85…` over the full chain |
| `test_ota.py` | **opt-in** (`NUCULA_OTA_BIN`) reflash via `ota_push.py`; waits ≤60 s for the board to come back (OTA port + vpcd ESTAB), with one runbook pcscd restart to clear a stale half-open session |
| `test_smartpgp_lifecycle.py` | **DESTRUCTIVE** full GP cycle: `gp --list` (APP SELECTABLE + ISD OP_READY) → raw SELECT 9000 → `gp --delete` applet+package → SELECT 6A82 + registry empty → `gp --install --bs 200` → `gp --list` back → `gpg --card-status` reports the Nucula reader + OpenPGP app |
| `ota_push.py` | standalone OTA client (`"OTA1"` + u32-LE size + image stream, per `firmware/esp32-ccid/src/ota.rs`); vpcd-aware liveness check — copied from the bench host-tools client because the repo has none |

## Bench prerequisites

- pcscd with the stock vpcd driver in **server** mode (reader.conf with
  `libifdvpcd.so`, listeners :35963/:35964) — runbook "Host setup".
- ufw: `35963/tcp`, `35964/tcp`, `4567/udp` allowed. A DROP policy looks
  exactly like a firmware hang (runbook "Host setup" #3).
- **Wall power (5 V/2 A) for the nucula** — computer USB couples
  marginally and the card drops under load (runbook "Power").
- SmartPGP bench dev card on the reader (ATR `3B8581…`) for
  `test_link.py::test_card_atr` and the lifecycle module.
- GP keys + CAP path via env — **never hardcoded, never committed**
  (runbook "GP"; caps live outside the tree).

## Operational rules the tests encode

From `docs/VPCD_WIFI_RUNBOOK.md` ("each one cost an hour"):

1. **Never `pkill -9 scdaemon`** — SIGKILL leaves a stale exclusive lock
   in pcscd (`SCARD_E_SHARING_VIOLATION` → "No such device" until
   restart). The lifecycle module kills scdaemon only via
   `gpgconf --kill scdaemon`, in `finally` blocks.
2. **Trust real `SCardConnect`, not `GetStatusChange`** — after pcscd
   restarts the cached presence lies. All reader discovery here
   (`conftest.real_connect`, `slot_map`) attempts a real connect.
3. Two driver slots share one board connection; the live slot is decided
   per pcscd restart — discovery, never assumption.
4. gp selects readers **by name** (`--reader "Nucula VPCD 00 00"`); Java
   SmartcardIO indices ≠ pyscard order.
5. gp needs **`--bs 200`**: NCI DATA packets carry a 1-byte length;
   APDUs > ~245 B fail SW 6400. Full-size STORE DATA is broken today.
6. gpg uses a temp `GNUPGHOME` with `scdaemon.conf` =
   `disable-ccid` + `reader-port <numeric index>`; a full `--card-status`
   takes ~40 s over this transport.
7. pcscd restart (recovery only, never routine):
   `stop pcscd.service pcscd.socket` → `rm -f /run/pcscd/pcscd.comm` →
   `start pcscd.socket` → sleep 1 → `start pcscd.service`.
   Encoded in the `restart_pcscd` fixture.

## Bench anomalies observed while proving this suite (2026-10-13/14)

These are failure modes the runbook's "Known limitations" hints at,
made concrete — all recovered without serial access except the last:

- **Phantom ATR + SW 6400 on every APDU**: power-on answers from the
  PN7160 driver's cached auto-ATS while the RF session is dead; every
  `transmit_apdu` errors and vpcd answers 64 00 (session survives, card
  unreachable). A pcscd restart does NOT fix it — the wedge is
  board-side (the driver keeps its session across vpcd reconnects).
  Recovery: same-image OTA reflash (board reboot) or USB-serial reset.
- **Stale half-open vpcd ESTAB after an OTA reboot**: pcscd's
  single-accept driver still holds the pre-reboot socket; the board's
  new outbound connection is never served. Recovery: runbook pcscd
  restart (the `restart_pcscd` fixture does this in `test_ota.py`).
- **Board dark on WiFi after a second OTA flash** (ARP INCOMPLETE, no
  netlog, OTA dead): the WiFi stack died post-boot. USB serial
  `/dev/ttyACM0` (Espressif JTAG) with an explicit RTS/DTR reset pulse
  revived it; a plain port open was not enough. Serial is the recovery
  path of last resort (runbook "Firmware build + flash").
- **Silent netlog on a healthy board**: netlog broadcasts WARN+ only —
  "no lines" is normal. `test_link.py` triggers one deterministic WARN
  via the OTA bad-magic error path instead of waiting passively.
