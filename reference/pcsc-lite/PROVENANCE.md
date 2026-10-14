# pcsc-lite quote-only reference subset

Quote-only copy (vendoring policy class 1, AGENTS.md → "Reference-First
Doctrine" → "Vendoring policy (three classes)"): never compiled, never
linked. Trimmed to exactly the files the PC/SC host-contract audit
(issue #141) cites. Full pcsc-lite remains upstream; do not grow this
copy beyond "what we quote".

| Field | Value |
|---|---|
| Upstream | https://github.com/LudovicRousseau/PCSC (GitHub mirror of salsa.debian.org/rousseau/PCSC) |
| Pin | `6cadd8152f3cfdb79131a65843d9b85f01f0ae51` — "Release 2.5.2" (2026-09-19) |
| License | BSD-3-Clause (upstream `COPYING`; per-file headers retained verbatim) |
| Vendored | 2026-10-14, issue #141 audit lane |

## What we quote and why

| File | Audit use (issue #141) |
|---|---|
| `src/winscard.c` | `SCardGetAttrib` refusal semantics (`IFD_ERROR_TAG` → `SCARD_E_UNSUPPORTED_FEATURE`, pcscd answers FRIENDLY/SYSTEM_NAME itself, winscard.c:1401-1426); `SCardTransmit` carries **no host-side timeout** (the whole patience budget is the IFD driver's, winscard.c:1491-1653) |
| `src/winscard_svc.c` | Daemon-side `SCard*` RPC dispatch — what the pcscd service layer enforces before winscard.c is reached |
| `src/readerfactory.c` | Reader-init capability queries (`TAG_IFD_THREAD_SAFE` :355, `TAG_IFD_POLLING_THREAD_WITH_TIMEOUT` :411, `TAG_IFD_SLOTS_NUMBER` :440, `TAG_IFD_SIMULTANEOUS_ACCESS` :765); reader naming for multi-reader benches |
| `src/eventhandler.c` | Presence-poll loop shape: 400 ms cadence, power-off grace state machine (eventhandler.c:455-540) |
| `src/prothandler.c` | PPS policy: T=1 preferred, always calls `IFDSetPTS`, fallback + `SET_PROTOCOL_PPS_FAILED` semantics (prothandler.c:60-160) |
| `src/ifdwrapper.c` + `.h` | The exact IFDH_* call sites pcscd makes (PowerICC :309, ICCPresence :363, Control :428/:479, TransmitToICC :541, GetCapabilities :256) |
| `src/pcscd.h.in` | Host constants: `PCSCLITE_STATUS_POLL_RATE` 400 000 µs (:53), `PCSCLITE_POWER_OFF_GRACE_PERIOD` 5 s (:70), `PCSCLITE_STATUS_EVENT_TIMEOUT` 10 min (:74) |
| `src/PCSC/reader.h` | PC/SC v2 part 10 FEATURE_* table (25 feature codes) — the feature vocabulary |
| `src/PCSC/ifdhandler.h` | IFD handler API contract: `TAG_IFD_*` codes, `IFD_*` response codes, IFDH semantics comments |
| `src/PCSC/pcsclite.h.in` | `SCARD_ATTR_*` table + `SCARD_PROTOCOL_*`/`SCARD_E_*` codes |
| `doc/reader.conf.5.in` | reader.conf(5) conventions (FRIENDLYNAME/DEVICENAME/LIBPATH/CHANNELID) — the serial-twin enrollment surface |

Cross-references: the driver-side counterpart (what answers these
queries for us) is the CCID driver, vendored as the `reference/CCID`
submodule @ `1a0f75d` — see `reference/README.md`. The serial-twin
specifics (synthesized descriptor, echo, 3 s read window) live there in
`src/ccid_serial.c`.
