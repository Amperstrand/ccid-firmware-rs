# WiFi transport for pcscd — decision record (#154)

Status: DECIDED — **native network IFD driver (the vpcd lineage) over any
socat/PTY bridge**, with the reversal the bench already proved. Supersedes
the 2026-10-12 librarian prior-art scan; this record is grounded in
bench-proven evidence through 2026-11-01.

## The three candidate shapes

### A. socat PTY bridge (the 2026-10-12 architecture) — RETIRED IN PRACTICE
`pcscd (libccidtwin) ↔ /dev/pts (socat) ↔ TCP ↔ firmware-serial-protocol`

Failure classes, all observed (issue body, 2026-10-12): zombie backlog
clients, half-open TCP after board resets (socat learns only on write —
no keepalive), double-spawn supervisor/watchdog races (13 orphans), pcscd
wedges on PTY state, and the bridge is a process soup. Additional ceiling:
libccidtwin hardcodes **B115200** on its serial side (reference:
`reference/CCID/src/ccid_serial.c` WriteSerial) — the PTY shape imports
the serial driver's limits into a network path.

### B. native IFD driver speaking the GemPC wire over TCP
A `libifd-nucula.so` that connects out and speaks GemPC frames natively.
Removes socat + PTY entirely. Cost: a new C driver surface to own
(the ifd-nucula driver exists and is mature — see the #450/#453/#455
lineage: TX semantics, error mapping, frame bounds — so this is an
extension of an audited codebase, not a greenfield one).

### C. vpcd reversed connection (vsmartcard lineage) — **SHIPPED AND PROVEN**
Board connects OUT to a host listener; pcscd rides libifdvpcd's slots.
This is what the bench runs today:

- End-to-end pcscd → Wi-Fi → card APDUs proven repeatedly (2026-10-28/29,
  issue #467 evidence): ATR + GET DATA through `Nucula VPCD wifi67`.
- OTA-primary on the same transport (#467): image pushes, slot switch,
  mark-valid, rollback-under-brownout — both directions bench-proven.
- The stock driver's two upstream bugs (empty-ATR eject in
  `recvFromVICC`, unbounded recv) are already patched in our deployed
  `libifdvpcd-amperstrand.so` (#326/#359 lineage) — the patched shape is
  the deployable one.
- Host-side operational notes that are now documented: per-slot listeners
  (:35963/4, :35967/8) need ufw rules on any new host (bit us on
  ai-legion-small 2026-10-31), and per-board `NUCULA_VPCD_PORT` bakes a
  deterministic board↔reader mapping (no accept race).

## Decision and rationale

1. **Keep C (vpcd reversed) as the production wifi transport.** Every
   alternative requires new driver code; C is proven on this bench with
   the failure modes already patched and documented.
2. **B is the upgrade path if vpcd's slot model ever binds** (e.g. a
   product needing GemPC wire compatibility without the vsmartcard
   listener model) — implemented as an extension of ifd-nucula.
3. **A is retired.** Any new socat/PTY proposal must beat C's evidence
   bar, and the 2026-10-12 failure classes are disqualifying absent a
   specific mitigations list.
4. Serial-over-USB remains the RESCUE path (not the product path):
   wifi-first operation is proven; serial is for bootloader pushes and
   wedge recovery (USBDEVFS_RESET discipline, #117).

## Known live caveats on C (tracked, not blockers)

- Load-class brownouts on the c3 rail (#489) degrade OTA install
  reliability (~half need retry) — board-level power fix pending; the
  transport itself is sound.
- pcscd-level reconnect behavior across board reboots is bounded by the
  patched driver's eject/re-poll semantics — acceptable, monitored.
- The listener ports are a small per-host ops surface (ufw + reader.conf
  sync when migrating hosts — see the 2026-10-31 small-host migration).
