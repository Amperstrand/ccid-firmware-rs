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
NUCULA_VPCD_HOST=<host-ip> ./build.sh wifi-vpcd
python3 host-tools/ota_push.py <board-ip> \
    ~/.cargo-target/esp32-ccid-wifi-vpcd.bin
```

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
   --key-dek $NUCULA_GP_KEY_DEK \
   --bs 200 ...                         # see NCI limitation below
```

**`--bs 200` is required today**: NCI DATA packets carry a 1-byte
length and the crate does not yet fragment (TX) or reassemble (RX)
PBF-chained messages, so APDUs > ~245 bytes fail as SW `64 00`. A
260-byte STORE DATA (255 B data + header, +8 B SCP02 MAC) is exactly
that. 200-byte blocks keep every APDU inside one NCI packet; a 117 KB
CAP loads in ~4 min. Keys come from env vars — never from the repo.

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
   :35963/:35964). The board connects to :35963 once; which reader
   name maps to the live connection is decided per pcscd restart.
4. Firmware v3+ treats vpcd read timeouts as **idle, not disconnect** —
   pcscd goes silent with no clients; a 30 s-timeout disconnect churned
   the TCP link and re-rolled the slot mapping every cycle.

## Known limitations

- NCI single-packet DATA cap (~245 B APDUs) — PBF-chaining fix planned
- ~2 s/APDU latency (fire-and-collect budget; early-return pending)
- Slot lottery on every pcscd restart (two driver slots, one board
  connection — script the real-connect detection)

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
