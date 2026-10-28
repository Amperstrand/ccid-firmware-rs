# PC/SC lane instances — frozen system instance + private serial-DUT instances

**Status: DRAFT PROPOSAL — zero bench verification yet.** Every mechanism
below is source- or man-page-cited; every behavioral claim about the bench
itself is a TODO (§9). Kanban: `t_937eee48`.

- Author lane: 2026-10-21, branch `docs/pcscd-lane-instances`.
- Scope: host-side pcscd management on the ai-legion bench (192.168.13.208).
  No firmware changes.
- Bench pcscd at time of writing: **Debian pcscd 2.0.3-1build1**
  (`pcscd --version`: features `libsystemd serial usb libudev polkit
  usbdropdir=/usr/lib/pcsc/drivers ipcdir=/run/pcscd filter
  configdir=/etc/reader.conf.d`, `PCSCLITE_MAX_READERS_CONTEXTS: 16`).
  Source citations are given for BOTH the installed 2.0.3
  (upstream tag `2.0.3` = `c4e7f6f9c6fe56fafd3f13c31fcc4f670ad6d022`) and
  the in-repo quote-only pin `reference/pcsc-lite/` @ `6cadd81`
  ("2.5.2", provenance in `reference/PROVENANCE.md`) wherever they were
  both checked.

## 1. Prior art (searched, honestly: none for this exact design)

Search performed 2026-10-21 across `/root/src` (all repos + the
`/root/src/worktrees/*` farm), `/tmp/opencode`, `/root/handoffs`, and
`grep pcscd AGENTS.md`:

