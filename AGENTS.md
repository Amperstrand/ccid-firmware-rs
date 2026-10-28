# AGENTS.md — ccid-firmware-rs Project Knowledge Base

## Project Overview

**ccid-firmware-rs** is a Rust USB CCID (Integrated Circuit(s) Card Interface Device)
firmware implementing the USB CCID class specification (Rev 1.1) for smartcard
readers. It targets two MCU families with three smartcard frontends:

- **STM32F469-DISCO** — USB CCID over USB OTG FS, contact smart cards via USART2
  smartcard mode (ISO 7816-3). Default build target.
- **STM32F746-DISCO** — USB CCID over USB OTG FS, contact smart cards via GPIO
  bit-banging (no smartcard-mode USART).
- **ESP32** — Serial CCID over UART0 (GemPC Twin framing, 115200 8N2), NFC cards
  via MFRC522 over I2C (primary) or PN532 over SPI (secondary).

The firmware advertises itself on USB as one of three reference commercial
readers (Cherry SmartTerminal ST-2xxx, Gemalto IDBridge CT30, Gemalto IDBridge
K30) so that existing host drivers (pcscd, OpenSC, etc.) recognise it without
custom drivers.

Repository: https://github.com/Amperstrand/ccid-firmware-rs
License: GPL-2.0-or-later

## Workspace Layout

Root `Cargo.toml` is a pure workspace manifest (no `[package]`). `.cargo/config.toml`
sets NO default build target (only per-target runner/rustflags) — every STM32
build/objcopy invocation must pass `--target thumbv7em-none-eabihf` explicitly.
(2026-10-16 lesson, #210: this file used to claim the target was defaulted;
because it is not, CI `cargo objcopy` compiled the bin for the HOST triple and
every uploaded firmware artifact was a host-ELF dump until the flags landed.)
ESP32 commands must be run from `firmware/esp32-ccid/` or with an explicit
target override.

```
ccid-firmware-rs/
├── Cargo.toml                  # workspace manifest, profiles, [patch] tables
├── crates/                     # shared, host-buildable libraries
│   ├── ccid-protocol/          # CCID protocol types, constants, ATR parsing
│   ├── card-interface/         # card frontend trait + PresenceState (no_std)
│   ├── ccid-core/              # CCID response builders, PPS validation, shared handler (65 tests)
│   └── ccid-transport-serial/  # GemPC Twin serial framing (25 tests)
├── firmware/
│   ├── ccid-firmware/          # STM32 USB CCID firmware (default-members)
│   └── esp32-ccid/             # ESP32 serial CCID firmware (MFRC522/PN532)
├── host-tools/                 # host-side tooling
├── vendor/                     # tracked patched dependencies (see below)
│   └── stm32f469i-disc/        # patched F469I-DISCO BSP (sdio-support, #193)
├── reference/                  # osmo-ccid-firmware submodule + CCID reader specs
└── tests/hardware/             # hardware integration test procedures
```

### `[patch]` tables (root Cargo.toml)

One crate is overridden by a git-fork patch (rev-pinned, byte-identical
to the former vendor copy), one by a locally tracked vendor path (the
BSP — see below). Both are patch-required; upstream sources do not
build for us:

```toml
[patch.crates-io]
# synopsys-usb-otg: retired from vendor/ to the canonical fork (#264,
# 2026-10-18) — upstream has no F469 feature and crates.io is stuck at
# 0.5.0; the fork carries F469 endpoint re-enablement + generated-RAL
# warning suppression. rev = the exact former submodule pin.
synopsys-usb-otg = { git = "https://github.com/Amperstrand/synopsys-usb-otg", rev = "764bc042f63cf00d0efd2417141438c1584cd187" }

# Patched BSP — local copy of Amperstrand/stm32f469i-disc sdio-support
# (fa6dc86) with its HAL dep realigned to this repo's pin 05d999d (the BSP
# pins ancestor 789e5e8; two HAL instances made display-init Pin/Rcc types
# from different crates — issue #193).
[patch."https://github.com/Amperstrand/stm32f469i-disc.git"]
stm32f469i-disc = { path = "vendor/stm32f469i-disc" }
```

