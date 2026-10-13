# vpcd over WiFi — bench runbook

Bench-verified 2026-10-13 (nucula ESP32-C3 + PN7160, host = the lab
ai-legion box). Everything below ran end-to-end: ATR, 7,914-read overnight
soak, GPG `--card-status` ×3, and full GP uninstall/install of SmartPGP.

## Architecture — reversed connection

```
gpg / scdaemon ─ pcscd ─ libifdvpcd.so (SERVER, listens :35963)
                                   ▲
                                   │ TCP (board connects OUT)
                                   ▁▼▁
              nucula wifi-vpcd firmware ─ PN7160 ─ RF ─ card
```

The stock `libifdvpcd.so` **client** mode (driver connects out) crashes
pcscd with a glibc `FD_SETSIZE` abort on this system; **server** mode is
the only stable configuration. The firmware therefore runs the vpcd
protocol as a *client* that connects out to the host listener and
auto-reconnects every 3 s — same pattern as vsmartcard's remote-reader
Android app. No socat, no PTY bridge, no watchdog.

Wire format (vsmartcard `src/vpcd/vpcd.c`): every message = 2-byte
big-endian length + payload. Control payloads are exactly 1 byte:
`0x00` OFF, `0x01` ON, `0x02` RESET, `0x04` ATR. A 0-length response to
ATR/RESET means "ICC absent" (the driver ejects + re-polls); APDU
failures answer SW `64 00` so the session survives transient RF drops.

## Host setup (one-time)

```sh
# 1. reader config — stock vpcd driver, server mode
sudo tee /etc/reader.conf.d/vpcd <<'EOF'
FRIENDLYNAME "Nucula VPCD"
DEVICENAME /dev/null:0x8C7B
LIBPATH /usr/lib/pcsc/drivers/serial/libifdvpcd.so
CHANNELID 0x8C7B
EOF

# 2. keep pcscd (and the :35963 listener) alive without clients,
#    and raise its fd limit
sudo mkdir -p /etc/systemd/system/pcscd.service.d
sudo tee /etc/systemd/system/pcscd.service.d/no-auto-exit.conf <<'EOF'
[Service]
ExecStart=
ExecStart=/usr/sbin/pcscd --foreground
EOF
sudo tee /etc/systemd/system/pcscd.service.d/fdlimit.conf <<'EOF'
[Service]
LimitNOFILE=1024:65536
EOF

# 3. firewall: the board's inbound vpcd SYN + netlog UDP.
#    A ufw INPUT policy DROP here looks EXACTLY like a firmware hang.
sudo ufw allow 35963/tcp comment "vpcd nucula"
sudo ufw allow 4567/udp comment "netlog bench"
```

