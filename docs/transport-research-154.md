# Transport research: wifi CCID transport for pcscd — socat PTY vs native network IFD driver

**Issue:** https://github.com/Amperstrand/ccid-firmware-rs/issues/154
**Date:** 2026-10-13 (mission night post-mortem lane)
**Method:** 4 research lanes — vsmartcard/vpcd source read (zread), USB/IP (kernel docs
+ usbipd-win + USENIX '05 paper + jiegec/usbip), remote-PC/SC patterns survey
(pcsc-lite history, Chrome Smart Card Connector, OpenCT, pcsc-relay, RDP), local code
map (worktree `/tmp/opencode/ccid-idf555` @23d77d3 + uncommitted hardening).
All load-bearing claims carry a source. Analysis is marked ⚠️ where it is ours, not theirs.

---

## TL;DR

**Recommendation: write `libifd-nucula.so` — a native pcscd IFD driver (ifdhandler v3)
that connects over TCP to the firmware's existing `:29443` server and speaks the GemPC
Twin serial framing directly.** Zero firmware protocol change; the socat/PTY/systemd/
watchdog process soup is deleted by construction; every failure class from tonight maps
to a mechanism the vsmartcard `vpcd` driver has used in production for a decade
(eject-on-error + reconnect-on-demand + presence-as-poll). USB/IP is the worst fit for
this hardware (ESP32-C3 has no USB OTG; it would need an SBC gateway or an
experimental userspace usbip-server port). Latency is a wash on every option — wifi RTT
(7 ms floor) dominates; **the differentiator is robustness and process count, not speed.**

---

## 1. Baseline: what broke tonight and why

Chain: `pcscd (libccidtwin) ↔ /dev/nucula-wifi (socat PTY) ↔ TCP :29443 ↔ ESP32-C3`.

| Failure class | Mechanism (evidence) |
|---|---|
| Zombie backlog clients | Firmware listener is a single-client lwIP server; second connects complete the handshake and sit in the kernel accept backlog looking ESTAB. lwIP listening cap `CONFIG_LWIP_MAX_LISTENING_TCP=16` (sdkconfig.full:1233). Worktree contains an UNCOMMITTED fix: accept-and-`Shutdown::Both` extra clients each 250 ms tick (wifi_ccid.rs:184-187). |
| Half-open TCP after board reset | No `SO_KEEPALIVE` anywhere in the firmware (zero grep hits); socat only learns of peer death on write. Board resets ~2 s, host side hangs until serial timeouts fire. |
| socat double-spawn races | Supervisor vs watchdog both managing socat; 13 orphans found (mission log, decisions.md "Infrastructure state"). The bridge itself is not even in-repo — greps for `socat`/`nucula-wifi` hit only two doc comments (Cargo.toml:39, wifi_ccid.rs:8-9). |
| pcscd PTY wedges | libccidtwin holds PTY state; board reset + socat restart cycles leave pcscd in `ERR CardConnectionException` loops. Operational cost: **144 watchdog heals in ~7 h** (mission.log tally; heal actions = pcscd restart + bridge restart). |
| Latency | Not actually broken: wifi power-save OFF gives a 7 ms RTT floor (wifi.rs:89-95); the PTY hop adds ~0.5-1 ms class (CDC broker measured 0.72 ms p50 PTY RTT overhead, issue #117). |

---

## 2. Prior art, ranked by relevance

### 2.1 vsmartcard `ifd-vpcd` — the closest prior art (read in full)

A real pcscd IFD driver (`.so`) whose transport is TCP. In production since ~2010,
cross-platform (Linux/macOS/Windows), still building against current pcsc-lite. The
`remote-reader` Android app uses exactly our shape — "allow a host computer to use the
smartphone's NFC hardware as contact-less smartcard reader … the app establishes a
connection to vpcd over the network" (remote-reader/README.md) — i.e. a wifi NFC reader
serving a host, shipped for a decade.

**reader.conf integration** (doc/vpcd_example.conf):

```
FRIENDLYNAME "Virtual PCD"
DEVICENAME   /dev/null:0x8C7B
LIBPATH      /usr/lib/pcsc/drivers/serial/libifdvpcd.so
CHANNELID    0x8C7B
```

`IFDHCreateChannel(Lun, Channel)` receives CHANNELID **as the TCP port** (ifd-vpcd.c,
`vicc_init(hostname, Channel+slot)`). `IFDHCreateChannelByName` parses
`DEVICENAME host:port`: if the prefix is not `/dev/null`, the driver **connects out**
to that host (client mode); if it is `/dev/null` (or no DEVICENAME), the driver
**binds and listens** (server mode, device connects in). Both directions are
first-class — the `--reversed` connection mode in vicc and the Android app.

**Connection lifecycle (vpcd.c, all verified in source):**

- `opensock`: `SO_REUSEADDR`, `INADDR_ANY`, **`listen(sock, 0)`** — zero backlog by
  design; a second client simply is not accepted.
- `vicc_init(hostname, port)`: server mode binds (fail → driver load fails); client
  mode `connectsock()` result is **not** checked — a down device does not fail reader
  creation.
- `vicc_connect(ctx, 0, 0)`: called on demand before every exchange — server mode does
  a `poll(fd, 0 ms)` + `accept` if a client is waiting; client mode retries
  `connectsock()`. **Reconnect is implicit, per-operation, and free.**
- `vicc_transmit`: send length-prefixed message, blocking recv of the reply; on any
  `r <= 0` → `vicc_eject()`: close ONLY the client socket, set it INVALID. **The
  listener persists** — the next `vicc_connect` accepts the reconnecting device.
- Presence: `vicc_present` = `vicc_connect` + `vicc_getatr` — presence IS a wire poll.

**Wire protocol:** `[u16 BE length][payload]`; length 1 = control code
(OFF=0, ON=1, RESET=2, ATR=4 — vpcd.h). No LRC, no sync bytes — TCP ordering is the
framing guarantee. Deliberately simpler than GemPC framing.

**pcscd integration details worth copying:**

- `IFDHGetCapabilities(TAG_IFD_SLOTS_NUMBER)` = `VPCDSLOTS`; `TAG_IFD_THREAD_SAFE` =
  0 (global ctx array + per-ctx pthread mutex — the mutex serializes transmit vs.
  poweron/poweroff, lock.c).
- `IFDHSetProtocolParameters` returns `IFD_SUCCESS` unconditionally (virtual card).
- `IFDHControl`: answers `CM_IOCTL_GET_FEATURE_REQUEST` with exactly one feature
  (GET_TLV_PROPERTIES → advertises 64 KB extended APDUs).
- pcscd polls `IFDHICCPresence` at `PCSCLITE_STATUS_POLL_RATE = 400 000 µs` ≈ **400 ms**
  (pcsc-lite `src/pcscd.h`, eventhandler.c) — this poll is the heartbeat that drives
  reconnect and bounds reset-detection latency to ~1 s.

**Documented weaknesses we should NOT copy:** `recvall` has **no timeout** — a
black-holed peer can hang pcscd's reader thread indefinitely (we add `SO_RCVTIMEO`);
no keepalive (we add it driver-side); `hostname` is a file-static global (thread
safety 0).

**Security posture:** none — INADDR_ANY bind, `listen(0)`, no auth, no TLS; README:
the vpicc "does not need to run on the same machine … they can connect over the
internet for example". Field-standard for this entire class (see §6).

### 2.2 pcsc-lite: custom IFD drivers are the sanctioned extension point

- Full ifdhandler v3 API + reader.conf semantics documented at
  https://pcsclite.apdu.fr/api/group__IFDHandler.html (2.4.1): required entry points
  `IFDHCreateChannel[ByName]`, `IFDHCloseChannel`, `IFDHGetCapabilities` (TAG_IFD_ATR
  mandatory), `IFDHSetCapabilities`, `IFDHSetProtocolParameters`, `IFDHPowerICC`,
  `IFDHTransmitToICC` (driver owns T=0/T=1 differences), `IFDHICCPresence`,
  `IFDHControl`.
- **DEVICENAME was designed to carry non-file URIs**: ChangeLog 1.2.9-beta3
  (2004-06-30): allow `:` in DEVICENAME "to indicate a non-real device … You can use
  this in something like `net://1.2.3.4/foobar` to indicate a reader on a remote
  machine like an X11 terminal." The slot has been reserved for 21 years; nobody
  shipped a canonical driver for it (grep.app survey 2026-10-13: the only
  TCP-transport IFD drivers that ever existed are vpcd and historical OpenCT, §2.4).
- pcscd itself **never had a TCP listener** — client protocol is a Unix domain socket
  (`/var/run/pcscd/pcscd.comm`); the official remote mechanism is SSH-forwarding that
  socket (pcsc-lite 1.6.5, r5373, 2010; L. Rousseau, "PC/SC client and server on two
  different hosts", https://blog.apdu.fr/posts/2010/11/pcsc-client-and-server-on-two-different/).
  That forwards the whole daemon — wrong shape for an MCU reader (needs a PC next to
  the device) — noted for completeness, rejected.

### 2.3 USB/IP

- Kernel stack (Documentation/usb/usbip_protocol.rst, tools/usb/usbip/README): client
  `vhci-hcd` imports by busid from server `usbip-host`; URBs encapsulated over
  TCP :3240, `TCP_NODELAY`. **The device driver runs on the client** — host pcscd would
  see a real USB CCID device (full libccid, not libccidtwin).
- Latency (measured, USENIX '05 paper): `t_overIP = 6.30e-2 ms/KB × size + RTT`,
  fixed intercept 0.24 ms vs 0.07 ms direct (P3-1 GHz era). For small APDU-sized URBs
  the added cost is ≈ RTT + ~0.2 ms — same class as our TCP hop. ⚠️ CCID also has an
  interrupt-IN endpoint whose periodic polling becomes background TCP round trips
  (structural consequence of URB encapsulation; magnitude unmeasured here).
- Robustness: attach is **non-persistent** across device reset/unplug/reboot (usbipd-win
  README, explicit); server bind must be redone after suspend; busids churn across boots
  → bind-by-VID:PID + systemd template units + community `usbip-systemd` helper
  (Arch Wiki, ed. 2026-07). usbipd-win `--auto-attach` exists (v5.1.0+, 2025). On
  WSL2 clients, smartcard attach has known friction: pcscd must be started/restarted
  after attach and udev reloaded before (usbipd-win #97, #139; maintainer: "distribution
  dependent"). Bonus: ESP32 flashing via DTR/RTS **fails over usbip** (usbipd-win #469,
  open since 2022) — our reflash cycles would break attachments.
- **The hardware wall:** the ESP32-C3 has only the USB-Serial/JTAG peripheral —
  verified in the esp-idf tree: `components/soc/esp32c3/register/soc/` contains
  `usb_serial_jtag_*` only, no `usb_wrap`/OTG (S2/S3 have it). The C3 can neither be a
  USB CCID device nor run `usbip-host`. Options: (a) Linux SBC gateway in front of the
  reader — but mainline has **no CCID gadget function** (drivers/usb/gadget/function/
  listing: f_acm/f_serial/f_hid/…, no f_ccid) so the SBC would need a FunctionFS
  userspace CCID gadget — real work, plus a new box; or (b) a **userspace usbip server
  ON the ESP32**: `jiegec/usbip` (Rust, ★539, active 2026-09) proves the pattern
  (simulates HID/CDC devices, QEMU-tested, `UsbDevice` trait takes arbitrary class
  codes) — but it depends on tokio and unconditionally links `rusb`/`nusb`, so an
  ESP32 port is a fork job, and no CCID-over-usbip-from-MCU precedent exists.
  ⚠️ Feasible-in-principle, unproven on C3 RAM, weeks of work — park it.

### 2.4 Other patterns (survey)

| Pattern | What it is | Takeaway for us |
|---|---|---|
| **OpenCT `ifdproxy` + RIA** (OpenSC/openct, legacy) | Full historical stack: `openct-ifd` pcscd driver → `ifdproxy` daemon exporting readers over **TCP :6666** (`ifdproxy export name device address`). Shipped in Solaris/Sun Ray distros. | Proof the "network IFD driver" shape shipped commercially 20 years ago; dead since — not a candidate. |
| **pcsc-relay** (frankmorgner/vsmartcard; orig. D. Oepen 2009) | Emulator↔connector APDU relay; TCP :35963 by default, listens and waits for the far side; no auth. | Best-documented APDU-relay-over-TCP composition model; same threat model as ours. |
| **Chrome Smart Card Connector** (GoogleChromeLabs/chromeos_smart_card_connector) | pcscd + CCID driver compiled to WASM inside a Chrome extension; libusb shim → `chrome.usb`; PC/SC exposed via JSON messages over `chrome.runtime` (`pcsc_lite_function_call::request/response`); security = user prompt + extension-ID allowlist + enterprise policy. Pluggable drivers are explicitly "future enhancement". | Architectural precedent: relocating pcscd behind a message API with a client-permission model. Not a network transport — reference only. |
| **RDP/terminal-services redirection** ([MS-RDPESC] v17.0 2024, [MS-RDPEFS] RDPDR) | Industry's actual answer: reader stays USB on the client; PC/SC is tunneled inside the authenticated remote-session protocol. | The reader never gets its own network identity in the industrial pattern — confirms we're in greenfield territory (and that's fine). |
| **"CCID over IP"** | USB-IF CCID Rev 1.1 (2001) is USB-transport-only; no revision adds IP. The "proposed but never standardized" story: **no primary source found — treat as folklore**. | Nothing to adopt; don't cite it as fact. |
| **libnfc `nfc-relay` / Proxmark3 relay** | Local two-device relay / TCP relay subsystem (`lf relay`, relay_posix.c), no auth. | NFC-lab prior art only. |

---

## 3. Decision matrix

Axes from the issue: latency, process complexity, host deps, reset robustness, auth,
maintenance, firmware change. Scores ⚠️ are our judgment on cited facts.

| | **A. socat PTY (status quo)** | **B. `libifd-nucula.so` IFD driver, GemPC over TCP** ⭐ | **C. stock `libifdvpcd.so` + firmware speaks vpcd protocol** | **D. USB/IP** | **E. SSH pcscd-socket forward** |
|---|---|---|---|---|---|
| Latency | baseline (PTY hop ~0.5-1 ms on top of 7 ms wifi RTT) | −PTY hop; otherwise identical wire | same as B | +~0.2 ms/URB + RTT per URB; interrupt-EP polling round trips; ⚠️ comparable totals | SSH crypto overhead on every PC/SC call |
| Process complexity | pcscd + socat + systemd unit + watchdog heal loop (144 heals/7 h tonight; 13 orphans) | **pcscd only** — driver is a dlopen'ed .so, no daemon, no unit, no watchdog | pcscd only | pcscd + vhci attach + server-side usbipd/usbip-host + (SBC gateway OR experimental ESP32 usbip-server port) + auto-attach scripts | pcscd ×2 + sshd + ssh client + a bridge PC next to the reader |
| Host deps | stock pcscd (libccidtwin), socat | stock pcscd + our ~600-line C .so in `/usr/lib/pcsc/drivers/` + one reader.conf entry | stock pcscd + distro `libifdvpcd.so` package (vsmartcard) | kernel vhci-hcd + usbip tools (Linux) / usbipd-win (Windows); SBC if hardware path | ssh everywhere + a full PC beside the reader |
| Reset robustness | broken (tonight's classes) | **eject-on-error + reconnect-on-demand per operation (vpcd model), driver-side keepalive 5 s/2 s×3 → dead board detected ≤ ~9 s; reader stays listed, card shows absent, recovers on board return** | same model; stock vpcd has NO recv timeout → black-holed peer can hang pcscd's reader thread (would need fork anyway) | non-persistent attach; rebind after suspend; busid churn; ESP32 DTR/RTS reflash breaks it (#469) | ssh session management on top of the same transport problems |
| Auth | none (plaintext TCP) | none — field-standard (§6); keepalive/timeout are the robustness story, not auth | none | none in protocol (relies on network controls) | **SSH** — the only authed option |
| Maintenance | 4 moving parts across 2 hosts, one of them not even in-repo | one .so, one conf entry; the broker stays as CDC-path code (fix-all-siblings: paths legitimately differ) | firmware framing rewrite + a foreign driver's quirks | largest surface: two kernel stacks + gateway hardware/port fork | extra host |
| Firmware change | none | **none** (commit the uncommitted wifi_ccid.rs hardening — the zombie/mute fixes exist only in this worktree) | rewrite wifi framing to u16-length protocol; **loses CCID-level escapes (0xD0/0xD1/0xD2/0xD3 ride CCID 0x6B, not APDUs)** | impossible on C3 without new hardware or a weeks-long usbip-server port | none, but wrong topology |
| Kills tonight's failure classes | — | zombies: single connection, no PTY, backlog drained by accept-and-reject; half-open: driver keepalive+timeout; double-spawn: nothing left to spawn; PTY wedge: no PTY exists | same as B | no (adds its own) | no |

**Also-ran:** upgrading the python broker to bridge wifi TCP (in-repo `host-tools/
nucula-ccid-broker.py` pattern — serving-proof gating, PowerOffShim, wire tap) is the
cheap incremental fix if B stalls; it fixes double-spawn and wedge discipline but keeps
two processes + a PTY + shim hacks. It is the fallback, not the target.

**Why not C (the tempting zero-custom-code option):** stock `libifdvpcd.so` in client
mode would connect to a firmware speaking the vpcd wire protocol — but (1) the firmware
would need its framing layer swapped for the wifi path (our codec is behind
`ccid-transport-serial`, so it's doable, but it forks the transport doctrine), (2) vpcd
protocol is APDU-level — our diagnostic escapes (`0x6B` CCID Escape with `0xD0`
counters, `0xD1` coredump trigger, `0xD2`/`0xD3` bench probes) have no vpcd carrier and
would be lost on that path, (3) stock vpcd's unbounded blocking recv can hang pcscd —
we'd fork it for the timeout alone, at which point B (own driver, own framing, own
escapes) is less total work than maintaining a vpcd fork plus a firmware protocol fork.

---

## 4. Recommendation

**B. `libifd-nucula.so`.** Native pcscd IFD driver, ifdhandler v3, transport = TCP
connect-out to the firmware's existing `:29443` listener, protocol = the existing GemPC
Twin framing byte-for-byte (`ccid-transport-serial`), lifecycle = the vpcd model
(eject-on-error, reconnect-per-operation, presence-as-poll) plus the two hardening
additions vpcd lacks (driver-side `SO_KEEPALIVE`, `SO_RCVTIMEO`).

Rationale in one line each:

1. **Zero firmware protocol change** — the serve loop already speaks exactly this
   protocol on UART/CDC/SPP/TCP (`serial_framing.rs:1` re-exports the shared codec);
   the wifi profile is strictly request → echo → response with no unsolicited frames
   (default `ServeConfig`: `notify_slot_change=false`, silent errors) — the simplest
   possible wire contract for a driver.
2. **Deletes the failure classes by construction**: no PTY (nothing for pcscd to
   wedge on), no socat (nothing to double-spawn), no watchdog (reconnect is a driver
   function, not a supervisor).
3. **Sanctioned extension point** — reader.conf/DEVICENAME-for-non-file-devices has
   been the designed hook since 2004 (§2.2); vpcd is a decade-old shipping proof.
4. **Latency neutral-to-positive**: removes the PTY hop; wifi RTT dominates either way.
5. **Bench wins for free**: `IFDHControl` can advertise `FEATURE_CCID_ESC_COMMAND` and
   pass escapes through — diagnostics (`0xD0` counters, `0xD2` turnaround) become
   `SCardControl` calls instead of pyserial port-grabbing that fights the reader.

**Phase 2 (optional, only if board-reset storms persist):** flip the direction to the
vpcd default — host listens (`listen(0)`), firmware connects out. This removes the
lwIP backlog surface entirely and matches the Android remote-reader topology. It is a
small firmware change (TCP client instead of server) but is NOT required for Phase 1.

**Concurrent firmware work required regardless of path:** land the uncommitted
worktree hardening (tick serve loop, accept-and-reject, mute recovery — currently
+78 lines only in `/tmp/opencode/ccid-idf555`) — the zombie-backlog fix lives only
there. And keep fixing #125 (post-power-off re-arm) in firmware: with the PTY broker
gone, the driver does NOT carry the PowerOffShim, so the firmware gap is hit directly.
Interim driver-side mitigation: env `NUCULA_POWER_OFF_SHIM=1` suppresses IccPowerOff
locally and synthesizes the SlotStatus ack (20 lines, mirrors the broker's shim).

---

## 5. Implementation sketch — `libifd-nucula.so`

### 5.1 Deployment shape

```
/etc/reader.conf.d/nucula-wifi.conf:
    FRIENDLYNAME "Nucula CCID (wifi)"
    DEVICENAME   192.168.13.42:29443      # or nucula-wifi.local once mDNS lands
    LIBPATH      /usr/lib/pcsc/drivers/libifd-nucula.so
    CHANNELID    1
```

No systemd unit. No watchdog. pcscd picks it up on restart (`-H` reload also works —
reader.conf re-read is the documented path for non-USB readers).

### 5.2 Module skeleton (C, libpthread only)

```c
/* libifd-nucula.so — pcscd IFD handler v3 for the nucula wifi CCID transport.
 * Protocol: GemPC Twin serial framing over TCP (crates/ccid-transport-serial):
 *   host→reader  [0x03 0x06 <10-byte CCID hdr> <payload ≤261> <LRC=XOR>]
 *   reader→host  echo of the received frame, then [0x03 0x06 <RDR msg> <LRC>]
 *   NAK [0x03 0x15 0x16];  slot-change [0x50 xx] (not sent in wifi profile)
 * Lifecycle model: vsmartcard vpcd (eject-on-error, reconnect-per-op, presence=poll)
 * plus SO_KEEPALIVE + SO_RCVTIMEO (vpcd lacks both).
 */
#include <ifdhandler.h>
#include <reader.h>
#include <pthread.h>

typedef struct {
    int           fd;          /* -1 = disconnected */
    char          host[64];
    unsigned      port;
    unsigned char seq;         /* CCID bSeq */
    pthread_mutex_t io_lock;
    unsigned char atr[MAX_ATR_SIZE];  size_t atr_len;   /* last ATR */
} nucula_ctx;

static nucula_ctx g_ctx;                 /* single slot: TAG_IFD_THREAD_SAFE = 0 */

/* --- transport --- */
static int  nuc_connect(nucula_ctx *c) { /* socket + connect (500 ms nonblocking
    * connect+poll so a dead board can't stall pcscd's load path), then:
    *   TCP_NODELAY      1
    *   SO_KEEPALIVE     1, TCP_KEEPIDLE 5, TCP_KEEPINTVL 2, TCP_KEEPCNT 3   (dead peer ≤ ~9 s)
    *   SO_RCVTIMEO      10 s   (bounds every read; card worst case ≪ that)
    * failure is NOT fatal (vpcd precedent: reader listed, card absent) */ }
static void nuc_eject(nucula_ctx *c) { close(c->fd); c->fd = -1; }

/* frame codec: straight port of ccid-transport-serial/src/lib.rs
 * FSM (WaitSync→WaitCtrl→ReadHeader(10)→ReadPayload(dwLength LE)→CheckLrc),
 * overflow reject > 261; skip stray [0x50 xx] defensively (wifi profile never sends it) */
static int  nuc_send_frame(nucula_ctx *c, const unsigned char *ccid, size_t n);
static int  nuc_recv_frame(nucula_ctx *c, unsigned char *out, size_t *n);

/* one command/Response exchange: send → read ECHO → verify echo == sent (free
 * integrity check) → read response frame. Any error: eject, return -EIO. */
static int  nuc_transact(nucula_ctx *c, const unsigned char *ccid, size_t n,
                         unsigned char *resp, size_t *resp_len);

/* --- ifdhandler v3 entry points --- */
RESPONSECODE IFDHCreateChannelByName(DWORD Lun, LPSTR DeviceName) {
    /* parse "host:port" (our parser; pcscd's DEVICENAME colon-split is libccidtwin's
     * problem, not ours). Init ctx. Try nuc_connect(); tolerate failure. IFD_SUCCESS. */
}
RESPONSECODE IFDHCreateChannel(DWORD Lun, DWORD Channel) { /* same, host from env
    * NUCULA_HOST default, port = Channel (vpcd maps CHANNELID→port the same way) */ }

RESPONSECODE IFDHCloseChannel(DWORD Lun) { nuc_eject(&g_ctx); return IFD_SUCCESS; }

RESPONSECODE IFDHGetCapabilities(DWORD Lun, DWORD Tag, PDWORD L, PUCHAR V) {
    switch (Tag) {
    case TAG_IFD_ATR:          memcpy(V, g_ctx.atr, g_ctx.atr_len); *L = atr_len; break;
    case TAG_IFD_SLOTS_NUMBER: *V = 1; *L = 1; break;
    case TAG_IFD_THREAD_SAFE:  *V = 0; *L = 1; break;   /* vpcd precedent */
    default: return IFD_ERROR_TAG;
    } return IFD_SUCCESS;
}

RESPONSECODE IFDHPowerICC(DWORD Lun, DWORD Action, PUCHAR Atr, PDWORD AtrLength) {
    /* UP:   CCID IccPowerOn  (0x62) → DataBlock (0x80) payload = ATR; cache atr
     * DOWN: CCID IccPowerOff (0x63) → SlotStatus (0x81)
     *   (optional NUCULA_POWER_OFF_SHIM=1: answer locally with the bench-exact
     *    SlotStatus ack and never send 0x63 — firmware #125 interim, mirrors broker)
     * RESET: IccPowerOn again (firmware re-activates; its lazy-teardown model) */
}

RESPONSECODE IFDHTransmitToICC(DWORD Lun, SCARD_IO_HEADER Send, PUCHAR Tx, DWORD TxLen,
                               PUCHAR Rx, PDWORD RxLen, PSCARD_IO_HEADER Recv) {
    /* CCID XfrBlock (0x6F) ← Tx; response DataBlock (0x80) → Rx.
     * NOTE: after IFDHSetProtocolParameters(T=1) the payload may be T=1 blocks —
     * the firmware answers S-blocks locally and reassembles I-blocks (t1.rs); the
     * driver is a dumb relay exactly like libccidtwin. Recv->Protocol = negotiated. */
}

RESPONSECODE IFDHSetProtocolParameters(DWORD Lun, DWORD Prot, UCHAR Fl, UCHAR PTS1,
                                       UCHAR PTS2, UCHAR PTS3) {
    /* FORWARD it: CCID SetParameters (0x61) with the T=1 param set — this ARMS the
     * firmware's T=1 endpoint (ccid_handler.rs t1_armed). Do NOT vpcd-style no-op it. */
}

RESPONSECODE IFDHICCPresence(DWORD Lun) {
    /* GetSlotStatus (0x65) → SlotStatus (0x81): bStatus&0xC0 == 0 → PRESENT.
     * comm error → eject + IFD_ICC_NOT_PRESENT (reader stays listed; vpcd does
     * exactly this on vicc_present failure) — this is the reconnect engine. */
}

RESPONSECODE IFDHControl(DWORD Lun, DWORD Code, PUCHAR Tx, DWORD TxLen,
                         PUCHAR Rx, DWORD RxLen, LPDWORD Ret) {
    /* CM_IOCTL_GET_FEATURE_REQUEST → advertise FEATURE_CCID_ESC_COMMAND only.
     * ESC ioctl → CCID Escape (0x6B) passthrough: host-side 0xD0 counters, 0xD2/0xD3
     * bench probes via SCardControl. 0xD1 stays raw-TCP-only (it reboots the board). */
}

RESPONSECODE IFDHSetProtocolParameters/IFDHSetCapabilities stubs per ifdhandler.h.
```

Wire mapping is 1:1 with the firmware dispatch table (ccid-core handler.rs:369-397;
esp32 ccid_handler.rs T=1 routing) — no message the driver must generate is missing
from the firmware, and vice versa (Secure 0x69 exists in-core; nucula has no PIN pad,
answer GET_FEATURE accordingly; Abort/IClock/SetRate → firmware already answers
CMD_NOT_SUPPORTED, driver mirrors).

### 5.3 Driver behavior contract (the parts that kill tonight's classes)

| Event | Driver behavior |
|---|---|
| Board resets mid-session | keepalive probe fails ≤ ~9 s (or first write errors) → `nuc_eject` → next `IFDHICCPresence` (≤ 400 ms cadence) reconnects when the board answers → pcscd sees card absent→present, **reader never leaves the list, no restarts** |
| Board down at pcscd start | `IFDHCreateChannel` still succeeds (connect failure tolerated, vpcd precedent) — reader listed, card absent, reconnect rides the presence poll |
| Second client (bench probe) | firmware accept-and-rejects it (worktree hardening — **must be committed**); probes should use `SCardControl` escapes or the driver must be stopped |
| Malformed/bad-LRC frame from wifi noise | firmware silently drops (default ServeConfig, no NAK) → driver bSeq-match + 400 ms timeout + one retransmit (pcscd's own model) |
| Card hangs (no DataBlock) | `SO_RCVTIMEO` 10 s → error → eject; pcscd surfaces comm error instead of a wedged thread (improvement over stock vpcd) |
| Inter-byte garbage | parser FSM + 10-tick inter-byte reset on firmware side; strict SYNC-anchored scan driver-side (the repo's standing reader-test lesson) |

### 5.4 Test plan (host-side, no hardware first)

- Codec conformance: port the 25 `ccid-transport-serial` host tests' golden frames as
  driver unit tests (echo-verify, LRC, overflow reject, stray-SYNC resync).
- Mock board: small python TCP server speaking the framing (reusing
  `tests/hardware/nfc/conformance_battery.py`'s `Reader` logic) → drive the driver via
  `SCardEstablishContext`/`SCardTransmit` against pcscd with a temp reader.conf.
- Bench: HIL-add `tests/hardware/labgrid/` cases — kill -9 the board's wifi (router
  side), power-cycle the board, verify reader stays listed and recovers; soak with the
  existing differential suite through pcscd.
- Mocks must model the firmware's actual filtering/error semantics (repo doctrine:
  mock/hardware divergence is bug class #1).

### 5.5 Repo placement

`host-tools/ifd-nucula/` (C + Makefile installing to `/usr/lib/pcsc/drivers/` +
reader.conf template) next to `nucula-ccid-broker.py` (which stays for the CDC path —
different transport, legitimately different tool per the sibling doctrine; cross-reference
both in the issue when this lands).

---

## 6. Security posture (all surveyed options)

| Option | Auth | Encryption |
|---|---|---|
| socat PTY (today) | none | none (plaintext TCP) |
| **libifd-nucula.so** | none | none — same posture as the field |
| vpcd / pcsc-relay / OpenCT ifdproxy / Proxmark relay | none (verified in code/options) | none |
| USB/IP (kernel/usbipd-win) | none in protocol | none |
| SSH pcscd-socket forward | SSH keys | SSH |
| Chrome Smart Card Connector | prompt + allowlist + policy | n/a (no network leg) |
| RDP redirection | RDP NLA/TLS | RDP TLS |

**Every general-purpose TCP reader option in the wild ships plaintext and unauthenticated
— they all assume a trusted segment.** Our bench LAN qualifies. If this ever crosses an
untrusted network: wrap in WireGuard (transport-agnostic, zero code) — do not invent a
TLS handshake into the driver first (⚠️ no prior art to copy; greenfield risk).

---

## 7. Sources

**vsmartcard (read via zread, 2026-10-13):** frankmorgner/vsmartcard —
`virtualsmartcard/src/ifd-vpcd/{ifd-vpcd.c, reader.conf.in}`, `virtualsmartcard/src/vpcd/
{vpcd.c, vpcd.h, lock.c}`, `virtualsmartcard/doc/vpcd_example.conf`, `remote-reader/
README.md`, `pcsc-relay/src/vpcd-driver.c`. pcscd poll rate: pcsc-lite
`src/pcscd.h` `PCSCLITE_STATUS_POLL_RATE 400000` (grep.app → LudovicRousseau/PCSC
src/eventhandler.c).

**pcsc-lite:** https://pcsclite.apdu.fr/api/group__IFDHandler.html (2.4.1);
ChangeLog https://salsa.debian.org/rousseau/PCSC/-/blob/master/ChangeLog (1.2.9-beta3
net:// entry; 1.6.0/1.6.5); https://blog.apdu.fr/posts/2010/11/pcsc-client-and-server-on-two-different/.

**USB/IP:** torvalds/linux `Documentation/usb/usbip_protocol.rst` + `tools/usb/usbip/
README`; Hirofuchi et al., USENIX '05, "USB/IP — a Peripheral Bus Extension"
(https://www.usenix.org/legacy/events/usenix05/tech/freenix/hirofuchi/); usbipd-win
README/wiki/issues #139 #97 #469 #636 + Tested-Devices (https://github.com/dorssel/usbipd-win);
Arch Wiki USB/IP (ed. 2026-07); jiegec/usbip (https://github.com/jiegec/usbip);
esp-idf tree C3 vs S2/S3 soc register dirs (github.com/espressif/esp-idf); kernel
gadget function listing (drivers/usb/gadget/function/ — no f_ccid).

**Patterns survey:** GoogleChromeLabs/chromeos_smart_card_connector (docs/
index-developer.md, docs/connector-app-api.md); OpenSC/openct (`src/ifd/ifdproxy.c`,
`ria.h`, `etc/openct.conf.in` :6666); grep.app IFDHCreateChannel survey 2026-10-13;
MS-RDPESC v17.0 / MS-RDPEFS (learn.microsoft.com); USB-IF CCID Rev 1.1
(https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf).

**Local tree (worktree /tmp/opencode/ccid-idf555 @23d77d3):** firmware/esp32-ccid/src/
wifi_ccid.rs (:31 port, :77 bind, :126-142 nonblocking/nodelay/250 ms tick, :184-187
accept-and-reject [uncommitted], :35-36 idle close, absence of keepalive);
src/ccid_serial_server.rs (ServeConfig profiles, echo→response order, stall guards);
src/ccid_handler.rs + crates/ccid-core/src/handler.rs:369-397 (dispatch), :1003-1090
(escapes); crates/ccid-transport-serial/src/lib.rs (codec, limits 271/274);
src/t1.rs (:39-57 T=1 constants); src/wifi.rs (:89-95 power-save off, 7 ms floor);
host-tools/nucula-ccid-broker.py + .service; sdkconfig.full:1233; mission/decisions.md
+ mission.log (144 heals, 13 orphans).
