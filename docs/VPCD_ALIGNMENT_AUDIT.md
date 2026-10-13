# VPCD Alignment Audit — spec citations and design decisions

Date: 2026-10-13 · Issue: #154 · Feature: `wifi-vpcd`

## What changed

The socat/PTY/libccidtwin chain is replaced by the vsmartcard-native path:

| Before | After |
|---|---|
| pcscd → libccidtwin → socat PTY → TCP :29443 → firmware (GemPC serial) | pcscd → stock libifdvpcd.so → TCP :35963 → firmware (vpcd protocol) |
| 4 processes (pcscd + socat + systemd bridge + watchdog) | 1 process (pcscd only) |
| 744-line CCID serve loop + framing codec + echo contract + LRC | ~130-line vpcd server (length-prefixed APDUs) |
| Custom transport protocol (GemPC Twin serial over TCP) | Standard vsmartcard wire protocol |

## Spec citations

### Wire protocol (firmware/src/vpcd.rs)

| Marker | Source | Verbatim check |
|---|---|---|
| `VPCD_PROTOCOL` | vsmartcard/src/vpcd/vpcd.c `sendToVICC`/`recvFromVICC` | 2-byte BE length prefix; control = 1-byte payload; APDU = full C-APDU → R-APDU |
| `VPCD_CTRL` | vsmartcard/src/vpcd/vpcd.h `VPCD_CTRL_*` defines | ATR=0x00, ON=0x01, OFF=0x02, RESET=0x03 |
| `VPCD_DEFAULT_PORT` | vsmartcard/src/vpcd/vpcd.h `VPCDPORT` | 35963 |
| `VPCD_EJECT` | vpcd.c `vicc_eject()` | close socket, mark dead, re-accept on next connect |
| `VPCD_TRANSMIT` | vpcd.c `vicc_transmit()` | send C-APDU → recv R-APDU (incl SW per IFD handler contract) |
| `VPCD_TIMEOUT` | ISO 7816-4 §5.1.3 | SW 6400: card timeout — clean host-stack failure |

### Host driver (stock, zero custom code)

| Component | Package | File |
|---|---|---|
| IFD handler | `vsmartcard-vpcd` (distro) | `/usr/lib/pcsc/drivers/serial/libifdvpcd.so` |
| Reader config | this repo | `/etc/reader.conf.d/nucula-vpcd` |

### Driver API mapping (ifd-vpcd.c → vpcd protocol)

| PC/SC call | vpcd message | Firmware handler |
|---|---|---|
| `IFDHCreateChannel` | TCP connect | `TcpListener::accept()` |
| `IFDHICCPresence` | CTRL_ATR (probe) | `driver.is_card_present()` + `power_on()` |
| `IFDHPowerICC(UP)` | CTRL_ON | `driver.power_on()` |
| `IFDHPowerICC(DOWN)` | CTRL_OFF | `driver.power_off()` |
| `IFDHTransmitToICC` | length-prefixed APDU | `driver.transmit_apdu()` |

## Known deviations from stock vpcd

1. **Port**: stock vpcd listens (server mode) waiting for the card to connect.
   Our firmware also listens — the driver's reader.conf specifies our hostname
   in DEVICENAME, which puts libifdvpcd.so in CLIENT mode (connect to us).
   This matches the vsmartcard "reversed" connection pattern.

2. **Thread safety**: stock vpcd is not thread-safe (ifd-vpcd.c TAG_IFD_THREAD_SAFE=0).
   Our implementation uses a dedicated thread per connection with sequential
   message handling — same effective contract.

3. **APDU size**: stock vpcd supports up to 65535-byte extended APDUs.
   Our NFC path handles short APDUs only (261 bytes max) — the PN7160's
   ISO-DEP limitation, not a vpcd protocol limitation.

## Gaps addressed (tonight's failure classes)

| G-XX | Failure | vpcd fix |
|---|---|---|
| G-06 | Zombie backlog clients | Single-accept model; vpcd.c `listen(sock, 0)` |
| G-07 | Half-open TCP after resets | Driver ejects on error; reconnects per operation |
| G-08 | pcscd PTY wedges | No PTY exists |
| G-09 | Session desyncs (stale bytes) | Length-prefixed framing; no echo/LRC contract |
| G-10 | socat double-spawn races | No socat exists |
| G-11 | 144 watchdog heals/7h | No watchdog needed; driver handles recovery |