pcscd restart sequence that works (order matters — see "Operational
rules"):

```sh
sudo systemctl stop pcscd.service pcscd.socket
sudo rm -f /run/pcscd/pcscd.comm
sudo systemctl start pcscd.socket && sleep 1
sudo systemctl start pcscd.service
```

## Firmware build + flash

```sh
cd firmware/esp32-ccid
NUCULA_WIFI_SSID=<ssid> NUCULA_WIFI_PASS=<pass> \
NUCULA_VPCD_HOST=<host-ip> ./build.sh wifi-vpcd          # nucula (slot :35963)
NUCULA_VPCD_PORT=35964 ./build.sh m5stick-vpcd \
    --flash /dev/ttyUSB0                                  # m5stick (slot :35964)
python3 host-tools/ota_push.py <board-ip> \
    ~/.cargo-target/esp32-ccid-wifi-vpcd.bin
```

**Multi-board benches**: the stock driver exposes one listener per slot
(:35963 = "Nucula VPCD 00 00", :35964 = "Nucula VPCD 00 01"). Give each
board its own `NUCULA_VPCD_PORT` and the board↔reader mapping is
deterministic — no accept race. Both ports need ufw rules. Bench-proven
with nucula (wall power) + m5stick (computer USB power) side by side.

**Dead/absent reader**: an MFRC522 frontend that never answers degrades
to `OrNull::Null` — the board still serves its vpcd slot as a
permanently EMPTY reader, with OTA and netlog alive. "Reader online but
no card" means the frontend is broken/absent; "no connection at all"
means the board is gone. A wall-power nucula that loses its reader
behaves the same via the PN7160 card-absent mode.

Credentials are build-time env vars only — never in the tree. The
board answers OTA on TCP :3232 (ESP-IDF OTA with rollback hardening:
`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` + `mark_app_valid` after
bring-up). USB serial is only needed for recovery — opening the port
toggles DTR and chip-resets the board.

Logs: the firmware broadcasts WARN+ lines as UDP to the /24 subnet
broadcast, port 4567 (`192.168.x.255:4567`; limited broadcast is not
forwarded by the bench AP). Listener:

```sh
python3 -c "import socket; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);
s.setsockopt(socket.SOL_SOCKET,socket.SO_BROADCAST,1); s.bind(('0.0.0.0',4567));
[s.recvfrom(2048) for _ in iter(int,1)]"
```

**Power**: the nucula needs the 5 V/2 A wall adapter for a stable RF
field — computer USB couples marginally (card drops under load).
Contactless cards are RF-powered by the reader field.

## GPG

`scdaemon.conf` (per-homedir):

```
disable-ccid          # scdaemon must use the pcsc backend for virtual readers
reader-port <INDEX>   # INDEX of "Nucula VPCD 00 00" from SCardListReaders
```

Reader names with spaces do **not** match in `reader-port` — use the
numeric index. Verify the index after any reader plug/unplug (indices
shift); a full `gpg --card-status` takes ~40 s over this transport
(~2 s/APDU).

## GP (GlobalPlatformPro)

```sh
gp --reader "Nucula VPCD 00 00"        # reader by NAME (indices differ
                                        # from pyscard order under Java
                                        # SmartcardIO)
   --key-enc $NUCULA_GP_KEY_ENC --key-mac $NUCULA_GP_KEY_MAC \
   --key-dek $NUCULA_GP_KEY_DEK ...
```

Full-size STORE DATA blocks work natively since the NCI DATA
fragmentation landed (PBF-chained TX, credit-gated between fragments
on CORE_CONN_CREDITS_NTF, RX reassembly; bench 2026-10-13: 260-byte
APDU answered by the card, 117 KB CAP installs at default block size
in ~3.5 min). Before that fix a `--bs 200` workaround kept APDUs
inside one NCI packet — no longer needed.

## Operational rules (each one cost an hour)

1. **Never `pkill -9 scdaemon`** while it holds the reader — SIGKILL
   leaves a stale exclusive lock in pcscd (`SCARD_E_SHARING_VIOLATION`
   → "No such device" until pcscd restarts). Use
   `gpgconf --kill scdaemon`.
2. **Trust real `SCardConnect`, not `GetStatusChange` cache** — after
   pcscd restarts the cached presence can show CARD on the dead slot
   while the live connection serves the other one. Discover the
   working slot by attempting a real connect (ATR or not).
3. The stock driver exposes **two slots** ("00 00"/"00 01", listeners
   :35963/:35964). Map boards to slots deterministically with
   `NUCULA_VPCD_PORT` (see "Multi-board benches").
4. Firmware v3+ treats vpcd read timeouts as **idle, not disconnect** —
   pcscd goes silent with no clients; a 30s-timeout disconnect churned
   the TCP link and re-rolled the slot mapping every cycle.
5. **Restart pcscd after any board reboot/OTA reflash** — the board's
   lwIP drops TCP without a FIN on hard reset, leaving a half-open
   ESTAB that wedges the vpcd driver's single-accept; the next
   `SCardEstablishContext` hangs forever. The runbook restart sequence
   clears it (bench 2026-10-13: reproduced after every OTA).

## Recovery

- **Wedged card session** (all APDUs → SW 64 00 while ATR still
  serves): a cached auto-ATS keeps serving presence after the card
  leaves the field or the RF link degrades; `power_off` is lazy and
  never clears it. Recover with a same-image OTA reflash.
- **Board unresponsive** (no ping, no OTA port, silent console, USB
  still enumerates): full USB reflash — the ROM still answers esptool.
  ```sh
  esptool --chip esp32c3 -p /dev/ttyACM0 --baud 460800 erase_flash
  esptool --chip esp32c3 -p /dev/ttyACM0 --baud 460800 write-flash \
      0x0 <esp-idf-sys-out>/bootloader/bootloader.bin \
      0x8000 <esp-idf-sys-out>/partition_table/partition-table.bin \
      0x40000 ~/.cargo-target/esp32-ccid-wifi-vpcd.bin
  ```
  (Bench 2026-10-13: recovered a board that died after two OTA cycles.)

## Known limitations

- ~2 s/APDU latency (fire-and-collect budget; early-return pending)
- Slot lottery on every pcscd restart (two driver slots, one board
  connection per port — script the real-connect detection)

## Bench evidence (2026-10-13)

| Milestone | Result |
|---|---|
| M1 card detection | reproducible |
| M2 ATR over WiFi | via pcscd + raw prober |
| M3 APDU/CPLC soak | 7,914 overnight reads |
| M6 GPG card-status | ×3 through `Nucula VPCD 00 00` |
| M8 GP uninstall+install | SmartPGP delete→verify(6A82)→install→gpg OK, `--bs 200` |

See `docs/VPCD_ALIGNMENT_AUDIT.md` for the protocol-alignment audit and
`mission/decisions.md` (bench notebook) for the full experiment ledger.