**esp-idf-hal is NOT patched anymore** (retired 2026-10-17, PR #242 /
#149 P3): the Amperstrand fork (`ai-experiments-0.47`, whose only delta
over upstream was the `legacy-i2c` opt-in) went away when its last
consumers (backend-mfrc522, the m5stick LCD's AXP192) moved to the
shared driver_ng embedded-hal adapter (`pn7160-nci::hal_i2c`).
esp-idf-hal resolves from crates.io (v0.47.0, the exact base the fork
was rebased onto) — see D-9 in `docs/DECISION_REGISTER.md`.
**Runtime caveat (#278, 2026-10-18)**: "exact base" holds at COMPILE
time only — registry hal links IDF's legacy i2c object, whose boot
constructor aborts alongside driver_ng. Every `.full` sdkconfig + the
defaults now carry `CONFIG_I2C_SKIP_LEGACY_CONFLICT_CHECK=y` (IDF's
documented bypass; the fork's real runtime role was excluding that
object from the link). Compile-green ≠ boot-green: this class of change
needs a bench boot proof (the knob must be IN the authoritative `.full`
files — `sdkconfig.defaults` does not layer into an existing full
config; and a changed sdkconfig needs the documented CMakeCache purge
to take effect).

The MFRC522 driver is NOT vendored: `esp32-ccid` consumes the canonical
`Amperstrand/mfrc522-rs` fork at `ai-experiments` rev `e9ced1e` (git dep),
the same source bolty-rs uses — the former divergent `vendor/mfrc522` copy
was removed (issue #19).

The ISO 14443 crate is also NOT vendored: `esp32-ccid` consumes the canonical
`Amperstrand/iso14443-rs` fork (`ai-experiments` branch, git dep),
the same source bolty-rs uses — the former `vendor/iso14443-rs` copy
was removed (issue #6).

### Release profile (root Cargo.toml)

```toml
[profile.release]
debug = 2           # full DWARF (probe-rs RTT location info)
opt-level = "z"     # size optimization (embedded flash constraint)
lto = true
codegen-units = 1
panic = "abort"     # no unwinding

[profile.release.package.esp32-ccid]
opt-level = "s"     # ESP32 uses "s" instead of "z"

[profile.dev]
debug = 2
opt-level = 1       # faster dev iteration
```

Release mode is **required** for reliable USB behaviour with `synopsys-usb-otg`.
Do not ship dev builds.

## MCU Profiles

### STM32 profile selection (mutually exclusive)

Defined in `firmware/ccid-firmware/Cargo.toml`. Exactly one MCU feature and one
device-profile feature must be active.

| MCU Feature | Board | Smartcard Frontend | Default? |
|---|---|---|---|
| `stm32f469` | STM32F469-DISCO | USART2 smartcard mode (hardware ISO 7816-3) | ✓ |
| `stm32f746` | STM32F746-DISCO | GPIO bit-bang (no smartcard USART) | |

```toml
[features]
default = ["stm32f469", "profile-cherry-smartterminal-st2xxx"]

# MCU target selection (mutually exclusive)
stm32f469 = ["dep:stm32f4xx-hal", "dep:stm32f469i-disc"]
stm32f746 = ["dep:stm32f7xx-hal"]
```

### USB device profiles (mutually exclusive)

Reference: `reference/CCID/readers/*.txt` (authoritative device specifications).

| Profile Feature | Device | VID:PID | PIN Pad | Default? |
|---|---|---|---|---|
| `profile-cherry-smartterminal-st2xxx` | Cherry SmartTerminal ST-2xxx | 046A:003E | ✓ Yes | ✓ |
| `profile-gemalto-idbridge-ct30` | Gemalto IDBridge CT30 | 08E6:3437 | No | |
| `profile-gemalto-idbridge-k30` | Gemalto IDBridge K30 | 08E6:3438 | No | |

> **⚠️ IMPORTANT — only Cherry ST-2xxx has PIN pad support.**
> The K30 (PID:3438) is a basic reader, virtually identical to CT30 (PID:3437).
> A prior version of the firmware falsely claimed PIN pad + LCD capabilities for
> the K30 profile; this was corrected (see `CHANGELOG.md` Unreleased). The real
> K30 has `bPINSupport=0x00`, `wLcdLayout=0x0000`, and uses TPDU exchange level
> (`dwFeatures=0x00010230`), not Short APDU (`0x00020472`).

> **Bench-distinct iSerial (2026-10-11, commit 0af0fb0):** the F746 Cherry
> build enumerates with iSerial `ST2XXX-F746`; the F469 keeps `ST2XXX-001`.
> Both boards share the Cherry USB identity (046A:003E) — pcscd and the
> labgrid DUTs-disjoint-by-identity doctrine need distinct serials when both
> are connected. The reference files do not constrain iSerial
> (CherrySmartTerminalST2XXX.txt has no serial field), so the override is
> doctrine-justified. Single definition point:
> `device_profile::USB_SERIAL_NUMBER` (cfg-gated on
> `stm32f746`+`profile-cherry-smartterminal-st2xxx`).

### ESP32 backend selection

ESP32 firmware lives in `firmware/esp32-ccid/` and has its own feature set:

| Backend Feature | NFC Chip | Bus | Default? |
|---|---|---|---|
| `backend-mfrc522` | MFRC522 | I2C (M5Stack Atom Matrix) | ✓ |
| `backend-pn532` | PN532 | SPI | (secondary, still supported) |

## Reader-Role vs Standalone-Role T=1 Doctrine (owner directive 2026-10-12)

**Two roles, deliberately different T=1/protocol ownership — never mix them
on one wire path.** "T=1" and "the secure channel" are two DIFFERENT
layers that can be owned independently — the layer table that makes the
doctrine precise:

| Layer | Reader role (STM32 USB, TPDU) | GemPC serial emulation (ESP32 UART, m5stick/SPP) | Nucula CDC/broker | Standalone role (provisioning mains) |
|---|---|---|---|---|
| T=1 block transport (ISO 7816-3 §11: I/S/R blocks, chaining, WTX, EDC) | **host** — blocks relayed byte-transparent, never inspected/re-framed | **device** (`t1.rs` answers S-blocks, reassembles I-block INF — exactly what the real GemPC Twin does; what `libccidtwin` expects) | ISO-DEP handles blocks below us — device never sees T=1 blocks | device |
| APDU / TPDU payload | host | host (APDUs reassembled from I-blocks, relayed) | **device relays APDUs only** (APDU-level relay) | device |
| GP secure channel (SCP02/03) + card lifecycle | **host** — a host↔card SC must pass through byte-transparent | **host** — the SC rides the APDUs; the device never terminates it in this role | host | **device** (GP client logic lives here, in SEPARATE mains only) |

The rule that falls out of the table: in EVERY reader-facing column the
secure channel belongs to the host; the only thing the device may own
is transport plumbing (and on the GemPC-emulation path only because the
real hardware does). ccid-firmware-rs is the project where the
host/pcscd owns the channel; device-owned SC work belongs to standalone
mains (bolty-style provisioning), never inside a pcscd serving path.

1. **Reader role (pcscd-facing CCID, STM32 USB)**: the HOST owns the T=1
   protocol and the secure channel; the reader is a transparent TPDU-level
   block relay. Implemented and locked by the reference-first profiles:
   all three device profiles advertise TPDU exchange level (Cherry
   reference `dwFeatures=0x000100BA` → TPDU; CT30/K30 `0x00010230` →
   TPDU) and `smartcard.rs` `transmit_raw` forwards complete T=1 blocks
   untouched — only T=0 procedure-byte handling is on-device (standard
   for CCID TPDU level, `libccid` sends the full T=0 TPDU and expects
   the full response). **Rule: in the reader role we never inspect,
   reassemble, re-frame, or answer T=1 content** — a host-card secure
   channel (GP SCP02/03) must pass through byte-transparent. Issue #132
   (card-specific SCP02 mismatch through our path) is a bug in this
   relay's electrical/receive layers, NOT license to add protocol logic.
2. **Standalone role (device-driven card provisioning)**: firmware itself
   flashes/personalizes cards (GP install flows, wallet-style use). Here
   the device legitimately terminates protocols and runs GP client logic.
   This must live in a SEPARATE main/feature (like the pn7160-actdiag /
   bring-up mains), never inside the pcscd serving path.
3. **ESP32 serial GemPC-Twin emulation**: `t1.rs` answers S-blocks locally
   and reassembles I-block INF to APDUs — that is what the REAL GemPC
   Twin does and what `libccidtwin` expects; reference-justified, but it
   is the #101 RX-hardening weak spot. The USB-CDC nucula path
   (`ccid_serial_server`) stays APDU-level relay (NFC ISO-DEP handles
   blocks below us).

Stale-doc note: the repo README's "XfrBlock: Short APDU level" line
predates the descriptor correction — the profiles are TPDU level.

## Card-Handling Doctrine (2026-10-12 ISD-lock incident — never repeat)

**What happened:** a transport-fidelity hammer sent ~450 INITIALIZE UPDATE
commands in a few minutes against the SmartPGP card (CPLC 50947320) to
stress the T=1 relay. INIT_UPDATE is the FIRST step of the GP secure
channel: it makes the ISD issue a challenge. JCOP counts every
abandoned/failed authentication attempt against a SMALL non-renewable
error budget. The hammer exhausted it; the card now answers `69FF` to all
secure-channel-class commands, survives VCC power-off (persistent EEPROM
state — commercial-reader-verified, not a relay artifact), and the only
standard recovery path (a successful authentication resetting the counter)
is impossible because INIT_UPDATE itself is refused. **Effectively
permanent loss of GP management on that card** (applets on it remain
fully usable; only install/delete/re-personalize died). Incident record:
#127/#132.

**Rules:**

1. **Never send SC-class commands in loops.** SC-class = anything that
   initiates or feeds the GP secure channel: INITIALIZE UPDATE (80 50),
   EXTERNAL AUTHENTICATE (84 82), and SC-gated GET DATA (e.g. 00E0).
   Each one is a budget-bearing authentication event on JCOP-class cards.
2. **Load/stress/fidelity tests use the zero-risk set ONLY:** SELECT by
   AID (00 A4 04 00), GET DATA CPLC (80 CA 9F7F 00), and applet-level
   reads of already-selected applications. These are pure reads — no
   security state, no counters.
3. **GP authentication attempts are budgeted and ledgered.** Track every
   attempt per CPLC serial on #127 (current: 50947320 ISD locked;
   50972320 at 3 attempts; 50942720 at 2 mismatches + working AUTH
   triple; nucula-coil card unprobed). One aimed attempt per NEW keyset
   with provenance — never speculative retries.
4. **Any new hammer/soak design gets checked against this table before
   its first run** — including "read-only-looking" GP tooling: `gp -l`,
   `gp -i`, `gp --info` ALL attempt authentication and burn budget on
   failure.
5. **There is NO master-key recovery once the ISD lockout trips** —
   researched 2026-10-12 (#144): the 69FF refusal is pre-authentication
   (identical at key-ver 00/01 — the card refuses before key-set lookup),
   so knowing the correct keys does not help; every GP reset path needs
   a successful authentication first, which needs INIT_UPDATE, which is
   the refused command. Only NXP vendor re-personalization could undo
   it. Card **50947320 is the permanent record of this** (#144): a
   perfectly good SmartPGP single-applet card forever — applet fixture
   only, no GP install/delete.

**Safe/unsafe table for test design:**

| APDU / tool | Class | Loopable? |
|---|---|---|
| SELECT by AID | pure read | ✅ unlimited |
| GET DATA CPLC (80CA 9F7F) | pure read | ✅ unlimited |
| Applet GET DATA / app commands (e.g. OpenPGP 00CA 004F) | applet read | ✅ unlimited (mind PIN rules) |
| Raw INIT_UPDATE (80 50 …) — "no auth, just probing" | **SC initiation** | ❌ **NEVER in a loop** |
| `gp -l` / `-i` / `--info` / `--list` with wrong keys | full auth attempt | ❌ budgeted singles only |
| INIT_UPDATE + EXT_AUTH with correct keys | completes session (resets counters on some masks) | ⚠️ fine, but pointless in a loop |

## Reference-First Doctrine (owner directive 2026-10-11)

**We do not reinvent wire formats, timings, or protocol bytes by trial
and error. Before writing or "fixing" any protocol-facing constant:
find the reference, verify against it, and lock it with a unit test.**

The founding incident: our PN7160 CORE_SET_CONFIG TLV was wrong for
WEEKS (trailing garbage octet the chip rejected) because a session
guessed the TLV format instead of checking the kernel/NXP references —
and wrote the wrong story into this file, which later sessions trusted
(the #110 lesson again: docs lie until re-verified against the tree).

### The rules

1. **No assumptions on wire formats.** Every protocol byte we emit or
   parse must trace to a citation: spec section, kernel struct, NXP
   header, or bench-proven reference bytes. "I think it's 2 octets" is
   a bug, not a fact.
2. **Check references BEFORE bench iterations.** Tools: z.ai zread
   (`zread_read_file`/`zread_search_doc` on GitHub repos), grep.app
   (literal code search across public repos), web search, and the
   in-repo `reference/` + `docs/nfc_citation_map.md`. One reference
   lookup beats ten flash-and-probe cycles.
3. **GREAT EXPECTATION annotations.** Reference-derived byte sequences
   carry an inline annotation near the constant AND a unit test that
   locks the bytes (`tx_frames_match_c_driver_bytes` style). If the
   test breaks, the breaker must re-verify against the citation —
   never "fix" the test to match new unverified bytes.
4. **Mocks must model the reference, not our bug** (mock/hardware
   divergence is this repo's #1 bug class).
5. **DRY = single place to be right.** Shared logic lives in
   `crates/` (ccid-core handler, pn7160-nci, transport-serial). A wire-
   format fact must exist in ONE place — where it is already
   reference-locked and tested.
6. **Known-good states + known unknowns are tracked as GitHub issues**
   (checklists, `- [ ]`, with commit hashes). When hardware misbehaves,
   check the issue tracker + the bench table below BEFORE assuming a
   hardware defect — and use the documented full-reflash ladder to get
   back to a known good state.

### Fix-all-siblings protocol (owner directive 2026-10-12)

DRY is the FIX protocol, not just a new-code rule. The tree serves the
same protocol through several sibling paths — STM32 USB (`firmware/ccid-firmware`
ccid_core wrapper), esp32 serial (`ccid_handler` + `CcidSerialServer`,
shared by m5stick/nucula/SPP mains), shared crates (`crates/ccid-core`,
`ccid-transport-serial`), and per-firmware escape bolt-ons. When a bug
is fixed anywhere:

1. **Before writing the fix, grep the sibling paths for the same
   pattern** — same constant, same silent clamp (`.min(` on a copy),
   same parse, same timeout, same escape-code handling. The known
   sibling-groups: relay/APDU buffer capacities (now derived from
   `t1::MAX_RELAY`), escape tables, diagnostics extensions, presence
   retry patterns, T=1 block builders.
2. **If the logic can live in a shared crate without mixing the
   reader-role vs standalone-role doctrines** (see the T=1 doctrine
   above), REFACTOR it there and point every path at it — one fix
   lands everywhere. Prefer exporting the existing constant/function
   over duplicating its value at a second site.
3. **If the paths must legitimately differ** (electrical frontends,
   USB vs UART transport, role doctrine), fix EVERY sibling in the
   SAME session and cross-reference them in the commit/issue.
4. **Never leave a known-fixed bug alive in a sibling path** — a fix
   that only lands on one board is how the mock/hardware-divergence
   bug class breeds (G-03 was exactly this shape: one handler, but the
   capacity constant lived separately from the assembler's).

### Vendoring policy (three classes)

Three distinct things get called "vendoring" — only two are allowed:

1. **Quote-only reference copies** (`reference/…`, never compiled,
   never linked): ALLOWED and encouraged. Pin provenance (upstream repo
   + commit) in `reference/README.md`, keep upstream license headers,
   trim to what we quote; full upstream repos stay git submodules.
   PDF-only authorities get verbatim-quote markdown under
   `docs/quote-sources/` with section/page cites (the specquotes.toml
   approach). The index of all tracked sources: `reference/README.md`.
2. **Build-path vendored code** (`vendor/…`, compiled): ONLY when we
   patch it and the patch cannot live in a fork/git dep (the patched
   `vendor/stm32f469i-disc` BSP copy, #193, is the one remaining legal
   case). The synopsys-usb-otg vendor copy was RETIRED 2026-10-18
   (#264): its patch always could ride a fork/git dep, so it now does
   (Amperstrand fork, rev-pinned — vendoring-policy applied, not a new
   precedent). The esp-idf-hal story is
   HISTORY in three states, each retirement deliberate: the vendored
   copy was migrated to the canonical `Amperstrand/esp-idf-hal` fork,
   `ai-experiments` branch (2026-10-15, issue #195), and the fork
   itself was then RETIRED (2026-10-17, PR #242 / #149 P3): its only
   delta (`legacy-i2c` opt-in) lost its last consumers to the driver_ng
   migration, and esp-idf-hal rides crates.io v0.47.0 again (D-9).
   Un-patched vendored copies were deliberately removed (issues #19,
   #6) — do not reintroduce that class; linkable code goes through
   canonical git deps/forks.
3. **C headers as build dependencies**: NOT vendored — our HAL comes
   from the Amperstrand Rust fork. C headers (ST HAL smartcard,
   ST USB middleware) are vendored ONLY as class-1 quote sources.

### The reference library (verified useful)

| Topic | Reference | Where |
|---|---|---|
| NCI core (SET/GET_CONFIG, INIT, DISCOVER) | `include/net/nfc/nci.h`, `net/nfc/nci/core.c` | torvalds/linux (grep.app or zread) |
| NCI param IDs, status codes, TLV walker | `nci_defs.h` (1-octet param IDs!) | NXPNFCLinux/linux_libnfc-nci, NXPNFCProject/NFC_NCIHAL_libnfc-nci |
| PN7160 specifics, TOTAL_DURATION example | UM11495 §6.2 | docs/nucula-campaign/nfc_citation_map.md |
| C3 USB-Serial/JTAG registers (no RST_DIS on C3) | `usb_serial_jtag_reg.h` | espressif/esp-idf, components/soc/esp32c3 |
| CCID Rev 1.1 | DWG_Smart-Card_CCID_ReV110 | docs/CCID_SPEC_AUDIT.md + `// CCID_SPEC:` markers |
| T=1 (ISO 7816-3 §11) | Table 45 PCB bits | firmware/esp32-ccid/src/t1.rs citations |
| Reader identities | reference/CCID/readers/*.txt | authoritative device specs |

### Clean-wire doctrine (inviolable)

The CCID wire carries CCID frames ONLY. libccidtwin's parser is a
strict byte stream with NO resync — any ASCII between frames desyncs
pcscd for the whole session. Since 2026-10-12 (issue #131) this is
enforced by CONSTRUCTION, not discipline: the log ceiling
(`set_max_level(Off)`) is set BEFORE the protocol driver claim, right
after the one allowed boot banner (FWID + "rust main ALIVE", which
ends before serving starts). The #91 log-shim (ring + drain + purge)
is RETIRED — deleted, not fixed: keeping logs alive on a protocol wire
was machinery that itself became the bug (its 5.5.5 drain breakage was
the port's only regression). Debug output goes to: Escape 0xD0
counters, Escape 0xD1 coredumps (dump-and-retrieve), a `ble` build, a
`pn7160-actdiag` diagnostic build, or WiFi netlog — never unsolicited
on the CCID wire. Enforcement: `test_wire_silence_after_banner`
(tests/hardware/nucula) asserts zero printable ASCII after the banner.
Full analysis + alternatives matrix: `docs/LOGGING_DOCTRINE.md`.

### Debug-channel lesson (2026-10-12, #131 — read before adding ANY
### diagnostics to a time-sensitive path)

When troubleshooting feels hard because the debug channel interferes
with the thing being debugged, DELETE the channel — do not build
machinery to route around the interference. Rules for the future:

1. A protocol wire is not a debug channel; strict parsers treat
   unsolicited bytes as corruption (correctly).
2. Diagnostics are PULL (in-band escape queries) or POST-MORTEM
   (coredumps) — never push.
3. Logging perturbs timing at the moment it matters (print-path
   stalls, console starvation during busy-waits, driver-claim
   semantics) — for timing bugs the observation must not perturb the
   observed (dump-and-retrieve).
4. Live logs belong on dedicated diagnostic surfaces: a diagnostic
   BUILD (actdiag pattern), a separate wire (RTT/JTAG, dedicated
   UART), or a non-protocol radio channel (ble/netlog).
5. If a workaround subsystem grows (rings, drains, purges, flush
   ordering) to keep observation alive inside a protocol path, that
   is the architecture telling you the channel should not exist.
   Complexity added to preserve observation becomes the next
   incident (log_shim: 619 lines, multiple bench sessions, and it
   still broke first on the IDF bump).
6. Lock the invariant with a wire-silence regression test — CI
   failure beats reviewer vigilance.

### Nucula C3 hardware wall (register-verified 2026-10-11)

The ESP32-C3 USB-Serial/JTAG peripheral chip-resets (`rst:0x15
USB_UART_CHIP_RESET`) on the host DTR/RTS patterns that pcscd's
libccidtwin uses on every serial-reader open/handshake. The C3
register map has **no disable bit** (`usb_serial_jtag_reg.h`: no
`CHIP_RST_DIS` — that bit exists only on C6/C61 silicon). Therefore:

- **The nucula cannot serve as a pcscd/libccidtwin serial reader by
  DIRECT CDC opens** — but since 2026-10-11 it DOES serve pcscd through
  the PTY broker (`host-tools/nucula-ccid-broker.py`, systemd service
  `nucula-ccid-broker`, reader.conf entry `nucula-broker`): the broker
  is the only CDC opener (safe opener, serving-gated symlink), pcscd
  talks to the PTY where DTR/RTS is a no-op. Bench-verified: enumerate,
  connect+ATR+SELECT sessions, >5.5 min stability, ~0.5 ms broker
  overhead (#117).
  Every pcscd port-open resets the chip; the boot banner races the
  handshake; the reader error-loops (bench-verified end-to-end
  2026-10-11). The bench reader.conf is disabled
  (`/etc/reader.conf.d/nucula-ccid.disabled`) to stop the reset vortex.
- **Direct-serial CCID WORKS and is the nucula's test surface**: GemPC
  framing over the CDC with the safe opener (construct `Serial()`,
  set `dtr=False; rts=False` BEFORE `open()`), ~300 ms/command CDC
  latency floor. ATR + APDU relay bench-verified.
- Paths to a pcscd-capable nucula-class device: C6/S3 silicon (has
  `USB_UART_CHIP_RST_DIS`), an external USB-UART bridge (FTDI, like
  the m5stick), or USB-OTG class CCID (S2/S3 only).

## Host pcscd instances — system + lane-private (2026-10-21)

**Read this before any pcscd-visible bench work on ai-legion.** The
SYSTEM pcscd (socket `/run/pcscd/pcscd.comm`) carries every reader
config in `/etc/reader.conf.d/` and is STOPPED/RESTARTED by any
ESP32 flash window (`esp32_flash.py`) — concurrent lanes kill each
other's runs this way (two lifecycle runs died to it 2026-10-21).

- `/etc/reader.conf.d/vpcd-nucula-35967` (the wifi-vpcd wifi67 reader)
  lives in the LANE dir `/etc/reader.conf.d-lanes/wifi-verify/` — the
  system instance intentionally does NOT load it. wifi-vpcd bench work
  runs against a PRIVATE pcscd instance (recipe below). Full migration
  of the remaining serial entries is `docs/PCSD-LANE-INSTANCES-DESIGN.md`
  §7 (owner-gated steps; §9 now carries the pilot's bench proof).
- **Private-instance recipe (bench-proven 2026-10-21, ran the full
  J3R180 lifecycle green through two external system-pcscd restarts):**
  ```sh
  mkdir -p /run/pcscd-lanes /etc/reader.conf.d-lanes/<lane>
  # put ONLY your lane's reader.conf entries in the lane dir
  systemd-socket-activate -l /run/pcscd-lanes/<lane>.sock -- \
      /usr/sbin/pcscd --foreground -c /etc/reader.conf.d-lanes/<lane> &
  # then EVERY client (pyscard, gp/JNA, scripts) needs:
  export PCSCLITE_CSOCK_NAME=/run/pcscd-lanes/<lane>.sock
  ```
  Mechanism per PCSD-LANE-INSTANCES-DESIGN §3 (socket activation skips
  the second-instance check; the daemon otherwise hardcodes the socket
  path). Caveats: the instance still writes the shared
  `/run/pcscd/pcscd.pid` (harmless lie; `-H` is racy), and USB hotplug
  readers fail to add inside it (system instance owns them — cosmetic).
  Kill the `systemd-socket-activate` process to tear down.

## CI Testing Protocol


CI lives in `.github/workflows/ci.yml`. All jobs check out with
`submodules: true` (required — see Gotchas).

### Rust toolchain policy (2026-10-12; FLEET STANDARD 2026-10-15) — the map and the why

House stance: **stable-first, early adoption by deliberate bumps,
nightlies ONLY where upstream forces them.** Every pin is exact and
carries its reason.

| Context | Toolchain | Why |
|---|---|---|
| Host crates, STM32 firmware, all `cargo install` tooling | **Stable, exact-pinned: 1.99.0** — the root `rust-toolchain.toml` is the single authority (rustfmt + clippy + `thumbv7em-none-eabihf` declared) | nothing we ship needs unstable features; the exact pin keeps CI, the release Docker build (#115), and every bench checkout bit-identical. 1.92.0 was retired when modern tooling (espflash 4.x class) stopped building under it |
| ESP32 — BOTH targets (xtensa m5stick/m5atom AND C3 nucula) | **pinned esp fork `v1.98.1.0`** + crosstool `esp-14.2.0_20260121` — `espup install -c 14.2.0_20260121 -v 1.98.1.0 --targets esp32,esp32c3` | upstream rustc has no xtensa-espidf backend (fork is the only compiler there), and no `-Zbuild-std`-capable stable exists for espidf std — so BOTH targets ride the fork (2026-10-12 one-Rust-version change: same version NUMBER as the stable pin, one vendor-frozen compiler instead of fork + dated upstream nightly). `-v` is load-bearing: espup installs the LATEST fork by default (a moving nightly-derived compiler) |
| CI mirrors | exact same strings: dtolnay `1.99.0`, `cargo +esp`, espup pins `-c 14.2.0_20260121 -v 1.98.1.0` | one authority; the strings must match the toml + bench EXACTLY |
| Upstream `jmcorgan/fips` builds (conwrt/fieldlab lanes) | **1.94.1** — upstream's own `rust-toolchain.toml` pin (their README/release notes: "Rust 1.94.1 per rust-toolchain.toml", edition 2024) | align with what upstream does; NOT installed fleet-wide — rustup auto-restores it on demand when a fips checkout is entered |

**FLEET STANDARD (2026-10-15 cleanup — the only three allowed):**

1. **1.99.0** — every Amperstrand Rust repo pins it via `rust-toolchain.toml`;
   CI installs read the toml (`dtolnay/rust-toolchain@master` with no
   version input, or `actions-rust-lang/setup-rust-toolchain@v1` like
   upstream fips). Repos aligned tonight: bolty-rs, conwrt, micronuts,
   physical-router-test-automation, tollgate-rs, tollgate-rs-amperstrand
   (each carries the fleet-standard commit). ccid was already exact.
   NOT touched (other sessions' in-flight checkouts — align when their
   lanes close): cln8-wt (detached HEAD), prta-secrets-og (feature
   branch); micronuts' push rides with its active session.
2. **esp fork 1.98.1.0** — structural, esp-idf targets only.
3. **1.94.1** — upstream-fips alignment, on-demand only.

Nightly exception: instrumented fuzzing legs (micronuts
`fuzz-nightly.yml`, ccid `fuzz-nightly.yml` — 2026-10-17, mirrors the
fleet pattern) — structural, documented in place. Bench .208 now
carries exactly: `stable`, `1.99.0`, `esp` (11.5 GB of dead toolchains
removed: 1.92, 1.92.0, 1.94.1, 1.97.1, 1.98.1-stable,
nightly-2026-09-29; 1.94.1/1.97.1 auto-restore from their checkouts'
tomls when those lanes run). The 2026-10-14 lesson stands: env first —
a bare non-login shell resolves toolchain names differently than a
login shell (issue #199's root cause); `. ~/export-esp.sh` before every
ESP build.

**Rules:**

1. **Stable by default; bumps are deliberate, not accidental.** Track
   current stable; move the pin when tooling stops building under the
   old one, or on a planned cadence. A bump must pass the full gate
   before landing: `cargo fmt --all --check`, BOTH STM32 clippy passes
   (`-D warnings`: f469 default + f746 bitbang), STM32 release build,
   workspace host tests. (2026-10-12's 1.92→1.99 migration surfaced
   zero new lints because the gate ran first.)
2. **Nightly-class only under a hard upstream constraint** — esp-idf's
   std story (see below). Since 2026-10-12 there are ZERO upstream
   nightlies in the fleet: both esp32 targets ride the pinned fork.
   Never a floating nightly or an unpinned espup (the `-v 1.98.1.0`
   pin is as load-bearing as the crosstool `-c` pin).

   Why a nightly-CLASS compiler is structural for esp-idf targets
   (bench-verified 2026-10-12 — this recurs, don't re-derive):
   `*-espidf` std links against ESP-IDF's newlib libc, which only
   exists inside an IDF build tree, so no stable-channel std exists
   (`rustup target add` has nothing to offer; the fork's rustlib
   carries only host+src — everything build-stds). For the pinned fork +
   global `[unstable] build-std` config, std is ALWAYS built from source
   (the fork's rustup toolchain ships rust-src but no per-target
   precompiled std; esp-idf-ssys documents `-Zbuild-std=std,panic_abort`
   as required) — a migration that omits rust-src or build-std fails
   (Codex #148). espup's `-b/--stable-version` precompiled-std model is
   a different, C3-only route that would need config restructuring. RESOLUTION (2026-10-12, later the
   same day): the dated upstream nightly is RETIRED — both esp32
   targets now ride the PINNED fork (v1.98.1.0 via espup -v), one
   compiler, same version number as stable. Zero upstream nightlies.
   Future option if we ever want C3 on true upstream stable: espup's
   `-b/--stable-version` RISC-V model (stable + espup-installed
   precompiled std) — requires config restructuring ([unstable]
   build-std is crate-global) and was not needed for the consolidation.
3. **Exact version strings everywhere.** `1.99` and `1.99.0` are
   DISTINCT rustup toolchains. The 2026-10-12 CI outage was exactly
   this mismatch: main's toml said `1.92.0`, ci.yml's dtolnay pin said
   `1.92` — the toml override shadowed the action's install with a
   component-less twin (no cargo-fmt → lint dead; no thumbv7em →
   builds dead).
4. **One authority per domain.** Root toml owns host+STM32.
   build.sh/CI `+toolchain` owns the per-board ESP32 split (env and
   explicit `+toolchain` beat the toml by design). No per-directory
   tomls inside `firmware/esp32-ccid/` — a single file cannot express
   the xtensa/C3 split and would fight build.sh.
5. **CI tool installs run under the toml's stable** (`cargo install
   espup ldproxy` …): keep installed tools stable-compatible or pin
   their versions — the espflash-under-1.92 breakage was this class.

### Jobs

The full ci.yml job set (tier policy: the SECONDARY tier runs on push to
main, schedule, dispatch `full-matrix`, or a PR labeled `ci-full-matrix` —
never blocking ordinary PRs; `ci-ok` aggregates the primary tier only,
owner directive 2026-10-17 "a secondary rot must never block all merges"):

| Job | Tier | Purpose | Commands |
|---|---|---|---|
| `changes` | — | paths-filter (PR) / all-true (schedule + dispatch) — decides which families run | dorny/paths-filter |
| `sdkconfig-clean-wire-gate` | primary | sdkconfig console-family pairing check | `python3 tools/check_sdkconfigs.py` |
| `stm32-build` | primary | STM32 build, Cherry default profile (+ bin/ihex artifacts) | `cargo build --release --target thumbv7em-none-eabihf` + `cargo objcopy` |
| `stm32-build-secondary` | secondary | STM32 builds: CT30 / K30 / F746-bitbang / F469-display (steps mirror `stm32-build`, kept in sync) | same as `stm32-build` per matrix entry |
| `stm32-lint` | primary | fmt + clippy for both MCU targets | `cargo fmt --check`, `cargo clippy ... -- -D warnings` |
| `stm32-test` | primary | Host-side workspace tests | `cargo test --workspace --target x86_64-unknown-linux-gnu` |
| `esp32-build` | primary | ESP32 compile matrix: m5stick / m5stick-lcd / c3-nucula-bringup / c3-nucula-ccid | `cargo +<matrix.toolchain> build <matrix.profile> --target <matrix.target>` per leg |
| `esp32-build-secondary` | secondary | ESP32 legs: m5atom-default / pn532 / m5stick-spp / wifi-ccid / m5stick-vpcd-bench | same per-leg build |
| `esp32-host-test` | primary | ESP32 host-side tests (no Xtensa toolchain in CI) | `cargo test --target x86_64-unknown-linux-gnu` (from `firmware/esp32-ccid/`) |
| `crates-lint` | primary | Quality blind-spot closer (2026-10-17): the three members NO other job clippy'd (pn7160-nci, ccid-pcsc-host, ccid-host-tools) + pn7160-nci's never-compiled no_std cfg | `cargo check -p pn7160-nci --no-default-features`; `cargo clippy -p pn7160-nci -p ccid-pcsc-host -p ccid-host-tools -- -D warnings` |
| `python-collect` | primary | Collection gate in a MINIMAL env (pytest only — no pyscard/pyserial/labgrid) + ruff correctness lint (F+E9, `ruff.toml`) + stdlib unit tests for scripts | `ruff check scripts tests/hardware host-tools`; `python3 scripts/test_gen_status.py`; `python3 -m pytest tests/hardware/ --collect-only -q` |
| `ci-ok` | — | required-check aggregator over the PRIMARY tier only | — |

**Why `python-collect` exists** (found live 2026-10-11): a duplicate
`--hil` `addoption` in `labgrid/conftest.py` made every combined-suite
invocation die with `ValueError` — and module-level pyscard imports plus
import-time `sys.exit(1)` in collected scripts are the same class.
`py_compile` (the old stm32-lint step) cannot see any of it; only real
collection can. The gate runs with pytest + stdlib ONLY, deliberately:
the documented no-hardware path (`pytest tests/hardware/ -v`, everything
skips without `--hil`) must collect without any hardware deps. Rule:
**hardware imports belong inside functions/fixtures, never at module
scope of a collected file** — a script named `test_*.py` that needs
pyscard up front uses the `if "pytest" in sys.modules:
pytest.skip(..., allow_module_level=True)` guard (see
`tests/hardware/test_ccid_apdu.py`).

Second rule of the same class (found live 2026-10-12): **test modules
never bare-import their conftest** (`from conftest import ...`). When
two suite directories are collected together, whichever conftest.py
claims the bare `conftest` module name first wins and the other suite
dies with ImportError — empirically both
`pytest tests/hardware/labgrid/ tests/hardware/nfc/` and the reverse
order broke collection while the single-dir and full-tree runs passed
by traversal luck. Shared helpers live in uniquely-named modules:
`labgrid/bench.py`, `labgrid/hil_helpers.py`, `nfc/cardlib.py` —
conftests re-import from there, tests import the module directly.

### stm32-build matrix — primary (1 entry) + secondary tier (4 entries)

The 4-profile matrix is SPLIT across two jobs by tier (the old single
"4 entries" table predates the split):

```yaml
# stm32-build (primary — every PR):
matrix:
  include:
    - profile: profile-cherry-smartterminal-st2xxx   # default features
      features: ""

# stm32-build-secondary (secondary tier — push/schedule/dispatch
# full-matrix/PR-label ci-full-matrix only):
matrix:
  include:
    - profile: profile-gemalto-idbridge-ct30
      features: "stm32f469,profile-gemalto-idbridge-ct30"
    - profile: profile-gemalto-idbridge-k30
      features: "stm32f469,profile-gemalto-idbridge-k30"
    - profile: stm32f746-bitbang
      features: "stm32f746,profile-cherry-smartterminal-st2xxx"
    - profile: stm32f469-display
      features: "stm32f469,profile-cherry-smartterminal-st2xxx,display"
```

The primary entry builds with **default features** (no
`--no-default-features` override). All secondary entries use
`--no-default-features --features <list>`.

### stm32-lint clippy runs (two passes, both must be clean)

```bash
# Pass 1 — F469 default (uses default features)
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf -- -D warnings

# Pass 2 — F746 bitbang
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf \
  --no-default-features --features "stm32f746,profile-cherry-smartterminal-st2xxx" -- -D warnings
```

### ⚠️ CRITICAL GOTCHA — issue #25: default features MUST include `stm32f469`

The `stm32-lint` job's first clippy pass runs with **no `--no-default-features`
flag**, which means it builds whatever `[features] default = [...]` declares.

**If `default` does not include `stm32f469`, that clippy pass fails** because
none of the MCU-specific HAL crates get pulled in and the `cfg`-gated modules
(`smartcard.rs`, USB PHY reset block, etc.) have nothing to compile against.

**Rule:** the `default` feature set in `firmware/ccid-firmware/Cargo.toml` MUST
always include exactly one MCU feature (`stm32f469`) and exactly one profile
feature. The current correct default is:

```toml
default = ["stm32f469", "profile-cherry-smartterminal-st2xxx"]
```

Do not change `default` to `[]` or to an ESP32/MFRC522 combination — CI clippy
will break. Issue #25 documents this exact regression.

### Reproducing CI locally

```bash
# Full STM32 host test suite (fast, no hardware)
cargo test --workspace --target x86_64-unknown-linux-gnu

# STM32 F469 default build (what CI ships as the Cherry profile)
cargo build --release --target thumbv7em-none-eabihf

# STM32 F469 default clippy (CI gate)
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf -- -D warnings

# STM32 F746 bitbang build
cargo build --release --target thumbv7em-none-eabihf \
  --no-default-features --features "stm32f746,profile-cherry-smartterminal-st2xxx"

# STM32 F746 bitbang clippy (CI gate)
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf \
  --no-default-features --features "stm32f746,profile-cherry-smartterminal-st2xxx" -- -D warnings

# fmt check (CI gate)
cargo fmt --check
```

## USB PHY Reset Pattern (issue #22)

### Problem

After flashing with `st-flash`, the chip performs a **soft reset** (SYSRESETREQ)
rather than a full power-on reset. The USB OTG FS peripheral retains stale PHY
state across this soft reset, which prevents USB re-enumeration on the next boot.
Symptom: the reader does not appear on the USB bus after `st-flash write ...`
until the board is physically power-cycled.

This affects both STM32F469 and STM32F746 builds.

### Fix (proven pattern, sourced from the microfips project)

> **amp-embedded-common dissolved (2026-08-31):** the T13 extraction into
> `amp-embedded-common` was reversed per the existential necessity audit
> (`.omo/evidence/amp-necessity-audit.md`) — the repo had exactly one legal
> consumer (this one) and its crates were parallel discovery rather than
> library-sized work. `dwt_watchdog`, `diagnostics`, and
> `InitRecoveryTracker` are restored in-repo behind their historical paths;
> these inline USB-PHY reset sequences stay deliberately (see the
> `Kept inline deliberately` comments in `main.rs`). The only surviving
> component, the reusable CI workflow, lives at org level in
> `Amperstrand/.github`. The repo itself is archived (read-only, never deleted).

The fix is implemented inline in `firmware/ccid-firmware/src/main.rs`, in the
block titled **"USB OTG FS PHY reset (fix for issue #22)"** (search for that
comment). The sequence runs early in `#[entry] fn main()`, before
`USB::new(...)` constructs the OTG bus. It is `cfg`-gated per MCU feature.

The sequence, in order:

1. **Disable USB OTG FS clock** — `RCC.AHB2ENR.OTGFSEN = 0`, wait ~100 cycles.
2. **Re-enable the clock** — `RCC.AHB2ENR.OTGFSEN = 1`.
3. **Assert peripheral reset** — `RCC.AHB2RSTR.OTGFSRST = 1`, wait ~100 cycles.
4. **Deassert peripheral reset** — `RCC.AHB2RSTR.OTGFSRST = 0`, wait ~100 cycles.
5. **Wait for AHB idle** — poll `GRSTCTL.AHBIDL` (bit 31) at
   `USB_OTG_FS_GLOBAL` base `0x5000_0000` + offset `0x010`, with a 100 000-iteration
   timeout.
6. **Core soft reset** — write `GRSTCTL.CSRST = 1` (bit 0, self-clearing), poll
   until it clears (100 000-iteration timeout).
7. **PHY power cycle** — write `GCCFG = 0` (offset `0x038`), wait ~100 cycles,
   then write `GCCFG.PWRDWN = 1` (bit 16).

The register addresses are raw (`0x5000_0000usize as *mut u32` + offset) because
this runs before the HAL's `USB` abstraction is constructed. All access is
`unsafe { ... read_volatile / write_volatile ... }`.

```rust
// Sketch — see main.rs for the authoritative implementation.
#[cfg(feature = "stm32f469")]
{
    unsafe {
        let rcc = &*stm32f4xx_hal::pac::RCC::ptr();
        rcc.ahb2enr().modify(|_, w| w.otgfsen().clear_bit());
        cortex_m::asm::delay(100);
        rcc.ahb2enr().modify(|_, w| w.otgfsen().set_bit());
        rcc.ahb2rstr().modify(|_, w| w.otgfsrst().set_bit());
        cortex_m::asm::delay(100);
        rcc.ahb2rstr().modify(|_, w| w.otgfsrst().clear_bit());
        cortex_m::asm::delay(100);

        let otg_global = 0x5000_0000usize as *mut u32;
        // wait AHB idle
        let mut timeout = 100_000u32;
        while otg_global.add(0x010 / 4).read_volatile() & (1 << 31) == 0 {
            timeout -= 1;
            if timeout == 0 { break; }
        }
        // core soft reset (self-clearing)
        otg_global.add(0x010 / 4).write_volatile(1);
        timeout = 100_000u32;
        while otg_global.add(0x010 / 4).read_volatile() & 1 != 0 {
            timeout -= 1;
            if timeout == 0 { break; }
        }
        // PHY power cycle (GCCFG.PWRDWN, bit 16)
        otg_global.add(0x038 / 4).write_volatile(0);
        cortex_m::asm::delay(100);
        otg_global.add(0x038 / 4).write_volatile(1 << 16);
    }
    defmt::info!("USB PHY pre-init reset");
}
```

The F746 build has an analogous block with the same sequence using
`stm32f7xx_hal::pac::RCC`. Both blocks must stay in sync.

### When this matters

- **Always**, on any boot path. The reset is cheap and idempotent.
- **Especially** after `st-flash write` (soft-reset flash path).
- Not needed after `probe-rs run` (which uses a different reset strategy), but
  running it anyway is harmless.

## ESP32 Stack Size (issue #21)

The ESP32 main task stack is sized via `sdkconfig.defaults` in
`firmware/esp32-ccid/`. The correct Kconfig symbol is:

```
CONFIG_MAIN_TASK_STACK_SIZE=16384
```

> **⚠️ Do NOT use `CONFIG_ESP_MAIN_TASK_STACK_SIZE`.**
> That symbol does not exist in current ESP-IDF and will silently be ignored,
> leaving the stack at the default (typically 3–4 KB), which is too small for
> the MFRC522 + CCID handler path and causes a stack overflow / panic on boot.
>
> The correct symbol is `CONFIG_MAIN_TASK_STACK_SIZE`. Issue #21 fixed this.

If you see an ESP32 boot panic with a stack-overflow backtrace pointing into
the CCID handler or MFRC522 driver, verify `CONFIG_MAIN_TASK_STACK_SIZE` is set
and large enough (≥ 12 KB; 16 KB is the recommended value).

## Known-Good Bench State (2026-10-09, re-verified 2026-10-12)

**STATE 2026-10-20 (night) — the current tested surface.** The matrix
below is historical context; this block is the live truth until the
next stamp:

- **#330 root-caused and fixed (both causes).** (1) T=1 endpoint
  counters surviving host reconnects — fixed by resetting the endpoint
  at SetParameters(T=1)-accept (PR #345, regression test, bench: 3×
  consecutive connections against one long-lived pcscd all green where
  unfixed firmware failed from connection 2). (2) Per-APDU pcscd
  connections lose the applet selection through spec-correct
  PowerDown-on-last-disconnect (MFRC522 honors it; the nucula keeping
  state was the anomaly) — fixed in the lifecycle tool by holding ONE
  connection across the probe phase (PR #341). Full-green m5stick run:
  SELECT/GET DATA 6E (220 B)/VERIFY PW1/GET DATA 5E all 9000 through
  pcscd, uninstall clean, baseline restored.
- **Device scorecard**: nucula — full J3R180 lifecycle green (serial,
  via broker). m5stick — full lifecycle green on PR-#345-code firmware
  (flashed; re-stamp to merged main after #345 lands). STM32F469 —
  proven lifecycle, currently UNPLUGGED (cable borrowed). ACR1252 —
  oracle. Card placement: good card on m5stick coil (4-pkg baseline),
  original on nucula, baseline card on ACR.
- **Session lessons (both bit hard tonight):** (a) worktrees need
  their OWN `CARGO_TARGET_DIR` — the shared one has a CMake cache
  keyed to the main checkout's esp-idf path and fails with "source
  does not match the source used to generate cache"; symlink
  `.embuild`, separate target dir, cold build. (b) Decoding pcscd
  strace output requires `strace -xx -s 4096` (hex escapes) — the
  default octal/letter escapes silently mis-decode `\f`/`\n`/`\t`
  control bytes inside binary payloads and produced two false
  "corruption" findings before the discipline landed.
- **Known host-side (not reader) issues**: scdaemon wedges on the
  pcscd reader set (vpcd/modemifd IFDs) — pinned-reader + direct-APDU
  workaround, hygiene task t_fe356d5c; transient empty `gp -l` after
  heavy traffic (#332 item).


The per-board verification matrix — run these probes after acquiring a
place or whenever bench reality is in doubt. **A place you don't hold is
a board you don't own**: the bench is shared with micronuts/bolty-rs,
and the m5stick was re-flashed by a micronuts session within ~30 min of
a release the same day.

| Board | Identity probe (run this) | State 2026-10-11 ~22:3x |
|---|---|---|
| **IDF 5.5.5 unification (2026-10-12, branch `sisyphus/idf-555-unify` @91653fc)** — all three ESP-IDF repos (ccid/bolty/micronuts) bumped v5.5.1/v5.2.4→v5.5.5, crosstool `esp-14.2.0_20260121` on bench+CI, embuild 0.33.5, vendored-hal sd.rs backport (upstream 0.47.0 fix, field added in 5.5.5). Bench-verified: m5stick conformance battery (fuzz 87/0 wedges, pcscd restart 10/10, soak 1000/1000) + card-level differential FULL PASS (ATR/AID/APDU + ACR1252 agreement); nucula D1 coredump round-trip + fuzz 87/0 + soak 998/1000 + broker serving; stm32 HIL 7/7. **Hardware scorecard @67bcfbc (one-Rust-version toolchain, 2026-10-12 close):** nucula C3 — fork v1.98.1.0 image flashed, wire-silence PASS, D1 coredump PASS, broker serving through /opt/bench copy (service ExecStart moved out of the checkout after a sibling tree-loss crash-looped it 1500+ times); STM32 — HIL 7/7 (stable 1.99.0, no IDF); m5stick — fork image flashed, Escape 0x02 identity PASS, conformance battery in flight (log: /tmp/opencode/idf555/battery-final.log). **One open 5.5.5 item**: `log_shim` post-claim ring drain delivers nothing on the wire (bench-reproduced: serve loop + framing + D1 all fine, log lines never drain — suspect the 5.5.2 USJ-console tx changes) — **RESOLVED-BY-RETIREMENT the same day (#131)**: the shim was deleted, the ceiling moved before the driver claim, and `test_wire_silence_after_banner` now locks the clean wire. esp-idf-svc stays 0.52.1 (0.53.0 = hal 0.47/sys 0.38 generation, conflicts with the fork's legacy-I2C hal — follow-up lane) [superseded twice since this 2026-10-12 row: svc moved to the 0.53/hal-0.47 generation 2026-10-15/16 (D-2), and the esp-idf-hal fork itself was retired 2026-10-17 (PR #242 — hal rides registry v0.47.0, D-9; the "vendored-hal sd.rs backport" above lived in that fork and is history too)]. | | |
| nucula | pcscd: `Nucula CCID 00 00/01 00` via the PTY broker (`systemctl status nucula-ccid-broker`, journal = wire tap); identity probe: connect → ATR `3B 85 80 01 80 73 C8 21 10 0E` → SELECT ISD `9000` | OURS @3c74d74-era flashed 2026-10-11 ~23:0x (PT@0x8000 + app@0x40000): **pcscd WORKS through `host-tools/nucula-ccid-broker.py`** (PTY relay defeats the rst:0x15 wall — direct CDC opens still chip-reset, safe opener only). Broker = systemd service, symlink `/dev/nucula-ccid` gated on serving. Bench-verified 2026-10-11: 8/8 connect+ATR+SELECT sessions, >5.5 min stable enumeration, PTY GSS RTT 0.72 ms p50 (direct floor 0.20 ms). **Known gaps**: (1) after a host IccPowerOff the re-arm probe is rejected NCI `DISCOVERY_ALREADY_STARTED` (0xA0) and the HALTed card never re-activates (#125 hardware gap) — the broker's `POWER_OFF_SHIM=1` answers power-offs locally until fixed; (2) GP CAP installs fail at the first T=1 INSTALL (card answers `9100`, gp retries into comm error — m5stick-class T=1/CAP limitation). reader.conf `nucula-broker` DISABLED when idle (`mv` to re-enable + `flock /run/lock/bench-pcscd.lock systemctl restart pcscd`). Recovery: full reflash ladder (bootloader@0x0 + PT@0x8000 + app@0x40000), safe-opener probes only |
| m5stick | CCID Escape 0x02 over the by-id port → payload `GemPC Twin ESP32 1.0`; Escape 0xD0 → sane counters; Escape 0xD2 → turnaround split + exchange decomposition (issue #120, 32-byte LE: last/max turnaround µs, last/max card-exchange µs, blocks/wtx/recoveries/max_transceive_us; probe: `tests/hardware/nfc/turnaround_probe.py`); Escape 0xD3 [D3,N] → I2C poll-cadence probe (total µs for N ComIrqReg reads + configured FWT); T=1 retry-idempotence proof (issue #101: duplicate final I-block → cached replay with apdu_tx flat, chained 33B APDU, 5× retry storm; probe: `tests/hardware/nfc/t1_retry_probe.py`, needs pcscd stopped) | OURS @fix/101-era flashed 2026-10-12 ~12:43 (bl@0x1000+PT@0x8000+app@0x40000): **#101 FIX HARDWARE-PROVEN 2026-10-12** — serve-loop RX hardening (UART ring 548→4096: the old ring was 548-128=420 usable B < one 60 ms burst at 115200 8N2 ≈ 628 B; post-flush drain feeds queued retries before blocking) + retry-idempotent T=1 endpoint (duplicate final I-block replays the cached answer — never re-executes the APDU (double-INSTALL guard), never R-blocks into the libccid resend spiral). Bench evidence: pcscd 12/12 SELECTs @36.8 ms + 100 b2b exchanges @42.4 ms ZERO errors + 5 connect/disconnect cycles clean; raw T=1 probe (t1_retry_probe.py) — 3+5 duplicate replays byte-identical with apdu_tx flat, chained 33 B APDU (the 2026-10-11 killer class) delivered, fresh ns still executes, zero nak/err deltas. **CAP install still PENDING a CARD MOVE**: the keyed card 50942720 (working AUTH triple) sits on the ACR1252 coil; the m5stick coil holds CPLC 50906420 (unknown pre-personalized keys, not in the #127 ledger, default-ISD SELECT=6A82) — do NOT run gp against it (speculative auth). Move 50942720 onto the m5stick coil, then `gp --install /tmp/opencode/gp/SmartPGP-jc304-rsa4096.cap` with the soak_02 key triple (#59 payoff). Prior state: #120 closed card-bound (18–25 ms card floor, ~3 ms reader floor); #101 root cause = RX-loss-during-blocking-exchange (tap evidence 2026-10-11), NOT latency — at ~30 ms/APDU a 470-block upload ≈ 15–20 s card time |
| STM32 F469 | pcscd: `Cherry ... (ST2XXX-001)` + ComSign ATR `3B D5 18 FF ... 0A` | OURS, card working (eID ATR + SELECT re-verified 2026-10-11 22:2x), **42.75ms p50 n=60 post-DRY-phase-2 — non-regression proven on hardware**; labgrid HIL 7/7. **2026-10-12: T=1 relay hardening landed** (#132 fix — BWT-derived first-byte wait, honest-error contract, pre-read drain, ORE hygiene; root cause `docs/t1_relay_132_rootcause.md`; suite 14+1 green + fidelity hammer 0 anomalies vs 54 pre-fix; definitive card-50942720 gp proof pending its move into our slot). **Bench lesson**: INIT_UPDATE hammering locked the SmartPGP card's ISD management state (69FF on SC-class commands, survives power cycle; applet unaffected — #127) — NEVER hammer SC-class commands (INIT_UPDATE/EXT_AUTH/GET DATA 00E0); fidelity hammers use SELECT+CPLC only. |
| STM32 F746 | NOT ON USB (no second 0483 device ever appeared 2026-10-11 session; **bring-up needs the physical USB plug**) | PREP COMPLETE 2026-10-11: bench-distinct iSerial `ST2XXX-F746` landed (commit 0af0fb0, cfg-gated on `stm32f746`+Cherry — F469 keeps `ST2XXX-001`; the reference files constrain no iSerial, CherrySmartTerminalST2XXX.txt has no serial field); release binary cached at `/tmp/opencode/ccid-firmware-f746.bin` (both clippy passes + 562 host tests green). NEXT: plug USB → flash via probe-rs (chip STM32F746xx) or st-flash → verify pcscd shows BOTH Cherrys distinctly (iSerial ST2XXX-001 + ST2XXX-F746) → add labgrid place `stm32-f746` to `exporter-ai-legion-nfc.yaml` (match `ID_SERIAL_SHORT=ST2XXX-F746`) + environment.yaml + exporter restart per doctrine → T=1 card check per CHANGELOG [0.1.1] (bitbang TPDU pipe, 74.4 ms RTT class) |
| ACR1252 ref | pcscd ATR | P71 card activates fine (reference oracle) |

**Why the m5stick has no FWID probe**: the UART main applies the
clean-wire log ceiling BEFORE its FWID log line — identity on the CCID
wire must be ON-DEMAND (the Escape 0x02 string is today's identity;
folding the git rev into it is the tracked improvement). Never put an
unsolicited FWID on the UART0 wire — libccidtwin is strict.

**2026-10-12 session verification (post-migration)**: bench_inventory
all-green; all five project readers serve pcscd SIMULTANEOUSLY
(GemPCTwin serial, Nucula CCID via broker, CardMan 3121, Cherry
ST2XXX-001, ACR1252 PICC+SAM — NR7101 canary intact after every
restart); labgrid suite `pytest tests/hardware/labgrid/ -v --hil
--firmware-bin=<fresh build>` = 14 passed + 1 xfailed (STM32 st-flash
reflash + re-enumeration included); differential 12 passed / 4
card-gated skips. Bench card/GP inventory + key state: issue #127 —
AUTH triple verified on the ACR1252 card (full install→use→uninstall
lifecycle proven with TeapotApplet, SmartPGP fixture preserved); three
cards carry unknown pre-personalized ISD keys (retry budgets: OmniKey
3, Cherry 2, m5stick 2 of ~5-15 — no blind retries). Nucula
direct-serial gaps encoded as the exact-set xfail `NUCULA_KNOWN_GAPS`
(#128). `nucula-broker` reader.conf left ENABLED (serving state) —
disable when idle per the nucula row above.

### Bench state delta (2026-10-14 session — read before any STM32 HIL run)

- **The F469's ST-LINK (066FFF515786534867184152) is DETACHED from the
  bench host (.208)** (SUPERSEDED 2026-10-17: REATTACHED — see the newer
  delta below; kept for history); its USB CCID (046a) is absent. The `stm32-ccid`
  place still exports resources but is a phantom — the classic
  `test_ccid_hil.py` suite cannot run until the board is re-attached.
- **The F746 is wired (ST-LINK 066EFF3837334D4E43123327, place
  `stm32-f746`)** — ST-LINK only; its user USB remains unplugged (no
  046a enumeration possible yet). Its flash was restored to the
  current-main image 2026-10-14 (written+verified) after an incident
  where a bare local `st-flash` wrote an F469 image onto it (the
  conftest now pins the adapter serial — see the flashed_firmware
  fixture). ST-LINK SWD latch recovery on .208: USBDEVFS_RESET
  (ioctl 0x5514) on the ST-LINK device node.
- **CardMan 3121 is on the bus as `076b:3021`** (OmniKey VID — grep
  for 0b97 finds nothing; bench_inventory's USB-presence check keys on
  the right IDs only on the bench host).
- New places: `ccid-nucula` / `ccid-m5stick` (OTA seats, boards at
  192.168.13.230/.234:3232, flash via `tests/hardware/vpcd/ota_push.py`),
  and the wifi-vpcd path serves pcscd from the bench nucula. The
  sibling root session on .208 actively holds `nucula-c3` + `m5stick`
  — coordinate, don't steal.
- **Nucula HIL pretest is blocked by its own backup-first guard**
  (probed 2026-10-14 with the place free): the board is ALIVE —
  esptool flash-id answers (MAC 90:da:72:9a:50:18, 4 MB) and the
  partition table reads — but `pretest_check`'s unknown-app-region
  backup dies with `Packet content transfer stopped` (the documented
  C3 CDC bulk-read stall), so the suite refuses to run until a
  backup succeeds. That is the guard working as designed; treat the
  app-region read as the bench-limited step, not the board.
- Coordinator: `LG_COORDINATOR=192.168.13.208:20408` (the .221 host is
  NOT the bench host; USB readers and the classic HIL suites live on
  .208 — run them there).

### Bench state delta (2026-10-15 session — nucula-fork test-framework night)

- Both ESP32 boards ended the day BACK on ccid wifi-vpcd serving VPCD
  (nucula rev=40d391e, m5stick rev=683c879; FWID-verified; both
  `Nucula VPCD` readers in pcscd). **m5stick restore lesson**: the bench
  vpcd image runs from **ota_0@0x40000** — writing it at 0x30000 under
  the OTA table boot-loops with `0x40000 has invalid magic byte
  (nothing flashed here?)`. Restore recipe (PT already on board):
  `esptool --chip esp32 -b 115200 -p <hades-by-id> write-flash 0x40000
  ~/.cargo-target/esp32-ccid-m5stick-vpcd.bin` **then erase the otadata
  selector** (`esptool --chip esp32 -p <hades-by-id> erase-region
  0x30000 0x2000` — otadata sits at 0x30000 in partitions-ota.csv) so a
  stale ota_1 selection cannot keep booting the old image; erased
  otadata makes the bootloader take the first valid slot (ota_0). The
  FWID banner on the console is the boot proof either way.
- **Mint service live on .208:3338** (micronuts-audit-adapter fronts
  mint_server, FakeWallet auto-settling; durable start =
  Amperstrand/nucula `rig/scripts/start-mint.sh` in the fork worktree;
  ufw open to the bench LAN) — deliberately left running as cross-project
  test infra (the fork rig's money-loop e2e + soak ride it).
- **Shared IDF v5.5.5 tree** at `/root/esp/esp-idf-v5.5.5` on .208 — the
  Amperstrand/nucula fork builds ride the bench-unified version
  deliberately (its upstream pin is 5.5.1; deviation recorded in the
  fork's session branch). Retire candidates (owner-executed):
  `/root/esp/esp-idf` (v5.3.1) and the stale global `.embuild`
  esp-idf v5.2.3.
- **`nucula-rig` labgrid place** now exists on the .208 coordinator (the
  fork rig's cross-project lock; matches `m5stick-usb` + the ACR —
  acquiring it excludes ccid's `m5stick`/`ref-acr1252` properly).
- Fork session branch `sisyphus/test-framework` → **PR #13** (bench
  migration, classic-esp32 console SoC fix, plain-http mint fix — that
  one line blocked every mint op — NUCULAPCB board lane, e2e/soak
  invisible-success guards). Micronuts parity: **PR #105** (49-row
  cited matrix) + gap issues #98–#104. Full session report:
  `micronuts/reports/2026-10-15-parity-audit-session.md` (49 money-loop
  cycles green on hardware; crash-consistency boundary observed at
  ~714 sat).
- **PN7160 on the nucula: chip ALIVE** — answers ccid's window-rule
  probe at boot (`pn7160_i2c: probe @0x28 right after VEN cycle:
  Ok(())`); the fork's `nci_init` (+50 ms probe) fails against the same
  silicon — evidence + init-port plan on fork issue #12.
- **esp toolchain gotcha (#199)**: `RUSTUP_TOOLCHAIN=esp` does not
  resolve on .208 (version-named toolchains only; `-Z build-std`
  rejected) — actdiag/C3 Rust builds blocked there until
  `espup install -c 14.2.0_20260121 -v 1.98.1.0` per the pin above.
- hil-bench nightly was disabled for the session day and re-enabled
  (verified active) at close.

### Bench state delta (2026-10-17 session — #193 display hardware verification)

- **The F469 ST-LINK (066FFF515786534867184152) is REATTACHED** — the
  2026-10-14 "DETACHED" delta below is stale. Both `stm32f469-stlink`
  (SerialPort) and `stm32f469-ccid` (USBDevice 046a:003e/ST2XXX-001)
  resources live on the exporter; place is `stm32-ccid` as before.
- **The board RUNS the display image** (main @ 6526f93 + `display`
  feature, sha256 2766424…, flashed 2026-10-17 ~00:5x via
  `stm32_flash.flash_f469`) — bench-verified strictly additive: same
  USB identity, byte-exact ATR vs baseline, CPLC 9000, Escape 0xD0
  sane. Full RTT boot evidence (SDRAM/DSI/NT35510-default/FT6X06 lines)
  in issue #193's hardware comment; evidence file at
  `~/src/worktrees/ccid-firmware-rs/193-display-hil/.omo/evidence/`
  on .208 (worktree retained until #193's owner-eyes LCD check closes).
  Known anomaly candidates there: NT35510 DSI read probe fails →
  driver defaults (owner eyes decide if the panel renders), and
  Escape 0xD0 `apdu_tx=0` despite card exchanges.
- **The 03:29 nightly run failed on `ref-acr1252: absent`** — the
  ACR1252 was physically off the bench at 03:29, replugged by morning
  (exporter `avail: True` again). Everything STM32 passed; the board
  was NOT reflashed by the nightly. Per #183 doctrine an absent
  reference reader should be an amber skip, not suite red — candidate
  follow-up in the drift layer (#234/#237).
- Overnight 2026-10-16→17 waves (PRs ~#211–#237) fixed the pn532 CI
  break (68b9361, issue #211 closed) and landed the STATUS drift layer;
  main was green through ci-ok at 4fc1982 by afternoon.

### Bench state delta (2026-10-26 session — nightlies green)

- **ref-acr1252 place RETIRED from .208** (2026-10-25; the reader now
  lives on ai-legion-small as `ref-acr1252-small`, exported by that
  host's `labgrid-exporter-nfc-small.service`, consumed via conwrt-bench
  — retirement recorded in the coordinator place note, #71). ccid-side
  retirement landed 2026-10-26: PLACES/IDENTITY_PROBES/
  bench_inventory EXPECTED_PLACES+EXPECTED_PCSCD and the in-repo
  `exporter-ai-legion-nfc.yaml` all dropped it (the in-repo exporter
  config MUST NOT re-add it — re-exporting would resurrect a phantom
  place with no hardware). The ACR1252-emu lane (`--acr-emu`) is parked
  until an ACR returns to this host. The nfc/ differential suite's ACR
  oracle legs are equally dead on .208 — treat them as card-gated.
- **m5stick runs the cashu-nfc-matrix T5 wallet** (foreign firmware,
  reflash owner-gated): silent console, no vpcd reader, no GemPC
  answer. `dut_absent_reason("m5stick")` now applies the Codex-#188
  nucula pattern — port present + no pcscd reader + no GemPC identity
  answer = unknown/foreign variant → amber skip (a wedged standard
  image ambers there too; interactively re-verify when that matters).
  The 10-25 nightly was green only because the places were held; the
  10-26 nightly exposed all of this red.
- **Fuzz Nightly was NEVER green** (all 9 runs since creation red):
  the repo's `rust-toolchain.toml` (stable 1.99.0) outranks the
  dtolnay@nightly install for plain `cargo fuzz run` — stable rejected
  `-Zsanitizer`. Fixed with `cargo +nightly fuzz run` (the +toolchain
  proxy exports RUSTUP_TOOLCHAIN to cargo-fuzz's children, which beats
  the TOML). VERIFIED GREEN post-merge by dispatching both nightlies on
  main (2026-10-26 ~13:1xZ): Fuzz all three targets success (the
  workflow's first green run ever), hil-bench 115 tests / 0 failures /
  14 amber skips. Lesson: on ANY pinned-toolchain repo,
  `dtolnay/rust-toolchain` does not override `rust-toolchain.toml` for
  later steps — a step needing a different toolchain must either use
  `+<toolchain>` explicitly (what we did) or set a job-level
  `RUSTUP_TOOLCHAIN` env (micronuts' #110 escape hatch — CORRECTION
  2026-10-26 of this same delta: an earlier revision claimed micronuts
  carries the same latent bug; that was a too-narrow read of their
  workflow — micronuts sets the env override at job level and its fuzz
  nightly was green 5/5 throughout).

### Recovery events (2026-10-09, all resolved)

- **m5stick re-flashed by micronuts** (display showed their app,
  console silent): re-flashed our bench binary
  (`~/.cargo-target/esp32-ccid-m5stick.bin`), identity re-verified.
- **nucula latched in ROM download mode** (`boot:0x5, waiting for
  download` — stale esptool state, reader vanished from pcscd):
  recovered via the documented ladder (`esptool flash-id --after
  hard-reset` + reset-and-capture-in-one-serial-session; a read window
  opened AFTER the reset loses the boot banner to CDC re-enumeration).
- **nucula download-latch via DTR-asserted port holder** (2026-10-09
  evening, twice): a python holder on ttyACM* opened with pyserial's
  DEFAULT DTR-asserted state makes every subsequent reset (RTS pulse,
  openocd `reset run`) boot into `boot:0x5` download mode — the C3's
  USB-Serial/JTAG peripheral latches host line state across soft
  resets (same class as the STM32 PHY issue #22). Symptom: their
  conformance battery read 0/1000 on the soak while the board sat in
  the ROM prompt. Rule: `setDTR(False); setRTS(False)` immediately
  after EVERY open on the nucula CDC, and recover with the flash-id
  ladder only when no other process holds the port.

### Open gaps to full known-good (card-level)

1. **m5stick card activation — FIXED (2026-10-09)**: marginal coupling
   made single-shot REQA→RATS ~50% flaky; `power_on` now retries the
   activation (4 attempts, frontend reset + 15 ms settle between) —
   bench-verified 20/20 power-on cycles with the card constantly on the
   coil. ISO 14443-3 §6.2.4 / 14443-4 §5.2 citations inline.
2. **nucula presence (#88) — RESOLVED (2026-10-10, bench-verified)**:
   the missing piece was never TTL tuning — the PN7160 AUTO-ACTIVATES
   the ISO-DEP bench card (RF_INTF_ACTIVATED, no RF_DISCOVER ever) and
   every revision discarded that NTF. Fixed end-to-end (presence +
   IccPowerOn ATR + APDU verified with the card on the coil); model and
   chip-wedge constraints under "Session Lessons". The physical-removal
   leg (RF_DEACTIVATE reason link-lost) is implemented, mock-tested,
   awaiting one hand-verified card removal.
 3. **The libccidtwin T=1 wall — RESOLVED (46cb50c, bench-verified
    2026-10-10, #101)**: pcscd used to enumerate the GemPCTwin reader,
    power the card (ATR flows), then die at connect ("Card is
    unresponsive") because libccidtwin sends SetParameters(T=1) +
    T=1 S-blocks as XfrBlock payloads which the APDU-relay firmware
    forwarded to the card as garbage (SW=6E00). The fix — an ISO
    7816-3 §11 T=1 block endpoint inside the reader (`t1.rs`: S-blocks
    answered locally, I-block INF reassembled to APDUs through the
    shared pipeline, responses re-framed) — is on main and
    **bench-verified**: pcscd connect + SELECT/AID APDUs through the
    serial reader return real card status words. Known caveat (#101):
    after SetParameters arms the T=1 route, RAW-APDU XfrBlocks from
    direct-serial consumers (battery-style) degrade — fuzz wedges up
    to 3/27 with one >5 s recovery; pcscd traffic is unaffected.
4. **GPG applet end-to-end — INSTALLED AND OPERATIONAL (2026-10-09)**:
   - **Keys found in-repo**: the AUTH triple from
     `tests/soak/soak_02_globalplatform.py` (`--key-enc 5A9E...9EDF
     --key-mac 7CCC...2998 --key-dek B040...AA80`) authenticates the
     bench P71's ISD (the default 40..4F is rejected — one retry
     consumed; do not blind-guess further).
   - **SmartPGP v1.23.0 (JC 3.0.4, RSA-4096 build) installed** through
     the ACR1252 via `gp -install`; applet AID `D276000124010304AFAF...`
     SELECTABLE; `gp.jar` + CAPs cached at /tmp/opencode/gp/ (re-fetch:
     github.com/github-af/SmartPGP releases, tag v1.23.0-javacard-3.0.4).
   - **Three RSA-2048 keys generated ON-card** via raw APDUs
     (GENERATE ASYMMETRIC KEY PAIR `00 47 80 00 02 B8/B6/A4 00` — note
     the **case-4 Le byte is REQUIRED** through the ACR: without the
     trailing `00` the card answers 6700). SmartPGP does NOT auto-write
     fingerprint DOs after on-card generation — compute SHA-1 over the
     pubkey packet (`99 05 14 || len(n) || n || len(e) || e`) and PUT
     DATA C7/C8/C9 (the combined C5 write is 6A88 — unsupported).
   - **gpg --card-status reads the card fully** (keys, fingerprints,
     counters). Bench gpg quirk: scdaemon's internal CCID driver fights
     pcscd for the ACR — run gpg with pcscd briefly stopped
     (`systemctl stop pcscd pcscd.socket; gpg --card-status; systemctl
     start pcscd`), or expect "ccid open error" / "No such device".
     If the ACR PICC interface vanishes from pcscd entirely, a USB
     device reset (USBDEVFS_RESET ioctl on /dev/bus/usb/001/<dev>)
     recovers it.
   - **OPEN QUIRK — PSO signature format**: PSO COMPUTE DIGITAL
     SIGNATURE (`00 2A 9E 9A`) returns 9000 + 256 B after PW1 verify,
     but the result does not verify as PKCS#1 v1.5 (SHA-256 DigestInfo
     in, DigestInfo-out mismatch) nor PSS at common salt lengths against
     the key read back via `00 47 81`. SHA-1 input gives 6985 (rejected
     — good). Raw APDU artifacts kept at /tmp/opencode/sigpub_do.bin +
     sig.bin. Suspects: SmartPGP padding variant or a key-slot readback
     subtlety. First test through gpg itself (scd drives PSO with its
     own framing) before assuming a card bug.
   - The bench NFC card is **P71D321 silicon** (CPLC `ICType=D321`,
     fabricator 4790=NXP) — "J3R180" is NXP's JCOP4 R-series PRODUCT
     name on that silicon.

### Codex review feedback — ALWAYS check before/during/after PR work

Every PR on this repo gets an automated Codex-bot review (user
`chatgpt-codex-connector[bot]`) with P1/P2/P3-badged findings. These
are HIGH-SIGNAL: the 2026-10-11 sweep found **53 items across 13
merged PRs — 38 had never been addressed**, including real protocol
bugs (CORE_SET_CONFIG status byte, PCB bit layout, ICC status decode)
that host tests missed because the mocks modeled the wrong behavior.

**The rules (rule 0 is a HARD MERGE GATE — owner directive 2026-10-20;
AMENDED 2026-10-21 — see the self-review fallback in 0b):**

0. **NO PR MERGES BEFORE ITS CODEX REVIEW IS READ AND TRIAGED.**
   The bot reviews the PR head within ~5–30 min of each push. A green
   CI is NOT merge permission. Before merging:
   - The Codex review for the **head commit** must exist
     (`gh api repos/Amperstrand/ccid-firmware-rs/pulls/N/reviews`);
     if it has not landed yet, WAIT for it. Never merge in the gap.
   - Every finding is either FIXED on the PR or explicitly triaged in
     a PR reply (wontfix rationale or a tracking-issue pointer). The
     reply IS the audit trail — same-session, BEFORE merge.
   - Origin: PRs #333 and earlier merged on green CI while reviews
     were pending/unaddressed; the findings had to be swept into #332
     after the fact. Post-merge cleanup is the failure mode this gate
     exists to prevent. (First enforcement: #341/#345, both held open
     until their reviews landed and were fixed+replied in 99cf263a /
     03140058.)
   b. **Self-review fallback (owner amendment, 2026-10-21)**: when the
     bot has produced NO review on the head after reasonable latency
     (retrigger once with an `--allow-empty` commit, then ~45+ min
     quiet), a substantive SELF-review satisfies the gate: read the
     full diff (not the description), run the branch's test gates
     locally, verify claimed-against-actual — then record one PR-reply
     line "self-reviewed per 0b: <what was checked, which tests ran>"
     and merge. UNADDRESSED BOT FINDINGS STILL BLOCK: if any bot
     finding exists on any commit of the PR, fix or triage it first
     (0b never overrides 0's triage requirement).
1. **Before opening a PR**: read the Codex feedback on the last 3-5
   merged PRs touching the same area — patterns repeat.
2. **After a PR merges**: re-check its Codex comments once more within
   the same session (the bot can review the final merged state) and
   reply to anything new the same way.
3. **When picking up work in an area**: grep the tracking issues
   (#112/#113/#114 and successors) for backlog items in that area
   before writing new code — don't duplicate or contradict.
4. **The triage file** `docs/CODEX_PR_FEEDBACK_TRIAGE.md` is the
   canonical sweep output; re-run the sweep when it goes stale.

**How to sweep** (from the 2026-10-11 session):
```bash
# 1. Dump all Codex comments from merged PRs
for pr in $(gh pr list --state merged --json number --jq '.[].number'); do
  gh api repos/Amperstrand/ccid-firmware-rs/pulls/$pr/comments \
    --jq '.[] | select(.user.login | test("codex")) | .body' > /tmp/pr-sweep/$pr.txt
done

# 2. Read each file, verify against the tree (NOT keyword-grep — read
#    the flagged code), classify ADDRESSED/UNADDRESSED with evidence

# 3. Reply on each PR + create tracking issues for remaining items
```

**Why this matters**: without the check-reply cycle, Codex feedback
accumulates silently. The P1 findings compound: the ICC-status decode
bug made the conformance battery's GetSlotStatus cases meaningless
(absent cards passed, active cards failed) — exactly the kind of
test-infrastructure rot that wastes bench sessions.

### Review-quality protocol (how to review + be reviewed)

**Reviewing others' work:**
1. **Read the diff, not the description** — verify every claim the
   PR/commit makes against the actual code. A commit message saying
   "fixes X" is a lead, not evidence.
2. **Run the tests yourself** — `cargo test --workspace --target
   x86_64-unknown-linux-gnu` from the repo root, plus the esp32 crate.
   A green CI badge is the floor, not the ceiling.
3. **Check the spec** — if the change touches protocol behavior (CCID,
   NCI, ISO 7816, ISO 14443), cite the section. The `// CCID_SPEC:`
   markers + specquotes.toml enforce this for comments; apply the same
   standard to code.
4. **Look for the second-order effects**: does this change break the
   conformance battery? The perf baseline? The clean-wire rule? Check
   `docs/PERF_AND_DRY_STRATEGY.md` for cross-cutting invariants.
5. **Review the tests too** — a new feature without a regression test
   is half-done; a test that asserts the bug (not the fix) is worse
   than no test.
6. **Size matters**: >300-line diffs get a second pass. If you can't
   hold the whole change in your head, split the review.

**Being reviewed (Codex or human):**
- Every finding gets a REPLY — done (commit hash) or skipped (reason).
  The reply IS the audit trail; silence = the finding compounds.

**Issue truth (2026-10-11 lesson, #110):** issues lag reality — 3 of 5
items in #110 were already fixed when picked up, wasting triage time
and risking duplicate fixes. Rules: (1) before working an issue,
re-verify each item against the tree (`git log -- <file>`) — an issue
body is a lead, not evidence; (2) when a PR/commit fixes an issue item,
cross-reference it on the issue in the SAME session (check off the
checklist line or comment the commit hash) so the next session starts
from truth; (3) write issue bodies as checklists (`- [ ]`) so partial
completion is visible at a glance.
- P1 findings get fixed before merge when possible; if deferred, a
  tracking issue MUST be created in the same session.
- Mock-only fixes don't count until verified against the hardware —
  the "mock/hardware divergence" class is this repo's #1 bug source
  (see the Session Lessons section).

### Testing through labgrid (how hardware verification works)

**The five places own the hardware.** Any test that touches a physical
device (flashing, pcscd enumeration, ATR capture, APDU relay, GPG
flows) must acquire the device's labgrid place first:

```bash
labgrid-client -p stm32-ccid acquire    # or nucula-c3, m5stick, ref-acr1252
# ... your hardware work ...
labgrid-client -p stm32-ccid release
```

**What each place provides:**

| Place | Device | Test path |
|---|---|---|
| `stm32-ccid` | F469-DISCO contact DUT | `pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil` (7 tests: USB enum, pcscd, ATR, APDU relay ×2, pinpad, escape 0xD0) |
| `nucula-c3` | ESP32-C3 + PN7160 DUT | `pytest tests/hardware/nucula/test_nucula.py -v --hil` (responsive, wallet boots, I2C scan, fw ACKs, NCI init, sustained) |
| `m5stick` | M5Stack MFRC522 DUT | via the differential/battery suites |
| `ref-acr1252` | ACR1252 reference NFC | differential oracle (`tests/hardware/nfc/`) — **NVM config contract (bolty-rs B33, PR #215)**: auto-polling/PICC-param registers are EEPROM that survives replugs and host moves; a sibling session's "quiet auto-poll" write (`E0 00 00 23 01 00`, 2026-10-22) made this reader look dead for 2 days (#214). For RF-quiet experiments use a transparent session (`FF C2 … 81`/`82`) — never the polling register. If the oracle "sees no cards" while firmware answers (`FF 00 48`): read `FF 00 50 00 00` — `90 3F` means polling OFF, fix is `FF 00 51 FF 00` + `E0 00 00 23 01 8B` (escape via `SCardControl(CTL_CODE(1))`), no replug. bolty-rs's HIL check-in audits + heals this automatically. |
| `ref-cardman` | CardMan 3121 reference contact | perf baseline |

**The test pyramid (bottom to top):**
1. **Host tests** (no hardware): `cargo test --workspace --target
   x86_64-unknown-linux-gnu` — run from repo root, every commit.
2. **Conformance battery** (no card): `python3
   tests/hardware/nfc/conformance_battery.py` — framing fuzz, GetSlotStatus
   soak, pcscd restart resilience. Run per firmware flash.
3. **Card-level differential** (card on coil): `pytest
   tests/hardware/nfc/test_differential.py -v --hil` — same card through
   every reader, ATR + AID + read-only APDU agreement.
4. **GPG HIL** (OpenPGP card on coil): `pytest
   tests/hardware/nfc/test_gpg.py -v --hil` — enumerate → AID SELECT →
   `gpg --card-status` per reader.
5. **Performance benchmark**: `python3
   tests/hardware/perf/benchmark_readers.py -n 300` — percentile latency
   per reader; run before/after any perf-touching change.

**Bench coordination rules:**
- **One hardware holder at a time** — acquire the place or don't touch
  the device. Concurrent sessions race ports and corrupt each other's
  results (paid for repeatedly, see Session Lessons).
- **pcscd serialization** — any pcscd stop/start must go through
  `flock /run/lock/bench-pcscd.lock`; the conformance battery stops
  pcscd, so no other test can hold it during a battery run.
- **Cards move** — verify coupling (`present` flag via Escape 0xD0)
  before any card-level conclusion; a cardless reader on a known-good
  build after a hard reset is a PHYSICAL issue, not firmware.
- **Clean-wire is inviolable** — no unsolicited log output on the CCID
  wire; diagnostics go through Escape 0xD0/0xD1 or a `ble` build.
- **Verify what booted** — every flash must be proven via FWID marker
  or boot behavior, never trusted from the flash log alone.

### Working-tree coordination

Sessions share this checkout — check `git status` before building.
The #90 serving-path consolidation HAS landed (both classic mains are
one-line `serve_uart` calls over `ServeConfig`); the two flags raised
while it was in flight were resolved in commit `9fd1626`: the bin-side
`crate::` import break (bin now uses `esp32_ccid::` paths — CI had been
red for ~2 h before anyone noticed) and the clean-wire `Off` ceiling
restored in the mfrc522 main. Verify `cargo build` for the BIN target,
not just host tests, after main.rs surgery.

## Crash Dumps & Snapshot Debugging (dump-and-retrieve)

**The preferred debug workflow for time-sensitive paths** (NFC card I/O,
PN7160 ACK window, CCID wire timing): don't log or debug live — run the
firmware undisturbed, then capture state post-mortem. No debug channel
means no timing perturbation and no wire contention.

### Workflow

1. **Crashes**: the panic handler writes a flash coredump automatically
   (`CONFIG_ESP_COREDUMP_ENABLE_TO_FLASH=y`, coredump partition
   `0x3F0000..0x400000` in `partitions-ota.csv`; verified end-to-end,
   issue #64).
2. **Suspicious state, no crash**: induce one — send **CCID Escape 0xD1**
   over the serial CCID wire. The firmware acks (`RDR_to_PC_Escape`
   payload `0xD1`), then the serve loop panics with
   `escape 0xD1: diagnostic snapshot requested` AFTER the ack is on the
   wire, so the panic handler writes a coredump of the live state.
   **Verified on the nucula (2026-10-09, IDF v5.5.1)**: trigger → ack on
   wire → silent dump → reboot → `retrieve` decodes a symbolized
   backtrace (`abort_internal` → `panic_abort`) plus every task's state,
   and the reader serves CCID again after the reboot.
3. **Retrieve + decode**:
   ```bash
   python3 tests/hardware/esp32_coredump.py trigger /dev/ttyACM0   # induce
   sleep 6                                                          # dump write + reboot
   python3 tests/hardware/esp32_coredump.py retrieve \
       /root/.cargo-target/riscv32imc-esp-espidf/debug/esp32-ccid   # read + decode
   python3 tests/hardware/esp32_coredump.py erase   /dev/ttyACM0    # when done
   ```
   The script auto-discovers the esp-idf copy + python env under the
   cargo target dir (`ESP_COREDUMP_IDF_PY`/`ESP_COREDUMP_PYTHON`
   override); decode uses `gdb-multiarch`. Manual procedure (esptool
   read-flash + espcoredump.py info_corefile) is documented below in
   "Coredump decode".

### The silent-panic rule (bench-proven 2026-10-09)

All full sdkconfigs set `CONFIG_ESP_SYSTEM_PANIC_SILENT_REBOOT=y`. The
default PRINT_REBOOT mode **hangs on the nucula**: the panic handler's
print stage blocks on the C3's USB-Serial/JTAG console once
`UsbSerialDriver` has claimed the peripheral — the escape-0xD1 snapshot
reached `panic_abort`'s unimp trap (JTAG-verified), then no backtrace,
no coredump, no reboot. Silent mode skips printing and goes straight to
the coredump write + reboot, which is the dump-and-retrieve philosophy
anyway: read the dump offline, not on the wire. (On UART-console boards
panic prints would additionally corrupt the CCID wire.) Do not "fix"
this back to PRINT_REBOOT without re-verifying the full round-trip.

The same applies to the HIL defaults flow: `sdkconfig.wallet-test`
(the `build_firmware()` fragment) also sets SILENT_REBOOT + coredump,
overriding `sdkconfig.defaults.esp32c3`'s PRINT_REBOOT (it layers
last). **Verified on both chips** (2026-10-09): nucula via the HIL test
`test_escape_d1_snapshot_roundtrip` (including the size-optimized
defaults-flow build), M5Stick via the manual round-trip — the xtensa
configs previously had coredump set to NONE (issue #64 only configured
the C3), now `CONFIG_ESP_COREDUMP_ENABLE_TO_FLASH=y` everywhere.

### The ack-before-panic flush (UART mains)

On the UART serve loops the 0xD1 response write only QUEUES into the
UART driver's TX ring; panicking immediately resets the peripheral
before the ack clocks out (symptom: echo on the wire, dump written, but
no `RDR_to_PC_Escape` frame). The mains call `uart.flush_write()`
between the response write and the snapshot panic. The USB-CDC path
drains fast enough not to need it.

### Decoder quirks (esp-coredump on this bench)

- IDF 5.5.1's `esp_coredump` thread printer crashes on newer
  gdb-multiarch `LWP N` thread ids **after** emitting the panic info —
  the decode still yields the panic reason, crashed task, registers,
  and memory map (the HIL test tolerates the nonzero exit). Full
  symbolized backtraces are reliable on the debug/C3 path; xtensa
  release builds may show `<unavailable>` frames (optimization +
  windowed registers).
- The xtensa dump's `exccause` reads `IllegalInstructionCause` with the
  EPC inside the trap path — that is IDF's deliberate `asm("ill")` in
  `panic_abort`, not a real fault.

### espup crosstool pin (CI, 2026-10-09)

CI pins `espup install -c 14.2.0_20260121` (the crosstool build IDF
v5.5.5's `tool_version_check` accepts — the check matches the FULL
crosstool-NG build string from `gcc --version`, so IDF v5.5.2+
REJECTS the 20241119 build of the very same GCC 14.2.0). espup
defaults to the LATEST crosstool
and its bin dir lands ahead of embuild's own copy in PATH — the day's
`esp-16.2.0_20260914` release failed every xtensa matrix entry at cmake
configure. The SAME day exposed a second failure mode: the
espressif-cache restore-key chain fell back across toolchain
generations and restored v5.2-era `esp-13.2.0` embuild tools against
IDF v5.5.1 — the red run then SAVED that poison under a fresh exact
key. The cache key/restore-keys now carry a generation prefix (`v7`,
the IDF v5.5.5 generation)
that only falls back within the current generation; **bump it whenever
the toolchain/IDF world moves** (ci.yml carries the same note).
**Bench**: espup re-pinned to `esp-14.2.0_20260121` on 2026-10-12
(the v5.5.5 unification) — bench and CI now match. Historical note:
before that the bench ran `esp-15.2.0_20250920`, which only passed
because the cmake configure was cache-stamped (a fresh configure
would have failed the tool_version_check). Also: pushes touching `.github/workflows/` must go over SSH
(`git push git@github.com:…`) — the OAuth tokens here lack the
`workflow` scope.

### The clean-wire rule (UART mains + pcscd, 2026-10-09)

libccidtwin's parser is STRICT: even WARN-level failure lines
("power_on failed: …") interleaving with GemPC frames make pcscd reject
the handshake or error-loop in EHStatusHandlerThread. The mfrc522 UART
main therefore sets `log::set_max_level(Off)` in non-`ble` builds —
UART0 carries CCID frames only; diagnostics go through Escape 0xD0
counters, 0xD1 coredumps, or a `ble` build. Bench host side of the same
rule: the FTDI `latency_timer` must be 1
(`/sys/bus/usb-serial/devices/ttyUSB0/latency_timer`, issue #51) — a
USB re-enumeration (ftdi_sio unbind/rebind, replug) RESETS it to 4 and
pcscd error-loops until it is set back. Re-enumeration also renumbers
ttyUSBx, but nothing on the bench hardcodes tty numbers anymore (2026-10-09):
`/etc/reader.conf.d/gempc-bench` uses the renumber-proof by-id
`DEVICENAME`, bench scripts resolve ports via
`tests/hardware/serial_ports.py` (by-id globs, `NUCULA_PORT`/`M5STICK_PORT`
env overrides), and `flash_and_test.sh` autodetects. Only the latency_timer
still needs manual re-set after a rebind. The nucula CDC path cannot use
by-id in reader.conf (its by-id contains colons, which DEVICENAME splits
on) — its config keeps the resolved `/dev/ttyACM0`-style path.

### Limits (and the complement)

A coredump is a frozen instant: it answers "what state was the firmware
in", not "what sequence led there". For sequence-dependent failures
(stalls, flapping presence, starvation), the complements are the
flight-recorder-style channels that already exist:

- **Escape 0xD0 diagnostics** (28-byte counters over the same CCID wire,
  zero extra wiring): apdu tx/rx, NAKs, reinits, presence, uptime.
- **`pn7160-actdiag`** diagnostic main (no USB-CDC claim, logs every NCI
  step) for card-path dives where the console is usable.
- **BLE debug console** (experimental, xtensa only — next section) for
  wireless live logs when a serial connection is impractical.
- **GDB-over-JTAG** for live single-stepping (procedure below).

### STM32 note

The 0xD1 escape + coredump workflow is ESP32-only (STM32 has no flash
coredump partition); the STM32 debug channel stays defmt + RTT.

## BLE Debug Console (issue #66) — EXPERIMENTAL, xtensa bench only

Firmware logs can ride **BLE GATT notifications** instead of the serial
console, keeping the CCID wire (USB-CDC on nucula, UART0 on M5 boards)
free of debug traffic. The GATT service imitates the **Nordic UART
Service (NUS)** — service `6E400001-B5A3-F393-E0A9-E50E24DCCA9E`, one
Notify characteristic on `6E400003-…` — so stock centrals work without
custom tooling (nRF Connect, "Serial Bluetooth Terminal", bleak).

### Module map (firmware/esp32-ccid/src/)

| Module | Role |
|---|---|
| `ble_log_queue.rs` | Pure ring buffer + line formatting (drop-oldest, dropped counter, attach banner). Always compiled, 7 host tests. |
| `ble_logger.rs` | `log::Log` sink: `log()` only enqueues (never blocks); `drain()` pumps the ring into GATT at safe loop points. |
| `ble_debug.rs` | Bluedroid GATT server (esp-idf-svc `EspBleGap`/`EspGatts`), CCCD subscribe tracking, MTU-3 chunking, per-connection notify latch. |
| `ble_console.rs` | `BleConsole::init(modem, silence_c_logs)` facade every main calls: installs the logger, optionally silences C-level ESP_LOGx, brings up BtDriver + GATT + advertising. `drain()` from the serve loop. |

### Build + capture

> **Status (2026-10-09): parked by owner decision** — dump-and-retrieve
> (previous section) is the preferred debugging style; live BLE logging
> stays available where it already works (M5Stick bench builds). The C3
> 0x0c wall is CLOSED (D-3, docs/DECISION_REGISTER.md) — both boards
> advertise; what remains for `c3-ccid-ble` is the connect/NUS-delivery
> verification (tracked with issue #66). The code is feature-gated,
> host-tested,
> and costs nothing in non-`ble` builds.

```bash
# build (BT-enabled sdkconfig variants; stamp guard auto-invalidates cmake)
cd firmware/esp32-ccid
./build.sh m5stick-ble --flash <port>   # M5Stick: logs over BLE, UART0 = CCID only

# capture on the bench (hci0)
sudo rfkill unblock bluetooth && sudo hciconfig hci0 up
pip install bleak
python3 tests/hardware/ble/ble_log_capture.py            # scan + follow
python3 tests/hardware/ble/ble_log_capture.py --list     # scan only
python3 tests/hardware/ble/ble_log_capture.py --send 'x' # write to NUS RX
```

Logs queue from logger-install time (32 lines × 200 B, drop-oldest); a
central that attaches later receives an `=== BLE log attached; N older
lines dropped ===` banner, then the retained history, then live lines.

### The pairing rule (compile-loud)

The cargo `ble` feature REQUIRES a BT-enabled sdkconfig
(`sdkconfig-c3-ble.full` / `sdkconfig-xtensa-ble.full` — the build.sh
`*-ble` board entries pair them automatically). A `ble` feature build
against a BT-disabled sdkconfig fails to COMPILE (esp-idf-svc's bt
module vanishes) — deliberately loud. The reverse (BT sdkconfig, no
`ble` feature) merely wastes flash.

build.sh's sdkconfig stamp is keyed per (triple, profile), NOT per
board: boards sharing a target share one esp-idf-sys cmake cache, so a
sdkconfig switch between boards (e.g. `c3-ccid` ↔ `c3-ccid-ble`)
invalidates the cache and rebuilds (~10 min). This is the fix for the
"cmake cache doesn't pick up sdkconfig BT changes" blocker from the
original issue.

### The logger-ordering rule (latent-bug class)

The Rust `log` crate accepts exactly ONE global logger — first
installer wins, permanently. `EspLogger::initialize_default()` claims
it; a later `BleLogger::install()` silently fails (its Err is usually
swallowed). Under the `ble` feature a main must NEVER call
`EspLogger::initialize_default()` — `BleConsole::init` installs the
BLE logger itself. The original M5Stick build had both calls and its
BLE queue stayed empty forever while logs kept hitting the console.

### C3/Bluedroid sdkconfig traps (bench-proven)

- `CONFIG_BT_BLE_42_FEATURES_SUPPORTED=y` is REQUIRED: esp-idf-svc
  uses the legacy 4.2 advertising API; on C3 it defaults OFF and the
  two `esp_ble_gap_*` adv symbols vanish from libbt.a (link error).
- `CONFIG_BT_BLE_50_FEATURES_SUPPORTED` must be OFF on C3: with
  BLE-5.0 extended advertising enabled the controller rejects LE Set
  Advertising Parameters with Command Disallowed (0x0c) and the device
  never advertises (`bta_dm_ble_set_adv_params_all` / `hci write adv
  params error 0xc` in the console with C logs unsilenced).
- **OPEN (updated 2026-10-14): the 0x0c rejection is REAL controller
  behavior — config/cache hypotheses eliminated.** Even with 42-on/50-off,
  a correct adv/scan-rsp split, the canonical IDF event chaining
  (adv-data complete → scan-rsp write → scan-rsp complete →
  start_advertising), AND a `c3-ccid-ble` build.sh board that forces the
  sdkconfig through the cache-stamped flow (branch `sisyphus/c3-ble-board`),
  the C3 still rejects the adv params write with 0x0c — bench-reproduced
  2026-10-14 with a hash-verified flash: boot log prints `ble: adv + scan
  rsp configured — starting advertising` then `ble: event-chain error:
  ESP_FAIL` from the AdvertisingStarted event. The GATT chain completes —
  only advertising is blocked. Stale-sdkconfig as an explanation is dead
  (the `ble` feature is deliberately loud against BT-off configs and the
  build compiled+ran with BT enabled). Ranked next steps on issue #66:
  (1) surface the REAL BtaStatus instead of collapsing to ESP_FAIL +
  unsilence BT_BTM logs in a diagnostic build; (2) diff against the
  working xtensa path (identical Rust chain advertises fine on m5stick);
  (3) if Bluedroid-on-C3 stays opaque, evaluate the NimBLE host for the
  C3 console. `sdkconfig-c3-ble.full` stays 42-on/50-off. The nucula
  debug story stays: standard build (console shares CDC; the GemPC parser
  tolerates log noise), coredumps for crashes (issue #64),
  `pn7160-actdiag` for card-path dives, GDB-over-JTAG for live state.

### Bench central (BlueZ) gotchas

- **The classic-page trap (updated 2026-10-16)**: for a peer address
  bluetoothd has typed BR/EDR, `Device1.Connect()` PAGES over classic —
  `Create Connection (0x0005)` + `Page Timeout` in btmon while the
  BLE-only peripheral keeps advertising fine. The old mitigation
  (pass the discovery object, not a bare address) became INSUFFICIENT
  with the **2026-10-15 bluez upgrade (5.72-0ubuntu5.7→5.7.8)** and
  bleak 3.x: verified on the bench, bluetoothd deterministically types
  the m5stick's SPP-era address classic even after bluetoothd restart,
  mgmt power cycle, rfkill, btusb module reload, and full
  /var/lib/bluetooth per-peer purge — while an LE-only-history peer
  (smoke-alarm control) connects via LE Extended Create Connection on
  the same adapter (#163). The capture tool now drops the stale peer
  object and connects against a fresh live-scan discovery; if that
  regresses, the remedy is host-side: pin bluez 5.72-0ubuntu5.7, or use
  a central adapter with no classic memory of the address (USB BLE
  dongle as hci1).
- If connects keep timing out: `systemctl restart bluetooth`, ensure
  `hciconfig hci0` shows UP, and prefer a fresh scan in the same
  process as the connect.
- `rfkill` soft-blocks hci0 on this bench after reboots — unblock
  before scanning.

### FWID + labgrid/test integration stance

In `ble` builds the FWID banner goes to the BLE ring, NOT the console
(console carries CCID only; C-level logs are silenced at runtime —
ROM/bootloader output still appears early, which the CCID serve loop
purges). **HIL/labgrid verification therefore stays on standard
(non-ble) builds** where FWID lands on the console as usual — BLE
builds are for interactive bench debugging. Tests read behavior
(pcscd, ATRs, FWID markers on standard builds), never BLE log lines.
Coredump-to-flash (issue #64) already covers crash forensics — BLE
logging is complementary live observability, not a substitute.

**GPG-on-readers HIL (2026-10-10)**: `tests/hardware/nfc/test_gpg.py`
runs the OpenPGP flow per reader — enumerate → AID SELECT (9000 =
applet present) → full `gpg --card-status` via scdaemon pinned to the
reader index (`disable-ccid` + `reader-port <n>`; indices shift on
re-enumeration — always resolve fresh). Current gating: acr1252 full
PASS; m5stick card-gated (T=1 stack proven 5/5 pcscd sessions +
real SWs — the SmartPGP install through our relay stalls on the
~470-block CAP upload, per-block MFRC522 I2C latency; the perf pass
is the unlock); nucula card-gated on the coil question + a reader
stability item (see below); stm32 card-gated (move a card into the
contact slot). The m5stick multi-session fix that unblocked this:
`t1_armed` disarms on card reset (pcscd's PPS-before-SetParameters
probe) — commit 60b3fa3, regression test included.

**Nucula reader-retirement issue (open)**: with the #88/#105
auto-activation + unconditional re-arm firmware, the nucula enumerates
but pcscd retires it minutes later. TWO mechanisms are now separated
(2026-10-10 bench, PR #107 comment 6098890356):

1. **Serve-loop stalls (real, FIXED by 5f702e4)**: ~2 s GetSlotStatus
   stalls at the ~5.7 s re-arm cadence (1414-probe histogram) — the
   blocking NCI exchange inside the presence poll. The non-blocking
   re-arm state machine (5f702e4/335a404) removes them; host tests
   47/47 + 128/128.
2. **The chip killer (open)**: on the 5f702e4 bench run the reader
   still died **T+95 s** into pcscd ownership — `rst:0x15
   (USB_UART_CHIP_RESET)` → `boot:0x5` download latch while pcscd's
   libccidtwin held the port (DTR asserted). No panic, no coredump,
   PC idle in the ROM download loop (JTAG forensics). The classic
   serial-reader reset idiom (host RTS toggle) is a plain app reboot
   on the m5stick's FTDI but a download-mode LATCH on the C3's
   USB-Serial/JTAG when DTR is asserted — the same class as the
   2026-10-09 "download-latch via DTR-asserted port holder" events,
   now triggered from *inside* a pcscd session (likely its
   power-up-failure error recovery). Serve-loop fixes alone cannot
   keep the reader in pcscd while this exists; candidate mitigations:
   make IccPowerOn's absent-state answer keep pcscd out of its retry
   escalation, or disable the USB-Serial/JTAG reset-from-USB path.

Bench hygiene lessons from the same run: JTAG-connected resets
(openocd `reset run`) boot the C3 into download mode too — the ROM
sees the JTAG session; shut openocd down BEFORE resetting for a
normal boot. And the safe CDC opener is `Serial(); port=…; dtr=False;
rts=False; open()` — pyserial asserts DTR at plain `Serial(port)`
open time, which is itself a latch hazard on any later reset. The
latch recurred 2026-10-10 ~17:35 under `conformance_battery.py`
(its `Reader` opens the CDC with pyserial defaults): the nucula
answered "no response" on every case until RTS-pulse-recovered —
pre-open dtr/rts deassert in the battery's `Reader` is the tracked
hardening item alongside PR #107.

### pcscd over Bluetooth (m5stick, 2026-10-11 — bench-verified)

The `m5stick-spp` board (build.sh entry, sdkconfig-xtensa-spp.full) serves
the SAME GemPC CCID protocol over BT-Classic SPP instead of UART0. Host:
`rfcomm bind 0 4C:75:25:CB:92:9E 1` + reader.conf `DEVICENAME
/dev/rfcomm0:GemPCTwin` — pcscd enumerates `GemPC Twin BT (m5stick)`,
card-level verified (ATR + APDU, 30/30 soak, p50 48ms/APDU). Recipe in
firmware/esp32-ccid/README.md; bench conf archived at
/root/reader-conf-backup/m5stick-spp.bench. NOTE: this board flashes the
ota_0 slot @0x40000 (build.sh), NOT the legacy factory @0x30000.

### Debug-channel matrix (per board)

| Board | CCID wire | Debug channel |
|---|---|---|
| STM32F469/F746 | USB CCID | defmt + RTT via probe-rs (separate from USB) |
| M5Stick/M5Atom (standard) | UART0 | console NONE by default; netlog (UDP :4567) with WiFi creds |
| M5Stick/M5Atom (`ble` build, experimental) | UART0 | BLE NUS notifications (`m5stick-ble`) |
| nucula standard | USB-CDC | clean-wire (#131): boot banner only, then framed CCID only — diagnostics via Escape 0xD0/0xD1, coredumps, or actdiag/ble/netlog builds |
| nucula (`ble` build) | USB-CDC | `c3-ccid-ble` board exists (`sisyphus/c3-ble-board`); advertising WORKS — the C3 0x0c wall is closed per D-3 (docs/DECISION_REGISTER.md); remaining: connect/NUS-delivery verification (issue #66) |

## Device CI: the hil-bench nightly + generated STATUS.md (2026-10-14, #183)

**Hardware status is GENERATED, never hand-written.** The `hil-bench`
workflow (`.github/workflows/hil-bench.yml`) runs nightly 03:17 UTC on
the self-hosted runner `ai-legion-bench` (systemd `gh-runner-bench`
on .208, label `bench`): bench inventory (`bench_inventory.py --json`)
→ labgrid HIL suites (per-test 240 s timeouts, GP lifecycle excluded
unattended — card-budget doctrine) → `scripts/gen_status.py` publishes
`docs/STATUS.md` to the **`bench/status` branch**. README links there;
no hand-maintained works/doesn't-work prose anywhere.

Rules and lessons (all paid for 2026-10-14):

1. **The runner executes as root on a PUBLIC repo** — `hil-bench` must
   never gain pull_request/pull_request_target triggers (fork code =
   RCE on the bench). schedule + dispatch only. Runbook + unit template:
   `tools/hil/README.md` §9.
2. **SKIP ≠ FAIL**: card moved / board detached / place busy = amber
   skip with reason; only code-caused failures are red. The current
   reds are REAL state (F469 detached; m5stick runs the wifi-vpcd
   variant — identity FAIL is variant drift, don't reflash).
3. **Killed pytest leaves labgrid places acquired** — kill pytest
   gracefully or release the places after (`labgrid-client who` shows
   them); a stale hold masks hardware absence (stm32-ccid "skipped"
   for hours because a killed run still held it).
4. **pytest-timeout method=signal, not thread** — a C-level `select()`
   wedge (pyserial) survives the thread method's dump and the process
   exits, losing junit; SIGALRM interrupts it and the run continues.
5. **earlyoom poisons `pgrep -f` guards** — its `--prefer` list
   literally contains "cargo|rustc"; use `pgrep -x` for build-liveness
   checks before target-dir surgery.
6. Registration-token transfer as a FILE (scp) — `> file 2>&1`
   redirects and ssh-stdin reads corrupt tokens into 401 mysteries
   (runbook §9).

## Labgrid Bench Doctrine (ai-legion NFC/CCID testbed, 2026-10-09)

The bench (ai-legion, multi-homed as 192.168.13.208) carries five labgrid
places, all exported by the `ai-legion-nfc` exporter from the in-repo
config `tests/hardware/labgrid/exporter-ai-legion-nfc.yaml` (deployed to
`/etc/labgrid/exporter-nfc.yaml`, restarted via nohup after edits):

| Place | Device | Identity |
|---|---|---|
| `stm32-ccid` | F469-DISCO CCID DUT | ST-LINK serial + Cherry-emul USB serial `ST2XXX-001` |
| `nucula-c3` | nucula PN7160 DUT | Espressif USB-JTAG by-id path |
| `m5stick` | M5Stack MFRC522 DUT | Hades2001 by-id path |
| `ref-acr1252` | ACS ACR1252 reference NFC reader | USB path 1-4 (serial-less) |
| `ref-cardman` | OmniKey CardMan 3121 reference contact reader | USB path 1-11 (serial-less) |

**The rules:**
1. **labgrid coordinates hardware** — every HIL session acquires the DUT's
   place before touching hardware (`labgrid-client -p <place> acquire`,
   release on teardown; the pytest conftest does this automatically).
   Concurrent sessions cannot race a device.
2. **pcscd owns logical reader access** — tests select readers by stable
   identity (USB serial embedded in the pcscd name), NEVER by enumeration
   order. Zero or multiple matches is a loud error with a listing.
3. **DUTs and reference readers are disjoint by identity** — our F469
   Cherry-emulation carries iSerial `ST2XXX-001`; the authentic ACR1252 /
   CardMan / NR7101 can never match it. If a real Cherry is ever added to
   the bench, its serial will differ.
4. **Tests run ON the bench host** — helpers are local subprocesses. SSH is
   only for genuinely remote benches (labgrid SSHDriver + NetworkService).

**Daily driver commands:**
```bash
python3 tests/hardware/labgrid/bench_inventory.py   # one-shot bench health
pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil          # F469 CCID
labgrid-client -p stm32-ccid acquire                # manual reservation
```

**Cross-project**: bolty-rs and micronuts use the SAME bench and places —
acquire the relevant place before any bench work there too.

**Exporter config trap**: the config is a Jinja2 template with
`line_statement_prefix="#"` — every `#`-line is a JINJA STATEMENT, not a
comment. Comments must be `##`. Symptom of getting it wrong:
`TemplateSyntaxError: Encountered unknown tag ...` on line 1. Also: this
labgrid build only supports STRING udev matches
(`match: 'ID_VENDOR_ID=046a ...'`), not dicts — dicts die with
`cannot translate OrderedDict to MapValue`.

**pcscd reader.conf trap**: `DEVICENAME` splits on the FIRST colon —
`/dev/serial/by-id/` paths contain colons (MAC bytes). Use the resolved
`/dev/ttyACM0`-style path (see /etc/reader.conf.d/nucula-ccid).

## ai-legion Host Hygiene (issue #108)

Two standing rules from the 2026-10-10 disk-full incident. The disk
filled to 78G+ on `/root/.cargo-target` while several worktrees held
unpushed fix branches that existed nowhere else.

### 1. Shared CARGO_TARGET_DIR: prune on a cadence

`/root/.cargo/config.toml` pins `target-dir = /root/.cargo-target` for
**every cargo project on the host** (this repo's runbook pattern, and
the root of several build gotchas above). Nothing evicts from it
automatically — without a pruning cadence it only grows.

Measured 2026-10-11 (79G total): `debug/` 42G (19G deps + 17G
incremental), `x86_64-unknown-linux-gnu/` 20G (host test artifacts),
`release/` 4.2G, `.embuild/espressif` 11G (tools 6.7G + IDF copies +
1.1G dist tarballs).

**Safe-to-prune (owner-executed; nothing here is unique):**

| Path | Size | Why |
|---|---|---|
| `debug/incremental/` | 17G | always regenerated; delete freely |
| `.embuild/espressif/dist/` | 1.1G | toolchain tarballs, re-downloadable |
| `.embuild/espressif/esp-idf/v5.2.3/` | 546M | stale — manifest pins v5.5.5 (v5.5.1 clones are also prunable post-migration) |
| `x86_64-unknown-linux-gnu/` + `release/` | ~24G | rebuildable; `cargo sweep --time 30` trims by mtime |
| other projects' binaries (`cln-*`, `cashud`, `decode-vls`, ...) | varies | the dir is HOST-WIDE — coordinate with those lanes before bulk deletion |

**Keep:** the top-level cached bench binaries (`esp32-ccid-m5stick.bin`
etc. — the m5stick recovery image), `.embuild/espressif/tools/` (the
actual toolchains; re-downloadable but a 10+ min stall mid-session),
`esp-idf/v5.5.5` (current pin), and the riscv/xtensa esp-idf-sys build
dirs while `/root/src/ccid-firmware-rs` (the checkout they cache-stamp
against) is live.

`cargo-sweep` 0.8.0 is installed (`/root/.cargo/bin/cargo-sweep`).
Cadence: run `cargo sweep --time 30` from the workspace root monthly or
at any disk-pressure signal; a full `rm -rf /root/.cargo-target/debug/
incremental` is safe any time no build is running. NOTE: `cargo sweep
--time` works per target dir, so run it once from this repo AND once
from the other active checkouts (their artifacts share the dir).

**The audit is code, not prose** (2026-10-11): run
`python3 scripts/host_hygiene.py` for the one-shot report —
disk pressure (85% warn / 90% action), prunable candidates vs the
manifest IDF pin, and the worktree-farm unpushed-work classification.
Read-only by design (pruning is owner-executed); `--json` for tooling,
exit 1 only when something needs action NOW (unpushed work, disk ≥90%).
`bench_inventory.py` additionally carries the disk-pressure check
(85%/90%) so the daily bench health run surfaces it.

Repo-side: `.gitignore` already covers `/target/` + `**/target/` and
`.cargo/config.toml` sets no target-dir — the pin lives host-level in
`/root/.cargo/config.toml` and is deliberately NOT tracked here.

### 2. Worktree farm: close lanes by pushing, then removing

ccid sessions spawn parallel worktrees (historically under
`/tmp/opencode` — 39 dirs at the 2026-10-11 survey; NEW lanes go to
`~/src/worktrees/<repo>/<branch>`, see section 3). Unpushed branches in
scratch worktrees are **fixes that exist only on this disk** — the
2026-10-10 audit found five (stm32-perf-fix, nucula-rearm-fix,
sisyphus/ccid-49-50, sisyphus/presence-ttl, sisyphus/pn7160-drvab)
sitting unpushed for days. The pattern recurred on 2026-10-11
(`triage-refresh` held one unpushed docs commit).

**Rule — at lane close, in order:**

```bash
git -C <worktree> push origin <branch>   # every branch that carries work
git worktree remove <worktree>           # then the worktree itself
git branch -d <merged-branch>            # labels already on origin/main
```

Before any disk cleanup that touches `/tmp/opencode`, audit for
unpushed work: for each worktree, `git log origin/main..<branch>` must
be empty (or the branch pushed) before that worktree is deletable.

### 3. Durable session artifacts — nothing load-bearing in /tmp (2026-10-14)

The 2026-10-14 PR-158/168/174 session paid for the /tmp habit three
ways on one dev box: a per-user tmpfs quota killed a background esp
build mid-compile (`Disk quota exceeded` — /tmp is a 7.8 GB tmpfs, far
too small for an esp-idf build tree regardless), quota-pressure writes
TRUNCATED worktree files silently (symptom: `git status` reporting mass
deletions in files nobody touched; recovery `git checkout -- .`), and
session-scoped background jobs died with their launching shell. Between
sessions the same day /tmp was then wiped outright: the Codex sweep
dumps and dispatch briefs for PRs 158–161 and two session worktrees
vanished (the branch labels survived on origin — the wipe cost the
artifacts and the briefs, not the git state).

**Rules:**

1. **Worktrees live on persistent disk**: `~/src/worktrees/<repo>/<branch>`
   (the main checkout's disk class, ~100 GB+ free) — never /tmp. That
   disk also fits an esp-idf target dir, making local target-compile
   verification possible where the tmpfs quota made it hopeless.
2. **Session artifacts** (Codex sweep dumps, dispatch briefs, battery
   logs, cached binaries an AGENTS row points at) live under
   `<checkout>/.omo/` — gitignored, persistent, per-checkout (existing
   convention: `.omo/pr-sweep/`, `.omo/logs/`, `.omo/evidence/`). New
   AGENTS rows citing an artifact path cite the `.omo` one. /tmp is
   acceptable only for genuinely disposable cache you would not miss
   after a reboot. Codex sweep dumps regenerate anytime:
   `scripts/codex_sweep.sh <pr> [<pr> ...]` (writes `.omo/pr-sweep/`).
3. **Long builds/jobs must outlive their shell**: `setsid nohup …
   </dev/null &` is the floor; better, launch from a persistent
   worktree so outputs are durable too.
4. **Recognize the truncation signature**: mass "deleted" hunks in
   `git diff` for files never edited = the filesystem ate writes.
   Recover with `git checkout -- <paths>` (or `.`), then re-verify the
   files you DID edit were not clipped (tail-check + full gates) before
   committing.

The worktree-farm rules in section 2 apply unchanged at the new path.

## Session Lessons: PN7160 Card Path + Reader Fuzzing (2026-10-09)

### Mock/hardware divergence — the #1 firmware-bug class this week
The pn7160 mock `transact` returned ANY queued reply; the real transport
filtered for MT_RSP only, stashing every NCI DATA packet (the APDU
responses!) as a "notification". 13/13 protocol tests passed while the
hardware starved. **Rule: mocks must model the transport's filtering
semantics, not just the happy reply queue.** When hardware fails where
the mock passes, diff the transport contract first.

### The wallet firmware's TLVs were RIGHT; our "fix" was wrong (corrected 2026-10-11)
> **This lesson previously said the wallet's TOTAL_DURATION TLV was
> malformed (missing LEN octet). That was OUR misreading, not the
> wallet's bug — kept here because the correction is the doctrine's
> founding incident (see "Reference-First Doctrine").**
The wallet's plen=5 payload `[num=1][id=0x00][len=2][val=FE 01]` is a
VALID NCI CORE_SET_CONFIG TLV: param IDs are ONE octet (kernel
`struct set_config_param { __u8 id; __u8 len; ... }`,
NXP `NCI_PARAM_ID_TOTAL_DURATION 0x00`). An earlier session read it
under a two-octet-id assumption, called it malformed, and "fixed" it
into a 6-byte TLV with a trailing garbage octet — the chip rejected
it (num_applied=0) for weeks while the pre-ed0ed5e parser masked the
rejection. The corrected bytes (wallet-verbatim) apply cleanly:
RSP `[status=OK, 0 failed params]`, bench-validated 2026-10-11.

### Edge-triggered discovery NTFs — SUPERSEDED by the auto-activation model (2026-10-10, #88)
The "one RF_DISCOVER_NTF per ARRIVAL" model was wrong for the bench card
class: the PN7160 AUTO-ACTIVATES a single ISO-DEP target during discovery
(NCI-sanctioned — kernel `nci_target_auto_activated()`) and emits
RF_INTF_ACTIVATED instead of RF_DISCOVER. Correct model, now in the driver:
- **Presence** = a live auto-activation (the tag sits IN the active RF
  interface). No re-arm cycling in this state — repeatedly deactivating an
  auto-activated interface WEDGES the chip (~10-11 cycles → I2C NACKs until
  power cycle; both IDLE+RF_DISCOVER-re-issue and type-DISCOVERY variants
  wedge identically when a card is in the field).
- **power_on** builds the ATR from the activation NTF's own ATS (NCI 2.0
  tail TLV) and MUST NOT send RF_DISCOVER_SELECT (select-on-activated is
  the other chip-wedger).
- **Removal** announces itself: RF_DEACTIVATE_NTF(reason=RF-link-lost) →
  absent immediately. Empty-field re-arm cycles are safe (overnight soak,
  9fd1626) and now use deactivate-type-DISCOVERY (0x03).
- Boot timing: the boot auto-activation lands ~800 ms after the ladder
  (~TOTAL_DURATION after discovery start) — anything reading earlier sees
  nothing; don't "fix" the wait.

### Main-loop logging dies after the USB-CDC driver claim — was "fixed" by `log_shim` (#91) — RETIRED 2026-10-12 (#131)
esp-idf logs flow until `UsbSerialDriver::new` takes the peripheral;
after that the CCID serving loop's log output silently drops. **Fix
(bench-verified)**: `log_shim::install()` after the claim routes logs
into a drained ring — `esp_log_set_vprintf` for C `ESP_LOGx` plus a
`log`-facade sink (esp-idf-svc's EspLogger writes to newlib stdout,
NOT the vprintf sink, so the facade must be routed too —
`log_shim::init` replaces `EspLogger::initialize_default`). Verified on
the nucula: FWID + full PN7160 ladder visible post-claim, LRC-valid
CCID responses interleaved with log text. Facade lines carry an
`esp_timer` ms stamp (`[WARN] (123ms) target: msg`); over-long lines
end `~\n` so truncation is visible. The HIL regression is
`test_log_shim_post_claim_output`. The no-claim diagnostic main
(`pn7160-actdiag`) remains the tool for card-path dives where the
console claim itself is the problem.

### Reader test timing
GemPC Twin error paths legitimately take up to ~0.8 s (presence
retries over I2C + status LED logging). Fixed-sleep probes misreport
slow responses as wedges: read drain-until-idle (two empty reads,
1.5 s cap). The m5stick echoes interleaved with log text — frame
parsers must scan for SYNC-anchored, LRC-validated frames, never
assume clean streams.

### Two CCID serving paths in esp32-ccid
The classic mains (main.rs) duplicate the serving loop the
USB-CDC main gets from `ccid_serial_server` (echo, poll gating,
timeout guards). Bug fixes now need THREE patch sites (server +
two main loops) — the stall guard went in exactly there. Refactor
opportunity: route the classic mains through CcidSerialServer.

## Session Lessons: Log-Shim Bench Verify + Multi-Session Bench Racing (2026-10-09, #91)

The #91 verification ran while TWO sibling opencode sessions
(`ccid-4950`, `ccid-specc`, both on issue #89) were cycling
conformance batteries against the same nucula. Traps, all paid for:

- **Raw port grabs contaminate sibling batteries.** An esptool/openocd
  attempt that opens ttyACM2 mid-battery resets the board under their
  probes — their soak reads 0/1000 and the verdict says FAIL for
  reasons that are not their firmware. Wait for battery PIDs to exit
  (`ps` on `conformance_battery.py`), and remember their `finally`
  block RESTARTS pcscd, which then holds the port — stop pcscd for the
  flash window, restart it after.
- **Shared target-dir flash images get silently overwritten.** Sibling
  sessions rebuild esp32-ccid into the shared `/root/.cargo-target`; a
  scratch `elf2image` output (even with your own filename in
  /tmp/opencode) can be someone else's binary hours later — the
  compile-time stamp and FWID rev in the boot log are the truth. Build
  flash images in a private `CARGO_TARGET_DIR`, or elf2image
  immediately before flashing and prove what booted via FWID.
- **CDC bulk READS stall; writes are fine.** `read-flash` and
  esptool's post-write verify die with "Packet content transfer
  stopped" (460800 always, 115200 intermittently, `--no-stub` gets
  ~97% then dies). `write-flash` at 115200 is reliable. Flash, then
  prove the image by boot behavior (FWID rev marker), not read-back.
- **Verification pattern that worked**: one python session, open the
  by-id port with `setDTR(False); setRTS(False)`, RTS pulse (0.2 s)
  for a fresh boot, aggressive reconnect through the re-enumeration
  (the RTS reset kills your own fd — catch, close, reopen), then
  framed GetSlotStatus/IccPowerOn while still capturing: assert
  LRC-valid responses AND log lines in the same byte stream. The FWID
  `rev=<git>` line proves WHICH build answered.

## ESP32-C3 Nucula Board — USB Port Lifecycle (CRITICAL)

The nucula's USB-Serial/JTAG is a **composite device** (CDC serial + JTAG on one USB port). Three failure modes that WILL happen if you're not careful:

### ⚠️ Failure mode 1: Port contention

When firmware with console output is running, the CDC endpoint is actively streaming. If any process (bench logger, test fixture, `tail -f`, another terminal) holds `/dev/ttyACM*`, **esptool cannot open the port** — it hangs or gets "device reports readiness to read but returned no data".

**Prevention**:
```bash
# ALWAYS kill port holders before flashing
fuser -k /dev/ttyACM* 2>/dev/null
pkill -9 -f nucula_logger.py
sleep 2  # let the OS actually release the port
```

### ⚠️ Failure mode 2: Partition table mismatch

Flashing only the app (at 0x40000) while the board has the wallet firmware's partition table (factory app at 0x30000) means the bootloader loads the WRONG binary or nothing at all. Symptoms: silent board, old firmware running, "M1" probes appearing from a binary you flashed 30 minutes ago.

**Prevention**: ALWAYS flash bootloader + partition table + app together:
```bash
esptool write-flash 0x0 <bootloader> 0x8000 <partition-table> 0x40000 <app>
```

### ⚠️ Failure mode 3: USB peripheral state corruption

After a full chip erase or certain flash sequences, the C3's USB peripheral stops responding to software reset (DTR/RTS toggle). **Only a hard power cycle (USB replug) recovers.** No software fix.

**Prevention**: Avoid `erase-flash` unless truly needed. If you must, expect to replug afterward.

### ⚠️ Failure mode 4: silently-dropped reset → stale firmware (CRITICAL for test integrity)

esptool's post-flash "hard reset" (RTS control request to the USB-JTAG) is
**intermittently dropped**: the flash log says "Hash of data verified. Hard
resetting via RTS pin..." but the chip never resets and the OLD firmware keeps
running. Bench proof (2026-10-07): health counter continued 52→124 across a
"successful" flash; the "new firmware" test silently exercised stale code.
Raw `setRTS()` toggles on an open port fail the same way.

**Prevention** — never trust the flash log alone; PROVE what booted:

1. **FWID markers**: every Rust firmware logs `FWID <name> rev=<git> build=<ts>`
   as its first console line (build.rs stamps `FW_GIT_REV`/`FW_BUILD_TS`).
2. **`board.flash_and_boot()`** (tests/hardware/nucula/board.py) runs the full
   protocol: flash → verify expected marker (FWID / wallet prompt) → if the
   reset was dropped: JTAG reset (`openocd init; reset run; shutdown`) →
   verify → if the chip latched in download mode (`boot:0x5`): esptool
   `flash-id --after hard-reset` round-trip → verify → raise with console
   evidence.
3. **Pre/post-test checklist**: `pretest_check()` (port exists, no port
   holders, esptool responsive) gates every HIL session; the session teardown
   runs `ensure_responsive()` and falls back to `restore_known_good()` (wallet
   flash) if the board is wedged.

Note: the FWID line can print during the USB re-enumeration window and be
lost — the ladder's retry handles that; periodic markers (`health[1..2]`,
low counters) are an additional fresh-boot signal.

### ⚠️ Failure mode 5: bootloader offsets differ by chip family + overwriting unknown firmware without a backup (m5stick incident, 2026-10-08)

The bench M5Stack (Hades2001 USB-serial, classic ESP32) boot-looped with
`flash read err, 1000 / ets_main.c 371` after a manual full-set flash.
ROOT CAUSE (found next morning, device fully recovered): the bootloader
was written to **0x0** — the ESP32-**C3** offset. Classic ESP32 boots its
second-stage bootloader from **0x1000**. Five "recovery attempts" failed
because they all repeated the same wrong offset while patching header
bytes (DIO/20MHz) — when N recovery attempts fail, re-derive the basics
instead of tuning guesses.

**Bootloader flash offsets (memorize or check before every manual flash):**

| Chip | Bootloader offset | Partition table | Typical app |
|---|---|---|---|
| ESP32 (classic, xtensa) | **0x1000** | 0x8000 | per table (default factory: 0x30000) |
| ESP32-C3 / S3 / C2 | **0x0** | 0x8000 | per table (our OTA table: 0x40000) |

The app must land where the partition table you ACTUALLY flashed points
(read it from the boot log — `boot: 2 factory factory app 00 00 00030000`
means factory@0x30000). build.sh --flash handles this; manual esptool
invocations are where the offset bugs creep in.

Recovery outcome: bootloader@0x1000 + default PT@0x8000 + app@0x30000 →
esp32-ccid MFRC522 firmware boots, pcscd enumerates it as
`GemPCTwin serial` on /dev/ttyUSB2, CCID GetSlotStatus answers correctly.

**Prevention**: before overwriting unknown firmware on ANY bench device:

```bash
esptool --chip esp32 -p PORT read-flash 0x0 0x400000 device-backup.bin
```

One minute of backup versus a soft-bricked board nobody can restore
unattended. This belongs in the HIL pretest checklist for shared rigs.

### ⚠️ Boot message loss during USB-CDC re-enumeration

After flashing, the USB device disconnects and reconnects. The first 1-2 seconds of boot output (including boot banners, VEN cycle logs, early probe results) are lost. This is NOT a firmware bug.

**Prevention**: Use `ConsoleCapture` (tests/hardware/nucula/console.py) which polls the by-id path at 50ms intervals. Add boot markers to distinguish firmware versions.

### Board state decision tree

```
Board silent after flash?
├── Check port exists: ls /dev/serial/by-id/usb-Espressif*
│   └── No port → USB replug needed (failure mode 3)
├── Check what's running: is old firmware's console output visible?
│   └── Old firmware → partition table mismatch (failure mode 2)
├── Try esptool flash-id
│   └── Hangs → port contention (failure mode 1)
└── Try RTS reset
    └── Still silent → USB replug needed
```

### Recovery: the nuclear option

```bash
# Kill everything
fuser -k /dev/ttyACM* 2>/dev/null
pkill -9 -f "esptool|nucula_logger|python.*serial"

# Wait for USB to settle
sleep 5

# Full flash from known-good wallet firmware
cd /tmp/opencode/nucula-fw/build-551
esptool --chip esp32c3 -p /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_90:DA:72:9A:50:18-if00 \
  --baud 460800 --after hard-reset write-flash \
  0x0 bootloader/bootloader.bin \
  0x8000 partition_table/partition-table.bin \
  0x30000 nucula.bin

# Verify: console shows "nucula>" prompt
# If still silent → try JTAG reset (below), then USB replug as last resort
```

### JTAG reset — the software-only recovery (no replug needed)

The USB-JTAG peripheral has a JTAG path **separate from the CDC serial path**. Even when the CDC is stuck, JTAG often still works. This is the first recovery to try before reaching for the USB cable:

```bash
# One-liner: connect via JTAG, issue system reset, resume, disconnect
/opt/espressif/openocd-esp32/bin/openocd \
  -f board/esp32c3-builtin.cfg \
  -c "init; reset run; shutdown"
```

If the board responds to JTAG (you see "JTAG tap: esp32c3.tap0 tap/device found"), the reset was issued. Wait 2–3 seconds for the board to reboot, then try esptool or console again.

**Recovery escalation ladder** (try in order):
```
Board stuck / unresponsive?
├── 1. Kill port holders: fuser -k /dev/ttyACM*; pkill -9 -f nucula_logger
├── 2. RTS/DTR reset: python3 -c "import serial,time; s=serial.Serial(PORT,115200); s.setRTS(True); time.sleep(0.2); s.setRTS(False); s.close()"
├── 3. esptool reset: esptool --chip esp32c3 -p PORT --after hard-reset flash-id
├── 4. JTAG reset: openocd -f board/esp32c3-builtin.cfg -c "init; reset run; shutdown"
└── 5. USB replug (nuclear — last resort)
```

Steps 1–3 fix port contention (failure mode 1).
Step 4 fixes USB peripheral corruption (failure mode 3) WITHOUT replug.
Step 5 fixes everything but requires physical access.

### Labgrid test framework (tests/hardware/nucula/)

Automated flash + test cycle — eliminates manual flash-and-pray:

```bash
# Run all nucula HIL tests
pytest tests/hardware/nucula/test_nucula.py -v --hil

# Just check board is responsive
pytest tests/hardware/nucula/test_nucula.py -v --hil -k test_board_responsive

# Flash wallet firmware and verify NCI init
pytest tests/hardware/nucula/test_nucula.py -v --hil -k test_wallet
```

Components:
- `board.py` — board metadata (pins, flash offsets, I2C devices) + esptool with port cleanup and retry logic
- `console.py` — USB-CDC capture with fast re-enumeration handling + boot markers
- `conftest.py` — pytest fixtures
- `test_nucula.py` — test pyramid: responsive → wallet boots → I2C scan → our firmware ACKs → NCI init → sustained

### esptool v5.3.1 gotcha: global options before subcommand

`--after` is a **global** option and must come BEFORE `write-flash`:
```bash
# CORRECT: esptool ... --after hard-reset write-flash ...
# WRONG:   esptool ... write-flash --after hard-reset ...  ← "No such option"
```

This bit us because the board's `run_esptool()` method originally placed
`--after` after the subcommand. If you get "No such option '--after'",
check the option ordering.

## PN7160 ACK Window (issue #63 root cause, bench-proven 2026-10-08)

The PN7160's I2C slave only ACKs when the host talks to it **immediately
after VEN rise and keeps talking**. Probing 5s after the VEN cycle = the
chip is permanently mute (NAK forever); probing 50ms after and proceeding
straight into CORE_RESET/CORE_INIT without pause = the chip responds and
stays alive. The wallet firmware always did the latter (nci_init probes
at +50ms); our firmware always probed seconds later — every earlier
theory (ISR priorities, sdkconfig diffs, bus priming, address straps,
build system) was a red herring.

Proven via C-control bisection (idf.py + IDF 5.5.1 builds of the same
sequence): v1-v3 NAK (build system exonerated — issue #83 disproved),
v4-v9 = wallet's own functions from a minimal main ACK (wifi/nvs/console/
keypad all unnecessary), v10 = the timing test that isolated the window.

The fix lives in `pn7160_i2c.rs` (v10raw-verified init: bus first, raw
gpio_config IRQ+ISR machinery, single clean VEN cycle, probe immediately)
and `pn7160_bringup.rs` (VEN re-cycle + immediate probe on NAK; ladder
runs without pause on ACK). `pn7160_v10raw.rs` is the known-good
pure-syscall reference binary — flash it first when in doubt.

**Init-order hazard**: `gpio_install_isr_service` BEFORE
`i2c_new_master_bus` hard-hangs the app (interrupt allocation deadlock,
C-control v5/v6 + Rust both). The bus must be created before any ISR
service installation.

**IRQ ISR contract**: any ISR handler on the PN7160's IRQ pin must
quench the level interrupt itself (`gpio_intr_disable`) — the chip holds
IRQ high until read; a no-op handler = interrupt storm that starves the
console/USB the moment the chip comes alive.

**Console starvation**: the NCI ladder's `Ets::delay_us` busy-wait (IRQ
polling) starves the USB console task during the ladder — console output
dies while the app + chip keep working (verify via GDB-over-JTAG, which
shows live transport state). Convert ladder waits to FreeRtos delays.

## ESP32-C3 nucula Build Flow (issues #63/#64, ai-legion)

The nucula (ESP32-C3 + PN7160) firmware builds with the nightly toolchain
(`-Z build-std` in `firmware/esp32-ccid/.cargo/config.toml`; no rustup target
needed). Build from `firmware/esp32-ccid/`:

```bash
source ~/.cargo/env && source ~/export-esp.sh
export RUSTUP_TOOLCHAIN=nightly
export ESP_IDF_SDKCONFIG="$PWD/sdkconfig.full"     # NOT plain SDKCONFIG!
cargo build --target riscv32imc-esp-espidf --no-default-features \
  --features pn7160-bringup,pn7160-verdict-b
```

### ⚠️ Global target-dir: set `CARGO_WORKSPACE_DIR` or the IDF pin is ignored

This machine's `~/.cargo/config.toml` pins `target-dir = /root/.cargo-target`
(shared by every checkout). The esp-idf-sys build script locates the root
crate by popping its OUT_DIR six levels — under a shared target dir that
lands INSIDE the target dir, `cargo metadata` finds no project, and the
build silently falls back to whatever IDF the stale `.embuild` state holds.
Observed: the bench compiled **v5.2.3 all day** while the manifest said
v5.2.4 (and later v5.5.1) — the pin was never even read. CI is unaffected
(per-repo target dir), which is why CI cloned v5.2.4 correctly while the
bench diverged.

Fix (encoded in `build.sh`):

```bash
export CARGO_WORKSPACE_DIR="<repo root>"
```

Verify after building — the `esp_idf_dir` must match the manifest pin:

```bash
python3 -c "import json,glob; print(json.load(open(glob.glob('/root/.cargo-target/riscv32imc-esp-espidf/debug/build/esp-idf-sys-*/out/esp-idf-build.json')[0]))['esp_idf_dir'])"
```

Also note: changing `esp_idf_version` in Cargo.toml does NOT rerun the build
script (nothing declares rerun-if-changed on manifest metadata) —
`cargo clean -p esp-idf-sys --target <triple>` (or delete the
`build/esp-idf-sys-*` dirs) after a version bump.

### ⚠️ Shared target-dir cmake-cache contamination (multiple checkouts, 2026-10-08 trap)

Same root cause as above, second symptom: the esp-idf-sys cmake cache
records **absolute IDF paths** under the building checkout's `.embuild`.
When two checkouts/worktrees of this repo share the global target-dir,
checkout B reuses checkout A's cache and fails with:

```
CMake Error: The source ".../components/bootloader/subproject/CMakeLists.txt"
does not match the source "/tmp/opencode/<other-checkout>/.embuild/..."
used to generate cache.  Re-run cmake with a different source directory.
```

**Fix**: `build.sh` auto-cleans contaminated esp-idf-sys build dirs (it
greps `esp-idf_SOURCE_DIR` in each CMakeCache.txt and removes any that
point outside the current workspace — expect a ~10 min full IDF rebuild
afterward). For ad-hoc `cargo build` invocations, clean manually:

```bash
rm -rf /root/.cargo-target/riscv32imc-esp-espidf/debug/build/esp-idf-sys-*
rm -rf /root/.cargo-target/debug/build/esp-idf-sys-*
```

**The stale-ELF amplifier**: a failed cargo build leaves the PREVIOUS
target's ELF in place — `elf2image` then "succeeds" on the old binary.
Never pipe `cargo build` through `grep` in scripts (the rc comes from
grep, reporting success on failure). Check the ELF mtime, or gate on
cargo's own exit (`set -o pipefail` + PIPESTATUS, or no pipe). build.sh
propagates rc correctly; ad-hoc scripts are the hazard.

Prefer ONE checkout per machine for esp-idf-sys targets, or per-checkout
`CARGO_TARGET_DIR`. Delete merged worktrees (`git worktree remove`) —
they keep building contamination for as long as they exist.

### ⚠️ The env var is `ESP_IDF_SDKCONFIG`, not `SDKCONFIG`

esp-idf-sys/embuild reads **`ESP_IDF_SDKCONFIG`**. A plain `SDKCONFIG` export
is silently ignored and the build falls back to whatever sdkconfig defaults
state is cached in the shared target dir — including STALE absolute paths
from previous working copies (this machine's global
`~/.cargo/config.toml` sets `target-dir = /root/.cargo-target`, shared by
every checkout). Verify the active path after building:

```bash
python3 -c "import json,glob; print(json.load(open(glob.glob('/root/.cargo-target/riscv32imc-esp-espidf/debug/build/esp-idf-sys*/out/esp-idf-build.json')[0]))['sdkconfig'])"
```

After changing `sdkconfig.full`, delete the cmake cache or the change is
ignored:

```bash
OUT=$(ls -d /root/.cargo-target/riscv32imc-esp-espidf/debug/build/esp-idf-sys*/out | head -1)
rm -f "$OUT/build/CMakeCache.txt"
```

The partition CSV (`partitions-ota.csv`, referenced by
`CONFIG_PARTITION_TABLE_CUSTOM_FILENAME` in `sdkconfig.full`) no longer needs
manual copying: the esp-idf-sys build script injects `ESP_IDF_GLOB_*` matches
into the embuild out dir **before cmake configure** (esp-idf-sys
BUILD-OPTIONS.md). `build.sh` exports the pair; bare `cargo build` needs them
in the environment:

```bash
export ESP_IDF_GLOB_PARTCSV_BASE="$PWD"
export ESP_IDF_GLOB_PARTCSV_1="/partitions-ota.csv"
```

CMake rewrites `sdkconfig.full` in place (kconfig normalization — new symbols
appear, deprecated ones migrate). That is expected.

### Flash (esptool, elf2image required)

esptool does not auto-convert Rust ELF output — `write-flash 0x40000 <elf>`
fails with "will not fit in flash". Convert first:

```bash
esptool --chip esp32c3 elf2image --flash-mode dio --output app.bin \
  /root/.cargo-target/riscv32imc-esp-espidf/debug/esp32-ccid
PORT=/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_90:DA:72:9A:50:18-if00
esptool --chip esp32c3 -p $PORT --baud 460800 write-flash \
  0x8000 /root/.cargo-target/riscv32imc-esp-espidf/debug/build/partition-table.bin \
  0x40000 app.bin
```

Flash the partition table (`0x8000`) whenever `partitions-ota.csv` changed —
flashing only the app leaves the old table on the board and the coredump
component reports no partition.

### Coredump decode (issue #64)

The coredump partition (`coredump, data, coredump, 0x3F0000, 0x10000` in
`partitions-ota.csv`, `CONFIG_ESP_COREDUMP_ENABLE_TO_FLASH=y` in
`sdkconfig.full`) is verified end-to-end: panic → flash write → host decode.
The scripted path is `tests/hardware/esp32_coredump.py retrieve <elf>`
(auto-discovers the embuild IDF copy + python env — the example below
pinned v5.2.3 manually and rotted when the manifest moved to v5.5.1;
prefer the script). Manual equivalent:

```bash
esptool --chip esp32c3 -p $PORT read-flash 0x3F0000 0x10000 coredump.bin
VENV=$(ls -d /root/.cargo-target/.embuild/espressif/python_env/idf*_py*_env/bin/python | tail -1)
$VENV $(ls /root/.cargo-target/.embuild/espressif/esp-idf/v*/components/espcoredump/espcoredump.py | tail -1) \
  --chip esp32c3 info_corefile --core coredump.bin --core-format raw \
  --gdb /usr/bin/gdb-multiarch \
  /root/.cargo-target/riscv32imc-esp-espidf/debug/esp32-ccid
```

GDB over the built-in USB-JTAG (same port) — **verified end-to-end on
ai-legion (issue #64)**: hardware breakpoint on `i2c_master_cmd_begin`
hit within one 5 s health-probe cycle with a fully symbolized backtrace
(IDF C → esp-idf-hal → `EspPn7160Transport::probe` → bring-up main) and
live argument values, target resumed cleanly.

Toolchain on ai-legion:
- openocd: Debian's `openocd` 0.12 package LACKS `target/esp32c3.cfg` and
  `interface/esp_usb_jtag.cfg` — use Espressif's build:
  `https://github.com/espressif/openocd-esp32/releases` (tarball extracted
  to `/opt/espressif/openocd-esp32`).
- gdb: `gdb-multiarch` works as the client (no riscv32-esp-elf-gdb needed —
  it also decodes coredumps, see above).

```bash
/opt/espressif/openocd-esp32/bin/openocd -f board/esp32c3-builtin.cfg > /tmp/openocd.log 2>&1 &
# Wait for the gdb server — openocd is still loading config / initializing
# USB-JTAG when the shell prompt returns; an immediate telnet connect gets
# ConnectionRefused and gdb then attaches without the required halt:
python3 - <<'PYEOF'
import socket, time, sys
for _ in range(50):
    try:
        socket.create_connection(("localhost", 3333), timeout=1).close()
        sys.exit(0)
    except OSError:
        time.sleep(0.2)
sys.exit("openocd gdb server never came up; see /tmp/openocd.log")
PYEOF
# Halt via the telnet command port BEFORE attaching gdb — attaching to a
# running target yields bogus registers (pc/sp zeros):
python3 -c "import socket,time; s=socket.create_connection(('localhost',4444)); s.sendall(b'halt\n'); time.sleep(1)"
gdb-multiarch -q -batch \
  -ex "file /root/.cargo-target/riscv32imc-esp-espidf/debug/esp32-ccid" \
  -ex "target remote :3333" \
  -ex "break i2c_master_cmd_begin" -ex "continue" -ex "bt" -ex "detach"
python3 -c "import socket,time; s=socket.create_connection(('localhost',4444)); s.sendall(b'resume\n')"  # leave it running
# Shut openocd down — detach/resume do NOT terminate it; a lingering server
# holds the USB-JTAG adapter and ports 3333/4444, blocking the next session:
python3 -c "import socket,time; s=socket.create_connection(('localhost',4444)); s.sendall(b'shutdown\n'); time.sleep(1)"
```

Note: small firmware fns (`probe()`) are inlined at this opt level — break on
a non-inlined callee (`i2c_master_cmd_begin`, `I2cDriver::write`) instead.
After openocd exits the USB-JTAG CDC re-enumerates; the next serial open can
block for a few seconds.

## Known Gotchas

### 1. Submodule init is required

CI checks out with `submodules: true`. Locally, after cloning or pulling a
branch that touches `reference/osmo-ccid-firmware/` or any `vendor/` crate:

```bash
git submodule update --init --recursive
```

Without this, builds fail on missing `[patch]` paths and missing reference
spec files.

### 2. st-flash soft reset leaves stale USB PHY state (issue #22)

See **USB PHY Reset Pattern** above. After `st-flash write`, the OTG FS PHY
retains stale state and the device will not enumerate without the in-firmware
PHY reset sequence. If you remove or break that sequence, flashing via st-flash
will appear to succeed but the reader will not appear on USB.

**F469 shared-board wedge table (cross-project audit 2026-10-16).** The
F469-DISCO is flashed by THREE projects (ccid Cherry reader, micronuts
wallet, bolty/gm65 scanner) and each documented half the wedge pair —
collected here; the HIL flash path
(`tests/hardware/labgrid/stm32_flash.py`) now encodes all of it:

| Wedge class | Symptom | Recovery | Automated where |
|---|---|---|---|
| Plain `st-flash write` (SYSRESETREQ) wedges target USB under FOREIGN firmware | flash "succeeds", Cherry/CDC never re-enumerates | `st-flash --connect-under-reset reset` after every write (bolty bench-sharing lesson; ccid's own firmware survives via the #22 PHY reset, other projects' does not) | `stm32_flash.flash_f469` (write + reset both connect-under-reset) |
| Running USB-CDC firmware locks out SWD | `st-flash` cannot attach (SwdDpWait) | `--connect-under-reset` attaches (micronuts firmware/AGENTS lesson); last resort: full power cycle with NRST held | same — connect-under-reset by default |
| ST-LINK dongle latch | adapter unresponsive | USBDEVFS_RESET (ioctl 0x5514) on its `/dev/bus/usb` node (.208 delta lesson) | one reset + single retry inside `flash_f469` |
| xHCI port wedge | port dead even after adapter reset | PCI remove/rescan of the port (bolty campaign lesson) | manual — rarest class |

Rule: NEVER a bare `st-flash --reset write` on this board — foreign
firmware may be on it, and the next project's session inherits the wedge.

**ESP32 serial wedge table (2026-10-19 m5stick session, #278 xtensa
leg).** The same doctrine for the serial-flash side — every ESP32 flash
goes through `tests/hardware/labgrid/esp32_flash.py`
(`flash_esp32()`), which encodes all of it:

| Wedge class | Symptom | Recovery | Automated where |
|---|---|---|---|
| pcscd SOCKET-ACTIVATION race | esptool: "device reports readiness to read but returned no data (multiple access on port?)" — daemon respawned onto the reader port mid-flash | stop BOTH `pcscd.socket` AND `pcscd` under the bench flock before flashing; restart after | `flash_esp32` quiesce/restore |
| Hades2001 FTDI baud flake | esptool: "Serial data stream stopped: possible serial noise or corruption" at 460800 | drop to the exporter-CONFIGURED speed (the m5stick port is pinned 115200) | `flash_esp32` baud cap vs exporter config |
| Erase-before-write stranding | board unbootable AND bridge dead: erased otadata + partial PT + no serial = maximally stuck (the 2026-10-19 state) | NONE remotely — physical replug + reflash; PREVENT instead: writes FIRST, destructive erases LAST, after every write landed | `flash_esp32` ordering (unit-locked) |
| FTDI/USB latch (persistent) | zero console bytes even across RTS pulses; `flash-id` fails | ladder: retry → USBDEVFS_RESET (sysfs serial match) → ftdi_sio unbind/rebind → retry; then physical replug (Gotcha #3 class) | ladder inside `flash_esp32`; verdict + board-state report on total failure |
| FTDI latency_timer reset on re-enum | pcscd error-loops after the flash (issue #51) | write 1 back to the latency_timer sysfs node | `flash_esp32` restore step |

Rule: NEVER a hand-rolled esptool sequence on a bench reader port —
the tool owns quiesce, baud, ordering, the ladder, hygiene, and the
boot signature check (`boot_check_no_conflict`, the #278 class).

### 3. espflash DTR/RTS wedge — physical replug required (issue #12)

After `espflash flash ...` on the M5Stack Atom Matrix (FTDI FT232 bridge), the
FTDI chip wedges itself via DTR/RTS toggling. The serial port disappears and
no further flashes or host-side `pcscd` communication work until the ESP32 board
is **physically unplugged and replugged** from USB.

This is a host-side FTDI driver quirk, not a firmware bug. Documented in
`CHANGELOG.md` [0.1.0] notes. Workaround: always physically replug the ESP32
board after flashing before expecting the serial CCID reader to be visible to
`pcscd`.

### 4. Default features must include `stm32f469` (issue #25)

See the CI section. Changing `default = [...]` to omit `stm32f469` breaks the
`stm32-lint` clippy pass.

### 5. ESP32 stack Kconfig symbol name (issue #21)

See **ESP32 Stack Size** above. Use `CONFIG_MAIN_TASK_STACK_SIZE`, not
`CONFIG_ESP_MAIN_TASK_STACK_SIZE`.

### 6. Release mode is mandatory for USB stability

`synopsys-usb-otg` is timing-sensitive. Dev builds (`opt-level = 1`) exhibit
unreliable USB enumeration and dropped transfers. Always ship
`cargo build --release`.

### 7. STM32F746 bitbang path is GPIO-driven, not USART

The F746 build has no smartcard-mode USART, so ISO 7816-3 is implemented in
software via `smartcard_bitbang.rs`. The F746 card clock was raised from 1 MHz
to 5 MHz (ISO 7816 maximum) — see `CHANGELOG.md` [0.1.1]. Hardware-verified at
74.4 ms average round-trip on a ComSign eID T=1 card.

### 8. F469 SRAM is capped at 256 KB

`memory.x` configures SRAM as 256 KB, not the documented 320 KB. The full 320 KB
causes a HardFault on boot (see `CHANGELOG.md` [0.0.4]). Do not "fix" this back
to 320 KB without hardware verification.

## Build Commands

### Prerequisites (one-time)

```bash
# Rust + STM32 target
rustup target add thumbv7em-none-eabihf

# ARM binutils (for objcopy)
sudo apt-get install binutils-arm-none-eabi      # Debian/Ubuntu
# brew install arm-none-eabi-binutils             # macOS

# Flashing tools (pick one)
cargo install probe-rs --features cli             # recommended
sudo apt-get install stlink-tools                 # st-flash alternative

# ESP32 (only if working on esp32-ccid)
rustup target add xtensa-esp32-espidf
cargo install espup espflash
espup install
. ~/export-esp.sh                                 # source before every ESP32 build
```

### Host tests (no hardware required)

```bash
# Full STM32 workspace host tests (matches CI stm32-test job)
cargo test --workspace --target x86_64-unknown-linux-gnu

# ESP32 host tests (matches CI esp32-host-test job)
cd firmware/esp32-ccid
cargo test --target x86_64-unknown-linux-gnu
```

### STM32 build

```bash
# Default — Cherry SmartTerminal ST-2xxx on STM32F469
cargo build --release --target thumbv7em-none-eabihf

# Gemalto CT30 profile
cargo build --release --target thumbv7em-none-eabihf \
  --no-default-features --features profile-gemalto-idbridge-ct30

# Gemalto K30 profile
cargo build --release --target thumbv7em-none-eabihf \
  --no-default-features --features profile-gemalto-idbridge-k30

# STM32F746 bitbang + Cherry profile (matches CI matrix)
cargo build --release --target thumbv7em-none-eabihf \
  --no-default-features --features "stm32f746,profile-cherry-smartterminal-st2xxx"
```

Binary location (all profiles):
`target/thumbv7em-none-eabihf/release/ccid-firmware`

### STM32 binary conversion + flashing

```bash
# ELF → .bin
arm-none-eabi-objcopy -O binary \
  target/thumbv7em-none-eabihf/release/ccid-firmware \
  ccid-firmware.bin
sha256sum ccid-firmware.bin > ccid-firmware.bin.sha256

# Flash via probe-rs (recommended — runs from ELF, resets cleanly)
probe-rs run --chip STM32F469NI target/thumbv7em-none-eabihf/release/ccid-firmware

# Flash via st-flash (remember the PHY-reset gotcha after this)
st-flash write ccid-firmware.bin 0x8000000
```

### ESP32 build

Run from `firmware/esp32-ccid/` (the workspace default target is STM32 — ESP32
commands must override or be run from the ESP32 crate dir).

```bash
cd firmware/esp32-ccid
. ~/export-esp.sh

# Default — MFRC522 backend
cargo +esp build --release

# Explicit MFRC522
cargo +esp build --release --features backend-mfrc522

# PN532 backend
cargo +esp build --release --no-default-features --features backend-pn532
```

Binary location: `target/xtensa-esp32-espidf/release/esp32-ccid`

```bash
# Flash (then physically replug the board — see Gotcha #3)
espflash flash --port <serial-port> target/xtensa-esp32-espidf/release/esp32-ccid
```

### Linting (CI parity)

```bash
cargo fmt --check
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf -- -D warnings
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf \
  --no-default-features --features "stm32f746,profile-cherry-smartterminal-st2xxx" -- -D warnings
```

## Hardware Pinout (STM32F469)

Authoritative source: `PINOUT.md`. Summary:

### Smartcard interface (ISO 7816)

| MCU Pin | Signal | Direction | Notes |
|---|---|---|---|
| `PA2` | `I/O` | Bidirectional | `USART2_TX` smartcard mode, AF7, open-drain, pull-up |
| `PA4` | `CLK` | Output | `USART2_CK`, AF7, push-pull |
| `PG10` | `RST` | Output | Card reset control (active LOW) |
| `PC5` | `PWR` | Output | Card supply gate (`LOW = ON`) |
| `PC2` | `PRES` | Input | Card detect (`HIGH = card present`) |

### USB interface

| MCU Pin | Signal | Notes |
|---|---|---|
| `PA11` | `USB_DM` | OTG FS data- |
| `PA12` | `USB_DP` | OTG FS data+ |

### F746 bitbang pins (differ from F469)

| MCU Pin | Signal | Notes |
|---|---|---|
| `PI0` | `I/O` | Open-drain, pull-up, High speed |
| `PF6` | `CLK` | Push-pull, Very High speed |
| `PI2` | `RST` | Push-pull, active HIGH |
| `PF10` | `PRES` | Floating input |
| `PF7` | `PWR` | Push-pull, active LOW = ON |
| `PK3` | Backlight | Push-pull (display) |

## Sibling-Repo Improvement Pass (August 2026, issues #28–#32)

Patterns sourced from the `gm65-scanner` project (same STM32F469I-DISCO board,
same HAL fork) and the wider Amperstrand STM32 ecosystem.

### New Modules

| Module | Location | Purpose |
|--------|----------|---------|
| DWT Watchdog | `firmware/ccid-firmware/src/dwt_watchdog.rs` | Cycle-counter-based wall-clock timeouts (ARM DWT CYCCNT). Replaces iteration-based polling. 14 host tests. |
| Diagnostics | `crates/ccid-core/src/diagnostics.rs` | Runtime counter struct (apdu_tx/rx, nak, error, reinit, card_present, uptime). 28-byte LE serialization. 12 tests (incl. byte-exact golden ported from amp-diagnostics before archival). |
| SmartcardConfig | `firmware/ccid-firmware/src/smartcard_common.rs` | Replaces 10 hardcoded `const SC_*` with a configurable struct. Values unchanged. |
| Self-Healing | `firmware/ccid-firmware/src/main.rs` (SmartcardWrapper), `firmware/esp32-ccid/src/mfrc522_driver.rs` | Re-init peripheral after 3 consecutive failures. `reinit_count` tracked. |
| Escape 0xD0 | `crates/ccid-core/src/handler.rs` (shared dispatch), fed by `firmware/ccid-firmware/src/ccid_core.rs` + `firmware/esp32-ccid/src/ccid_handler.rs` (thin wrapper) | Vendor-neutral CCID Escape diagnostic query. Payload `[0xD0]` → 28-byte Diagnostics struct. |
| HIL Harness | `tests/hardware/labgrid/` | Labgrid-native local HIL suite (2026-10-09 migration removed the old SSH style): bench flock + place acquire + identity probes + per-place suites. |

### SmartcardDriver Trait

`firmware/ccid-firmware/src/driver.rs` — added default `fn diagnostics()` method.
SmartcardWrapper (F469) overrides to return `reinit_count` + `card_present`.

### NfcDriver Trait

`firmware/esp32-ccid/src/nfc.rs` — added default `fn reinit_count()` method.
MFRC522 driver overrides to return actual count.

### HIL Testbed (local labgrid — SSH HIL retired 2026-10-09)

The bench is the local host (ai-legion, 192.168.13.208): all five devices
(stm32-ccid, nucula-c3, m5stick, ref-acr1252, ref-cardman) hang off it and
tests run ON it as local subprocesses through labgrid places. There is no
SSH anywhere in the harness; the old `--ssh-host` remote-HIL style and the
`exporter-208.yaml` remote-rig config were removed in the 2026-10-09
labgrid migration. See "Labgrid Bench Doctrine" + "Testing through labgrid"
above, and `tests/hardware/labgrid/README.md`.

Run HIL tests (bench host):
```bash
pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil
```

### libccid Configuration

To use the Escape 0xD0 diagnostic query from the host, `ifdDriverOptions` must be
set to `0x0001` in `/usr/lib/pcsc/drivers/ifd-ccid.bundle/Contents/Info.plist` on
the host running pcscd. This enables `FEATURE_CCID_ESC_COMMAND`.

## Recent Fixes

| # | Area | Summary |
|---|---|---|
| **#25** | CI | Default features in `firmware/ccid-firmware/Cargo.toml` must include `stm32f469`. The `stm32-lint` clippy pass builds with default features (no `--no-default-features`), so dropping `stm32f469` from `default` breaks CI. Current correct default: `["stm32f469", "profile-cherry-smartterminal-st2xxx"]`. |
| **#23** | HAL | `stm32f4xx-hal` dependency bumped to the Amperstrand fork pinned at rev `05d999d600d457f99aeb23ff93275d2c8f998908` (features `stm32f469`, `usb_fs`, `framebuffer`). The fork carries SDIO and USB patches not yet upstream. |
| **#22** | USB PHY | Added the OTG FS PHY reset sequence (RCC clock cycle + peripheral reset + `GRSTCTL` core soft reset + `GCCFG.PWRDWN` power cycle) at the top of `main()` for both F469 and F746 builds. Fixes re-enumeration failure after `st-flash` soft reset. Pattern proven in the microfips project. See **USB PHY Reset Pattern** above. |
| **#21** | ESP32 stack | Renamed the stack-size Kconfig from the non-existent `CONFIG_ESP_MAIN_TASK_STACK_SIZE` to the correct `CONFIG_MAIN_TASK_STACK_SIZE` in `firmware/esp32-ccid/sdkconfig.defaults`. Without this the ESP32 main task ran at the default ~3–4 KB and overflowed inside the CCID/MFRC522 path. |

## Hardware Verification History

### 2026-05 (CHANGELOG [0.1.1])
- **STM32F746-DISCO** (Cherry ST-2xxx USB CCID): 74.4 ms avg round-trip,
  ComSign eID T=1 contact card. F746 card clock raised 1 → 5 MHz.
- Both F746 and F469 firmware builds verified clean.
- ESP32 hardware testing pending (M5Stack Atom disconnected).

### 2026-04-24 (CHANGELOG [0.1.0])
- **ESP32 + MFRC522** (GemPC Twin serial): `pcscd` detects reader, NFC card responds.
  - Card: NXP P71 SmartMX3 P71D320 JCOP4 JavaCard
  - ATR: `3B 85 80 01 80 73 C8 21 10 0E` (TCK correct)
  - Reader: `GemPCTwin serial 00 00`
- **STM32 + Specter DIY Shield** (Cherry ST-2xxx USB CCID): `pcscd` detects reader,
  contact card responds.
  - Card: ComSign eID (T=1, IFSC=254)
  - ATR: `3B D5 18 FF 81 91 FE 1F C3 80 73 C8 21 10 0A` (TCK correct)
  - Reader: `Cherry GmbH SmartTerminal ST-2xxx (ST2XXX-001) 02 00`
- Both readers verified **simultaneously** on the same host.
- All host tests pass: STM32 82/82, ESP32 75/75, iso14443 52/52.

## References

- USB CCID Specification Rev 1.1 — `docs/CCID_SPEC_AUDIT.md`, `docs/AUDIT_PLAN.md`
- ISO 7816-3 (smartcard electrical/protocol)
- Reference device specs — `reference/CCID/readers/*.txt` (authoritative)
- osmo-ccid-firmware (protocol reference) — `reference/osmo-ccid-firmware/` (submodule)
- Specifications index — `docs/SPECIFICATIONS.md`
- Hardware validation procedures — `tests/hardware/README.md`
- stm32f4xx-hal (Amperstrand fork): https://github.com/Amperstrand/stm32f4xx-hal
- probe-rs: https://probe.rs

## Serial Performance Notes (issue #51/#51-closure, 2026-08-30)

Measured decomposition of the ~16ms APDU round-trip (vs ACR1252 ~2ms), via
byte-level serial probing on the rig (method in .omo evidence, session
amperstrand-nfc-mcu-dedup):

- Wire time @115200 8N2 (command + echo + response) ≈ 5-6ms.
- Firmware gap (CCID handling + MFRC522 I2C @100kHz + card I/O) ≈ 10ms —
  the dominant term and the real optimization lever.
- **Untried zero-firmware host tweak**: FTDI `latency_timer` is 4 — setting
  it to 1 (`/sys/bus/usb-serial/devices/ttyUSB0/latency_timer`) removes up
  to ~4ms of USB batching latency. Try this FIRST before any firmware work.
- Baud >115200 is host-blocked: libccidtwin hardcodes `cfsetspeed(B115200)`
  and reader.conf has no speed knob; a local ccid fork would buy ~4.6ms at
  the cost of permanent host-fork maintenance — rejected. #54 tracks the
  upstream change needed (speed knob + SIMPro2-style escape negotiation;
  firmware side is ready: `UartDriver::change_baudrate()` + escape dispatch).

## External posting (owner directive 2026-09-06 — CHANNEL rule)

Agents never post on non-member repos — no `gh` writes (issues, PRs,
comments, reviews, gists), not even with per-text owner sign-off; the
owner does the copy-paste into GitHub themselves. Member orgs (verify:
`gh api user/orgs`; 2026-09-06: Amperstrand, OpenTollGate, net4sats,
FreedomTechFeed) keep the existing owner-gate flow. Read the target
repo CONTRIBUTING/AI policy before drafting anything upstream.
Canonical text: lightning-playground AGENTS.md (standing rule UPDATE
2026-09-06).

## Lab hardware: one single source of truth (2026-09-28)

All lab hardware — routers, ESP32/STM32 fixtures, switches, power, serial —
is declared in the **private repo `Amperstrand/conwrt-lab`** (`lab.yaml`:
devices, MACs, IPs, topology, VLANs, labgrid places). This project does NOT
own hardware state.

Rules for code and agents in this repo:

1. **Never hardcode device IPs, MACs, or serial-port paths.** Resolve from the
   registry: `CONWRT_LAB=<conwrt-lab checkout>` +
   `python3 $CONWRT_BENCH/scripts/lab_registry.py …`, or from a labgrid place.
2. **Access hardware through labgrid places** (coordinator `ai-legion:20408`):
   `labgrid-client -p <place> ssh|console|power`. Acquire/release for
   exclusivity during tests.
3. **Flashing/adoption goes through conwrt tooling** (`Amperstrand/conwrt-bench`):
   `scripts/dut_recover.py --from-lab <device-id>` for recovery flashes,
   `scripts/bench_net.py` for bay/VLAN changes. These update the registry.
4. **State changes end with a registry commit** in conwrt-lab — if you changed
   a device (flashed, moved, adopted), lab.yaml must reflect it before you
   walk away.
5. **When surprised, reconcile first**:
   `python3 $CONWRT_BENCH/scripts/lab_registry.py reconcile` detects dead
   devices, switch reboots, and place drift in one command.

Env: `CONWRT_LAB` (this registry checkout) and `CONWRT_BENCH` (conwrt-bench
checkout) are the two paths every hardware-touching tool understands.