- **In this repo**: no existing multi-instance pcscd design. The nearest
  in-repo prior art is the **flash-window quiesce machinery** in
  `tests/hardware/labgrid/esp32_flash.py` (`pcscd_lock_acquire` /
  `pcscd_lock_release` / `recover_stale_pcscd_down`, the
  `.pcscd-down` marker, `quiesce_unit` for the nucula broker — Codex P1
  on #299 and the #314 broker lesson). That machinery *stops the one
  system instance* under `/run/lock/bench-pcscd.lock`; it does not create
  instances. `docs/PCSC_HOST_CONTRACT_AUDIT.md` (#141) audits the
  firmware-vs-pcscd contract, not instance management.
- **Sibling repos**: `tollgate-lab` carries `tollgate_lab/hardware/pcscd_domain.py`
  + `pcscd_ctl.py` (bolty-rs B32 lessons): a reader/disturbance
  SH/EX flock that *coordinates* restarts of the single system pcscd —
  the closest prior art in the fleet, and it explicitly models the
  coupling this design removes rather than any second instance.
  `bolty-rs` has a `hil/pcscd-reader-role` branch (same coordination
  class). `boltcard-cloudflareworker/scripts/pcscd-bridge.py` is an
  HTTP-over-pyscard client of the default instance.
- **Upstream**: pcsc-lite has no documented multi-instance mode; the
  mechanisms used here (socket activation, `--config`, client socket
  env, name filter) are each documented single features, verified in §3.

**Conclusion: this is a first-of-kind proposal for the bench, not a
write-up of something already half-built.** Everything below is design
until §9 passes.

## 2. The problem (bench pain, as it stands)

The system pcscd (Debian `pcscd.socket` + `pcscd.service`, socket at
`/run/pcscd/pcscd.comm`) carries **every reader on the bench**:

| Source | Entry (read from `/etc/reader.conf.d/`, 2026-10-21) | Class |
|---|---|---|
| hotplug (udev) | Cherry ST-2xxx DUTs (`ST2XXX-001` F469, `ST2XXX-F746` F746), OMNIKEY CardMan 3121 ref | USB CCID |
| `acr1252-if0` | ACR1252 ref, interface-pinned via `usb:072f/223b:libudev:…` DEVICENAME | USB CCID (path-pinned) |
| `m5stick-gempc` | `M5stick GemPC (MFRC522)` — libccidtwin on the Hades2001 FTDI | **serial DUT** |
| `nucula-broker` | `Nucula CCID via broker` — libccidtwin on the broker PTY | **serial DUT** |
| `modemifd.conf` | `NR7101_SIM_bench` — `libifd_modemifd.so` to 192.168.12.124:7002 | network IFD (conwrt-bench spike #21) |
| `vpcd-boltcard`, `vpcd-nucula-35967` | virtual readers via `libifdvpcd.so` | virtual IFD |

Two standing failure classes follow from this single shared instance:

1. **Serial-DUT work must stop the system pcscd.** Any ESP32 flash or
   raw-serial battery on a DUT whose reader libccidtwin would claim goes
   through `pcscd_lock_acquire` (stop BOTH `pcscd.socket` and `pcscd` —
   socket-activation respawns the daemon onto the reader port mid-flash,
   the documented first failure of the 2026-10-19 session) and holds the
   bench flock for the whole window. **Every other lane's USB reader
   work is blocked** for the duration: HIL suites, perf runs, gpg
   sessions — all PC/SC clients lose the daemon.
2. **The t_fe356d5c hygiene issue**: the vpcd/modemifd IFDs wedge
   `scdaemon` bench-wide (recorded in the J3R180 runbook findings:
   "scdaemon wedges uninterruptibly on the host's shared reader set
   (vpcd/modemifd IFDs) — drive gpg through pinned readers only"). Any
   client that enumerates the shared instance inherits the wedging
   readers.

## 3. What pcscd/pcsc-lite actually supports (verified — do not guess past this table)

All citations checked 2026-10-21 against the installed 2.0.3 (binary,
`man pcscd`, `man reader.conf`, `/lib/systemd/system/pcscd.{socket,service}`)
and upstream sources at tag `2.0.3` and pin `6cadd81` (2.5.2).

| Capability | Supported? | Citation |
|---|---|---|
| Per-instance reader.conf.d (`-c, --config dir`) | **Yes** | `man pcscd` (2.0.3); `pcscd --help`. Serial readers load at start via `RFStartSerialReaders(PCSCLITE_CONFIG_DIR)` (`pcscdaemon.c` ~:661 @2.0.3) |
| Foreground mode (`-f, --foreground`) | **Yes** | `man pcscd`; used by Debian's own unit |
| Auto-exit after 60 s idle (`-x, --auto-exit`) | **Yes** | `man pcscd`; stock `pcscd.service` ships it |
| **Client** socket override env | **Yes — `PCSCLITE_CSOCK_NAME`, full socket path** | `src/winscard_msg.c` `SocketName_init()`: `getenv("PCSCLITE_CSOCK_NAME")` else compile-time default (@2.0.3 :92, @2.5.2 :91). Confirmed in the installed `libpcsclite.so.1.0.0` (`strings`: `PCSCLITE_CSOCK_NAME`). Client liveness check `SCardCheckDaemonAvailability()` stats exactly this name (`winscard_clnt.c` :3551-3569 @2.0.3) |
| `PCSCLITE_CARDSOCKET` | **Does not exist** | grep of both source trees: zero hits. (Correcting the kanban brief's guess; the real variable is `PCSCLITE_CSOCK_NAME`.) |
| `PCSCD_SOCKET_DIR` env / `--socket-dir` flag | **Does not exist** | Absent from `pcscd --help`, `man pcscd`, and `print_usage()` in both sources. (Also correcting the kanban brief.) |
| **Daemon** bind path override | **NO — compile-time only** | `src/winscard_msg_srv.c`: `strncpy(sa.un.sun_path, PCSCLITE_CSOCK_NAME, …)` uses the *macro*, not the env (@2.0.3 :143, @2.5.2 :139); `ipcdir=/run/pcscd` baked into the Debian build |
| Two pcscd instances without socket activation | **NO — second bare start refuses** | `pcscdaemon.c` :473-492 @2.0.3: `stat(PCSCLITE_CSOCK_NAME)` + live pid from `/run/pcscd/pcscd.pid` → "Another pcscd (pid: %ld) seems to be running." → `EXIT_FAILURE` |
| systemd socket activation (fd-passed socket at an arbitrary path) | **Yes** | `sd_listen_fds` → `SocketActivated` → `ListenExistingSocket(SD_LISTEN_FDS_START + 0)` instead of `InitializeSocket()` (`pcscdaemon.c` :448-462, :723-727 @2.0.3; installed banner lists `libsystemd`; binary strings contain `ListenExistingSocket`, `sd_is_socket`). Skips the second-instance check; at exit does **not** remove the socket file (owned by systemd) |
| Keep an instance off the USB readers | **Env name filter only** (no flag disables hotplug) | `PCSCLITE_FILTER_IGNORE_READER_NAMES` (colon-separated substrings, daemon env): checked at the TOP of `RFAddReader` — a match returns `SCARD_E_READER_UNAVAILABLE` *before the IFD driver loads*, so the USB device is never opened (`readerfactory.c` :199-232 @2.5.2, `#ifdef FILTER_NAMES`; installed 2.0.3 banner lists the `filter` feature and its man page documents the envs). Bench proof pending (§9 TODO-4) |
| Per-user instance socket paths | **NO** | Same compile-time daemon path; nothing in either source keys the socket off the uid |
| `pcscd -H` (serial-reader rescan) with multiple instances | **Racy** | `-H` = `SendHotplugSignal()` → `GetDaemonPid()` reads the single shared `/run/pcscd/pcscd.pid` (`utils.c` :54-110 @2.0.3) |
| Shared-state hazard: pid file | **Yes — unconditional** | Every instance *writes* `/run/pcscd/pcscd.pid` after init (`pcscdaemon.c` :681-706 @2.0.3) and *removes* it at exit (`clean_temp_files()`, :831-847 — the socket-file removal is guarded by `!SocketActivated`, the pid removal is NOT) |

Implications that shape the design:

- A private instance **cannot** be "just `pcscd -f -c <dir>` with an env
  var" — the stock daemon would try to bind `/run/pcscd/pcscd.comm` and
  exit. The one supported coexistence route on a stock build is
  **systemd socket activation**, i.e. template units.
- Clients (pyscard, pcsc_scan, scdaemon's pcsc backend — anything on
  libpcsclite) follow the instance by **`PCSCLITE_CSOCK_NAME` in their
  own environment**; nothing about the reader set changes for clients
  that don't set it (they keep talking to the system instance).
- The **pid file is shared mutable state**; with more than one instance
  it lies (§6 has the one genuinely dangerous follow-on and its
  mitigations).

## 4. Design

### 4.1 Freeze the SYSTEM instance to USB-only readers

Move every non-USB entry out of the system config dir. Concretely,
`/etc/reader.conf.d/` after the freeze keeps ONLY:

- `acr1252-if0` (USB reader pinned by path — stays: it is USB work), and
- `libccidtwin` (reference comments, no active entry; harmless).

and the active serial/virtual entries move to per-lane dirs (§4.2):
`m5stick-gempc`, `nucula-broker`, `modemifd.conf`, `vpcd-boltcard`,
`vpcd-nucula-35967`.

The system instance then serves: USB hotplug readers (both Cherrys,
CardMan, any incidental CCID) + the path-pinned ACR1252. USB readers
never needed reader.conf.d (`man reader.conf`: "USB readers SHALL NOT
be configured using these files" — the ACR entry is our deliberate
path-pinning exception), so **the system instance needs no restart
discipline change beyond the one-time freeze restart**, and — important
consequence — **stopping it is no longer required by any serial-DUT
flash** (its libccidtwin/modemifd/vpcd ports are gone).

The stock units stay exactly as Debian ships them
(`pcscd.socket`: `ListenStream=/run/pcscd/pcscd.comm`, `SocketMode=0666`;
`pcscd.service`: `Requires=pcscd.socket`,
`ExecStart=/usr/sbin/pcscd --foreground --auto-exit $PCSCD_ARGS`).

### 4.2 Per-lane PRIVATE instances — systemd template units

Two unit files, instantiated per lane (`m5stick`, `nucula`, `boltcard`,
`nr7101-modem`, …). Proposed contents:

```ini
# /etc/systemd/system/pcscd-lane@.socket
[Unit]
Description=PC/SC lane instance activation socket (%i)

[Socket]
# flat path under /run: socket units do not create parent dirs,
# and /run/pcscd-lane-<name>.comm needs no mkdir at boot
ListenStream=/run/pcscd-lane-%i.comm
SocketMode=0666

[Install]
WantedBy=sockets.target
```

```ini
# /etc/systemd/system/pcscd-lane@.service
[Unit]
Description=PC/SC lane instance (%i) — serial DUT readers only
Requires=pcscd-lane@%i.socket
Documentation=man:pcscd(8)

[Service]
ExecStart=/usr/sbin/pcscd --foreground --auto-exit \
  --config /etc/reader.conf.d.lanes/%i
# keep the lane instance OFF every USB reader: matched at the top of
# RFAddReader, driver never loads, device never opened (§3, TODO-4)
Environment=PCSCLITE_FILTER_IGNORE_READER_NAMES=Cherry:ACS:OMNIKEY
ExecReload=/bin/kill -USR1 $MAINPID

[Install]
Also=pcscd-lane@%i.socket
```

Notes on the choices:

- **Template name `pcscd-lane@` (not `pcscd@`)**: avoids any unit-name
  collision with stock `pcscd.socket`/`pcscd.service` and makes lane
  instances grep-able (`systemctl list-units 'pcscd-lane@*'`).
- **`--auto-exit`**: an idle lane instance exits after 60 s; systemd
  re-activates it on the next client connect. No lane leaves a daemon
  parked on a serial port forever (the modemifd/vpcd wedge class moves
  from "always loaded" to "loaded while a lane actually works").
- **`--config /etc/reader.conf.d.lanes/%i`**: that directory contains
  exactly one lane's entries (e.g. `m5stick` → the `m5stick-gempc`
  entry verbatim). `-c` also accepts a single file
  (`man pcscd`) — a one-reader lane can be a single file, but a
  directory keeps multi-entry lanes (nucula: broker + vpcd) uniform.
- **Filter string**: colon-separated substrings matched with `strstr`
  against the full reader name. Current bench USB names all contain
  `Cherry` (both ST-2xxx DUTs), `ACS` (ACR1252), or `OMNIKEY`
  (CardMan 3121). Lane reader names (`M5stick GemPC (MFRC522)`,
  `Nucula CCID via broker`, `NR7101_SIM_bench`,
  `Virtual PICC boltcard vpcd`, `Nucula VPCD wifi67`) match none of
  them. **Rule: any new USB reader on the bench must extend this
  list** (and any new lane FRIENDLYNAME must not contain those
  substrings — `GemPC` is safe, `Twin` is NOT if a Cherry filter
  substring ever widens; keep substrings brand-specific).
- **`--disable-polkit`** is available if a lane instance ever hits a
  polkit denial (bench runs as root; expected to be a non-issue,
  noted for completeness).
- **Why not a private `pcscd -f` under the lane flock without systemd?**
  Because it cannot bind a second socket on a stock build (§3, row 4/8).
  Building a private pcscd with a different `ipcdir` would work but is a
  host-fork maintenance burden — rejected on the same grounds as the
  ccid speed fork (#54): a local fork of security-relevant host
  middleware for a convenience the supported route already provides.

### 4.3 Client connection (the env that selects the instance)

Lane-scoped clients export the FULL socket path:

```bash
export PCSCLITE_CSOCK_NAME=/run/pcscd-lane-m5stick.comm
pcsc_scan                # pyscard / pcsc_scan / opensc-tools all follow
pytest tests/hardware/nfc/test_differential.py -v --hil ...
```

- Applies to **any libpcsclite client**: pyscard (all bench python),
  `pcsc_scan`, OpenSC tools, and scdaemon's pcsc backend (`gpg` inherits
  the env: `PCSCLITE_CSOCK_NAME=… gpg --card-status`).
- Unset env = system instance = USB readers only after the freeze.
  Clients that previously found the m5stick/nucula readers in the
  default instance MUST set the env — §8 lists them.
- Reader selection *within* the instance stays by stable name per the
  labgrid doctrine (AGENTS "pcscd owns logical reader access") — the env
  changes which daemon, not the name-matching rule.

### 4.4 Lifecycle tied to the lane flock (extends the esp32_flash pattern)

The existing machinery (`esp32_flash.py`) already has every shape
needed; lane instances just become `systemctl` units instead of the
global ones:

- **Per-lane lock**: `/run/lock/bench-pcscd-<lane>.lock` (new). Lane
  instance start/stop and any lane serial-port quiesce happen under
  THIS lock. The SYSTEM lock (`/run/lock/bench-pcscd.lock`) remains
  authoritative for system-instance operations only (now rare: the
  one-time freeze restart, future USB-reader host maintenance).
  Doctrine amendment to propose in AGENTS when this lands: *"any pcscd
  stop/start goes through the flock of the instance it touches."*
- **Flash quiesce for a serial DUT** (e.g. m5stick): stop
  `pcscd-lane@m5stick.socket` AND `pcscd-lane@m5stick.service` under
  the lane lock (socket activation makes the pair mandatory — the
  exact class `esp32_flash` fixed for the system units), flash, then
  start both back. The system instance and every other lane keep
  serving. This is the design's headline win.
- **Marker + stale recovery**: same pattern as
  `recover_stale_pcscd_down()`: a `<lane-lock>.pcscd-down` marker laid
  down at quiesce, removed at restore; any lane tooling (or the flash
  CLI's next invocation) detects a stale marker + free lock and
  restarts the lane units.
- **Broker co-ordination** (`quiesce_unit`, #314): the nucula's
  `nucula-ccid-broker` unit keeps its existing treatment — stopped
  under the SAME held lane lock before a nucula flash, restarted in
  restore.

### 4.5 Bootstrap command lines (runbook form)

```bash
# -- one-time: units + lane conf dirs (as root on the bench) ----------
install -m 644 pcscd-lane@.socket pcscd-lane@.service /etc/systemd/system/
systemctl daemon-reload
mkdir -p /etc/reader.conf.d.lanes/m5stick          # one dir per lane
# move ONLY that lane's entries (§4.1 list) into its dir, e.g.:
git -C /root/src/ccid-firmware-rs show HEAD:...    # keep copies in-repo
# (entries are admin state on the bench; back them up like
#  /root/reader-conf-backup/ has been used before)

# -- enable a lane's socket (on-demand activation thereafter) ---------
systemctl enable --now pcscd-lane@m5stick.socket

# -- lane work ---------------------------------------------------------
export PCSCLITE_CSOCK_NAME=/run/pcscd-lane-m5stick.comm
pcsc_scan &      # first connect activates the instance (auto-exits @60s idle)

# -- flash window for that lane's DUT ----------------------------------
flock /run/lock/bench-pcscd-m5stick.lock systemctl stop \
  pcscd-lane@m5stick.socket pcscd-lane@m5stick.service
#   ... esptool / battery that owns the port ...
flock /run/lock/bench-pcscd-m5stick.lock systemctl start \
  pcscd-lane@m5stick.socket pcscd-lane@m5stick.service
```

(The runbook-level integration — a `tests/hardware/lane_pcsc.py` helper
owning lock+units+marker the way `esp32_flash.py` owns the system
window — is the natural follow-up PR once this design is approved;
this doc deliberately ships no code.)

## 5. What the design buys

1. Serial-DUT lanes stop blocking USB-reader work: a m5stick/nucula
   flash quiesces only its own lane instance.
2. The t_fe356d5c scdaemon wedge class loses its host-wide reach:
   vpcd/modemifd IFDs load only inside lane instances, so a
   system-scoped scdaemon never sees them at all.
3. `systemctl stop pcscd` under the bench flock becomes rare (USB
   readers need no reader.conf.d; nothing else lives in the system
   instance).
4. Lane instances are disposable: `--auto-exit` + socket activation
   means a wedged lane instance is `systemctl restart
   pcscd-lane@<lane>` — no bench-wide blast radius.

## 6. Hazards and mitigations

1. **Shared pid file (verified, §3)**: every instance writes/removes
   `/run/pcscd/pcscd.pid`. Consequences and mitigations:
   - `pcscd -H` signals whatever pid the file last received — treat
     `-H` as system-instance-only; lane instances are short-lived and
     get a fresh conf on every activation, so they never need `-H`
     (use the template's `ExecReload` kill if ever required).
   - **The one dangerous path**: pid file holds a *dead lane pid* +
     `/run/pcscd/pcscd.comm` exists (system instance live) + someone
     starts a **bare, non-activated** `pcscd` → `GetDaemonPid()` reads
     the dead pid → `ESRCH` → "the old pcscd is dead" →
     `clean_temp_files()` **removes the live system socket file**.
     Mitigation: bench rule (AGENTS amendment) — *pcscd is only ever
     started through its units, never bare*; plus keep lane instances
     `--auto-exit`ed when idle so the window where a stale lane pid
     sits in the file is small. The stock socket-activation start of
     the SYSTEM instance is immune (skips the check entirely).
2. **Filter miss = USB contention**: if a new USB CCID reader appears
   on the bench and isn't in `PCSCLITE_FILTER_IGNORE_READER_NAMES`,
   the lane instance will hotplug-claim it and fight the system
   instance (the ACR1252-wedge class). Mitigation: the §4.2 rule +
   TODO-5 (a bench check that asserts the filter covers every present
   USB reader).
3. **Client env forgetting**: a lane test that forgets
   `PCSCLITE_CSOCK_NAME` silently talks to the SYSTEM instance and
   reports "reader absent". Mitigation: helper/fixture-level env
   injection (follow-up PR) + the §8 inventory.
4. **Socket dir creation**: avoided by flat `/run/pcscd-lane-%i.comm`
   paths (socket units do not create parent directories).
5. **`/etc/default/pcscd` `PCSCD_ARGS`** applies to the stock unit
   only; lane units take args explicitly. Keep debug flags per-lane
   (`-d` on a lane instance is now SAFE to leave on — another
   instance-scope payoff; on the system instance it stays off per
   current practice).

## 7. Migration steps (ordered; each gated by §9 checks)

1. Back up `/etc/reader.conf.d/` (extend `/root/reader-conf-backup/`).
2. Install the two template units; `daemon-reload`; enable NOTHING yet.
3. Create `/etc/reader.conf.d.lanes/{m5stick,nucula,boltcard,nr7101-modem}`
   and MOVE each lane's entries there (§4.1 list). Coordinate the
   modemifd move with conwrt-bench (their spike, their lane).
4. Bench-verify a lane instance in isolation (TODO-1..4) BEFORE
   touching the system instance.
5. Freeze restart: under `/run/lock/bench-pcscd.lock`,
   `systemctl restart pcscd.socket pcscd.service`; verify the system
   instance lists USB readers only (TODO-5) and that a USB-reader HIL
   suite passes unchanged (TODO-6).
6. Flip lane consumers one lane at a time (m5stick first — its flash
   path is the documented pain), each with its §9 gate.
7. Rollback = move the entries back into `/etc/reader.conf.d/`, one
   flock'd restart, disable the template sockets. Lane conf dirs and
   units can stay dormant.

## 8. What breaks (inventory of consumers that must learn the env)

Files that talk PC/SC to the default instance today and target
serial/virtual readers (grep `SCardEstablishContext|pyscard` over
`tests/hardware` + `host-tools`, 2026-10-21):

- `tests/hardware/nfc/conformance_battery.py` — its `pcscd_stop()`
  (:268) stops the SYSTEM units; the pcscd-mode cases against the
  m5stick move to the lane instance (env + lane-unit stop/start under
  the lane lock instead of the system flock).
- `tests/hardware/nfc/test_differential.py`, `test_gpg.py`,
  `turnaround_probe.py`, `t1_retry_probe.py` — reader-name based;
  lane targets need the env.
- `tests/hardware/perf/perflib.py` (+ conftest) — same.
- `tests/hardware/test_ccid_apdu.py`, `test_seedkeeper_non_destructive.py`,
  `test_gemalto_pysatochip.py`, pinpad suites — reader-name based; same.
- `tests/hardware/labgrid/conftest.py` (`pcscd_running` fixture,
  `systemctl start pcscd.socket pcscd.service`) and
  `bench.py` (the m5stick-vpcd liveness signal) — system-scope parts
  stay valid; lane-scope parts need env/unit awareness.
- `tests/hardware/nfc/J3R180-RUNBOOK.md` + `j3r180_gpg_lifecycle.py` —
  gpg/scdaemon against the m5stick/nucula readers: wrap invocations in
  the lane env. (The runbook's scdaemon-wedge warning itself relaxes
  once vpcd/modemifd leave the system instance.)
- `host-tools/` consumers (e.g. anything pyscard-based) — same rule.

Non-breakage worth stating: STM32 USB HIL suites, the perf baselines
against ACR/CardMan, and every USB-reader test keep working with NO env
and NO code change — the system instance keeps those readers.

## 9. Verification TODO (all bench-gated; none done)

> **PILOT RESULT (2026-10-21, ad-hoc mechanism — not the template
> units):** TODO-1/TODO-2 cores proven by the wifi-vpcd lane under fire.
> Mechanism used: `systemd-socket-activate -l /run/pcscd-lanes/wifi67.sock
> -- /usr/sbin/pcscd --foreground -c /etc/reader.conf.d-lanes/wifi-verify`
> (socket activation = the §3 coexistence route; no unit files needed for
> the pilot) + `PCSCLITE_CSOCK_NAME` on every client (pyscard AND gp/JNA
> both honor it). Proven: the private instance served exactly
> `Nucula VPCD wifi67 00 00/00 01`, carried the FULL J3R180 lifecycle
> (gp auth, install, applet ops, uninstall, baseline restore) GREEN
> while the SYSTEM instance was externally stopped/restarted TWICE by a
> concurrent lane's flash windows — zero impact on the run (TODO-2's
> isolation half; system-side reader service during the pilot not
> asserted). `vpcd-nucula-35967` stayed in the lane dir, i.e. the §4.1
> freeze is real for that one entry. Observed non-blocking caveats:
> USB hotplug readers fail to add inside the private instance (system
> owns them — the TODO-4 filter env would hide the noise); shared
> `/run/pcscd/pcscd.pid` written by both (known §3 hazard). AGENTS.md
> now carries the runnable recipe. Remaining for §7: TODO-3/TODO-4/
> TODO-5 + migrating the other serial entries + the template units.

- [ ] **TODO-1**: `systemctl start pcscd-lane@m5stick.socket` + one
  client connect with the env → instance serves exactly
  `M5stick GemPC (MFRC522) 00 00`, connect+ATR+SELECT round-trips
  (the §4.5 smoke).
- [ ] **TODO-2**: while the lane instance is UP, the system instance
  still serves USB readers to env-less clients (two daemons, one host —
  the §3 socket-activation claims on the installed 2.0.3).
- [ ] **TODO-3**: lane flash-window drill: stop lane units under the
  lane lock, `flash_esp32`-style port ownership on the FTDI, restart;
  assert the system instance never dropped a reader (the headline win,
  measured).
- [ ] **TODO-4**: strace/usbmon-style proof that the lane instance
  with the filter env NEVER opens a USB CCID device node while a
  Cherry/ACR/CardMan is present (the RFAddReader-early-return reading,
  §3 — source-read, not yet bench-proven).
- [ ] **TODO-5**: post-freeze: system instance lists USB readers only;
  `gpg --card-status` via scdaemon on a USB reader no longer wedges on
  vpcd/modemifd (t_fe356d5c regression check).
- [ ] **TODO-6**: USB-reader HIL suite green with no env change
  (`pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil`).
- [ ] **TODO-7**: pid-file hazard drill: kill -9 a lane instance,
  observe marker/pid state, restart via units only; document the
  observed (not predicted) recovery.
- [ ] **TODO-8**: `--auto-exit` behavior: idle lane instance exits at
  ~60 s and re-activates on the next connect (stock-unit parity).

## 10. Open questions

1. Does the nucula wifi-vpcd serving mode (both ESP32s serving VPCD
   readers, AGENTS 2026-10-15 delta) want a *standing* lane instance
   (no `--auto-exit`, socket unit only) for its always-on readers?
   Cheap either way — decide when that lane migrates.
2. `MAX_READERNAME` (128) and `PCSCLITE_MAX_READERS_CONTEXTS` (16) per
   instance — no bench lane comes close; noted so nobody rediscovers
   them.
3. Whether the lane helper (§4.5 note) should live in
   `tests/hardware/labgrid/` next to `esp32_flash.py` or as
   `host-tools/` — follow-up PR decision.

## 11. References

- Installed: `man pcscd(8)`, `man reader.conf(5)`,
  `/lib/systemd/system/pcscd.{socket,service}`, `pcscd --version`
  (2.0.3-1build1, features incl. `libsystemd`, `filter`).
- Upstream pcsc-lite tag `2.0.3` (`c4e7f6f9…`): `src/winscard_msg.c`
  (client env, :92), `src/winscard_msg_srv.c` (daemon bind, :143),
  `src/pcscdaemon.c` (single-instance guard :473, pid write :681,
  socket activation :448/:723, `clean_temp_files` :831),
  `src/utils.c` (`GetDaemonPid`/`SendHotplugSignal` :54-110).
- In-repo quote-only pin `reference/pcsc-lite/` @ `6cadd81` (2.5.2 —
  `reference/PROVENANCE.md`): `src/readerfactory.c` (filter, :199-232)
  and the same files at the same shapes (line numbers in §3 cite 2.5.2
  where only that tree was read).
- Bench machinery: `tests/hardware/labgrid/esp32_flash.py` (lock /
  marker / recover / quiesce_unit), `AGENTS.md` (Labgrid Bench
  Doctrine, pcscd serialization rule, ESP32 serial wedge table).
- Kanban: `t_937eee48` (this design), `t_fe356d5c` (scdaemon hygiene
  issue this design addresses); J3R180 runbook finding (CHANGELOG
  [Unreleased]) records the wedge symptom.
