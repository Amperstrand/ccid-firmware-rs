# Engineering Hardening Tracker — ifd-modemifd + ccid-firmware-rs

Persistent implementation ledger for the systematic hardening mission.
Survives context compaction / session restarts. Update after every work
package. Reporting format per package: Finding / Impact / Implementation /
Verification / Remaining risk / Next task.

## Repository baselines (update on resumption — recheck git state, never trust memory)

| Repo | Path | Branch | Baseline SHA | Notes |
|---|---|---|---|---|
| ccid-firmware-rs | /home/ubuntu/src/ccid-firmware-rs | sisyphus/ifd-nucula-hardening (PR #457) | 7ce8a84 | main = origin/main f6b3433; local uncommitted deletion `firmware/esp32-ccid/reader.conf` (pre-existing, DO NOT TOUCH); worktree ccid-docs = stale drafting site, subsumed |
| ifd-modemifd | /home/ubuntu/src/ifd-modemifd | sisyphus/v34-no-resend-hardening (PR #2) | 2d50e52 | master = origin/master 3a475eb (v4.0); v3.4/v3.4.1 rebased on v4.0, 15/15 mock_at green post-rebase; untracked .omo/ |

**Published (session 2, 2026-10-24):** PR Amperstrand/ccid-firmware-rs#457
(bounds + TX semantics + decode, commits bc04025/7ea7318/2086cfa/d634dc3/d2d9b79
+ tracker docs); PR Amperstrand/ifd-modemifd#2 (v3.4/v3.4.1). Issue replies
posted on #450/#453/#455. NO merge before Codex review of the head (AGENTS rule 0).

## Hardware truth (session 2 recon, 2026-10-24)

- **Mission NR7101 = 192.168.1.10** (Zyxel NR7101, OpenWrt 25.12.6 r33311,
  ramips/mt7621, uptime fresh; SSH root@ works key-based). Quectel modem on
  /dev/ttyUSB0-3 (RG502Q-class). NO socat bridge running on :7002 yet.
  Reachable from ai-legion-small via enx00e04c3e7b58 (192.168.1.127/24).
- Registry conwrt-lab lab.yaml lists nr7101 (192.168.12.124) + nr7101-a6e4
  as dark-zycast-corrupt since 2026-10-22 — STALE vs the live .10 unit;
  owner reconciliation pending (do not edit registry from this mission).
- **Coordination hazard:** the w19:p4 tollgate session uses this router's
  modem-data WAN (usb0) for an E2E price test — NO CFUN cycles, modem
  reboots, or AT commands that reset the module until that lane closes.
  Read-only AT identity (ATI/AT+CGMM/AT+CPIN?/AT+CRSM reads) is safe.
- Deployed bench .so /usr/local/lib/libifd-modemifd.so = 2026-10-23 12:39
  (pre-fix; /tmp/modemifd.log 0644 world-readable live evidence). System
  pcscd reader.conf still points at dead 192.168.1.1:7002.
- Bench host for ccid HIL (ai-legion .208) is a DIFFERENT machine; nucula
  hardware rounds run there per AGENTS bench doctrine.

Key instructions read: ccid-firmware-rs AGENTS.md (doctrines: reference-first,
fix-all-siblings, card-handling/GP budget, clean-wire, CI protocol),
BUILDING.md, README; ifd-modemifd README (known issues: response-lag
aliasing, TCP-only, single-reader). Cross-project tracking issue:
Amperstrand/conwrt-bench#22 (OPEN; owner posts per-session reports there).

Baseline verification (2026-10-24):

- `host-tools/ifd-nucula`: `make all` clean; `make selftest` 9/9 PASS
  (mock board, legacy card profile).

## Prioritization table

| ID | Priority | Repository | Finding | Status | Verification | Commit/PR |
|---|---|---|---|---|---|---|
| H-001 | P0 | ccid-firmware-rs (host-tools/ifd-nucula) | C IFD uses CCID-message max (271) as payload max; TX path (`IFDHTransmitToICC`, `IFDHControl` escape) and RX path (`nuc_recv_frame_locked`) overflow 274-byte stack frame buffers by up to 10 bytes for payload/dwLength 262..271 | **FIXED (local)** | bounds_test.c ASan/UBSan RED→GREEN 24/24; selftest 9/9; hardware bench round pending | aba34a6 (+broker 3b5c74c); issue #450 |
| H-002 | P2 | ccid-firmware-rs (host-tools/ifd-nucula) | `nuc_t1_from_atr` interface-byte walker indexed atr[len..len+2] on truncated ATRs (OOB read, ASan-proven) | **FIXED (local)** | bounds walker group 3/3 RED→GREEN; full bounds 27/27 | 424e9be; issue #450 |
| H-001s | — | ccid-firmware-rs (host-tools/nucula-ccid-broker.py) | Sibling: `SANE_MAX=13+271` treated 262..271 dwLength anchors as "sane partials" (wedge class #375) | **FIXED (local)** | broker suite 5/5 (new band test) | 3b5c74c; issue #450 |
| H-003 | P0 | ccid-firmware-rs (host-tools/ifd-nucula) — nucula half | `nuc_transact_locked` resent CCID commands after post-send failures (echo loss / response loss) — duplicate state-changing APDU risk | **FIXED (local)** | tx_semantics_test 22/22 (exactly-one-frame invariants; safe-retry path proven) | 903efc7; issue #453 |
| H-003m | P0-candidate | ifd-modemifd | modemifd twin: `at_expect`/transport recovery may re-send AT+CSIM after ambiguous delivery; synthetic 6F00 masks transport failures | **IN PROGRESS (next)** | pending | — |
| H-004 | P1 | ifd-modemifd | AT+CSIM response validation hardening (framing, final result code, length checks, synthetic 6F00 classification) | merged into H-003m work | pending | — |
| H-005 | P1 | ccid-firmware-rs (host-tools/ifd-nucula) | CCID response decode audit: bmCommandStatus/bError mapping, time-extension, seq mismatch handling | investigation pending | pending | — |
| H-006 | P3 | ccid-firmware-rs (host-tools/ifd-nucula) | SIGPIPE dependence on host disposition in send path | **FIXED** (send MSG_NOSIGNAL, rode with 903efc7) | tx-semantics suite | 903efc7 |
| H-007 | P1 | ccid-firmware-rs (host-tools/ifd-nucula) | IFDHPowerICC silent ATR drop on short caller buffer (IFD_SUCCESS, empty ATR) | **FIXED (local)** | bounds 33/33 (board-backed group, RED→GREEN) | eb54716; PR #457 |
| H-008 | P2 | ccid-firmware-rs (host-tools/ifd-nucula) | IFDHGetCapabilities ATR read without g_ctx.lock (torn pair vs nuc_cache_atr writer) | **FIXED (local)** | bounds GetCapabilities check; make check green | cf6bfe2; PR #457 |
| H-009 | P1 | ccid-firmware-rs (tests/hardware/nfc) | #431: battery verdict counting — soak ok-overwrite false PASS, fuzz dead-port abort, conformance false red | **FIXED (local, software half)** | offline sim 0/5→5/5; broker 5/5; bench round pending | be3c3cf; #431 / PR #457 |

## Work package log

### WP-1 — H-001 Nucula C IFD frame-bounds overflow (Task 1.1)

**Finding.** Wire layout: `[SYNC][CTRL][10-byte CCID header][payload dwLength][LRC]`.
The Rust codec (canonical reference: `crates/ccid-protocol/src/types.rs`
`MAX_CCID_MESSAGE_LENGTH=271` = header+payload; `crates/ccid-transport-serial/src/lib.rs`
`MAX_CCID_PAYLOAD=261`, `MAX_FRAME_BYTES=274`, header-stage reject of
`payload_len > 261`) caps the payload at **261**. The C driver defines the
right constants (`NUC_MAX_CCID_MESSAGE 271`, `NUC_MAX_FRAME 274`) but uses
**271 as the payload bound** in three places:

1. `IFDHTransmitToICC` (ifd-nucula.c:786): `TxLength > NUC_MAX_CCID_MESSAGE`
   accepts host APDUs of 262..271 bytes → `nuc_build_request` memcpy at
   frame+12 into `req[NUC_MAX_FRAME]` on `nuc_command`'s stack writes up to
   index 282 + LRC at 283 ⇒ up to 10-byte stack smash. Reachable by any
   local PC/SC client.
2. `IFDHControl` CCID-escape path (ifd-nucula.c:874): same check, same
   overflow via SCardControl.
3. `nuc_recv_frame_locked` (ifd-nucula.c:326): `dwlen > NUC_MAX_CCID_MESSAGE`
   accepts peer-claimed dwLength 262..271 → `nuc_read_all_locked` writes up
   to 271 bytes at frame+12 into 274-byte stack buffers (`echo` in
   `nuc_transact_locked`, `resp` in every caller) + LRC store at 283.
   Reachable from the TCP peer (plaintext TCP transport ⇒ spoofable peer).

Also: stale comment claims "payload(<=271) + LRC = 274" (2+10+271+1=284≠274).

**Impact.** Memory corruption in a pcscd-loaded host driver, both
host-initiated (local client) and peer-initiated (remote). P0.

**Implementation.** (complete, commits aba34a6 + 3b5c74c + 424e9be, local
on main — push/PR owner-gated; issue #450 tracks)

- `NUC_MAX_CCID_PAYLOAD 261` derived explicitly; `NUC_MAX_FRAME` derived
  arithmetically; stale comment corrected.
- `nuc_build_request` refuses oversize payload (returns 0 = error);
  `nuc_command` validates `req_len` before any send;
  `nuc_recv_frame_locked` rejects `dwlen > 261` at header stage (matches
  the firmware codec); `IFDHTransmitToICC`/`IFDHControl` reject
  `TxLength > 261` before touching buffers.
- Sibling: broker `SANE_MAX` 284→274 + comment fix + band regression test.
- H-002 in the same issue: `nuc_t1_from_atr` per-read bounds guards.

**Verification.**

- `make bounds` (ASan+UBSan): pre-fix all three overflow groups ABORT
  (RED evidence in issue #450); post-fix 27/27 PASS.
- `make selftest` (wire level vs mock board): 9/9 PASS pre and post.
- Broker suite: 4/4 → 5/5 PASS (new 262..271-band test).
- Builds: `-Wall -Wextra -Werror` clean.

**Remaining risk.** No hardware bench round yet (real board, pcscd-level
smoke) — recorded as pending in #450. Local commits not pushed (owner gate).

**Next task.** WP-2 (Task 1.2): ambiguous APDU retransmission audit in
`nuc_transact_locked` (ifd-nucula) and `at_expect` recovery (ifd-modemifd).

### WP-2 — H-003 nucula ambiguous-APDU retransmission (Task 1.2, nucula half)

**Finding.** `nuc_transact_locked` retried the identical CCID command on
every failure class, including post-send ones (echo lost, response lost).
GemPC echo semantics: the firmware echoes only after reading a COMPLETE
frame — a lost echo/response after a locally-successful write means the
XfrBlock may already be executing on the card. Resend = potential
duplicate state-changing APDU (GP SC, eSIM, PIN — #127 incident class).
TCP reality: small-frame write() always completes locally, so peer
mid-read death is indistinguishable from post-execution death.

**Impact.** eSIM/GP/PIN operations could silently execute twice through
pcscd — invisible to the calling application.

**Implementation.** 903efc7: typed outcomes NUC_TX_OK / NUC_TX_NOT_SENT
(send failed mid-frame ⇒ strict prefix ⇒ provably unexecuted ⇒ retry
allowed) / NUC_TX_AMBIGUOUS (post-send ⇒ never resend). Retry restricted
to the provably-not-sent case. send(MSG_NOSIGNAL) hardening included
(H-006). Issue #453.

**Verification.** tx_semantics_test.c (fake-board thread, per-connection
fault script: NONE/MIDFRAME/CLOSE_EARLY/NO_ECHO/NO_RESPONSE + dead-write
socketpair for true send failure): 22/22 PASS under ASan+UBSan. The
no-echo/no-response exactly-one-frame invariants FAIL on pre-fix code
(two frames = the duplicate). Full `make check` green.

**Remaining risk.** Hardware bench round pending (with #450). pcscd-level
behavior (client sees COMMUNICATION_ERROR on ambiguity) not yet exercised
end-to-end — smoke target covers happy path only.

**Next task.** WP-3: ifd-modemifd — audit `at_expect`/CSIM recovery for
the same retransmission class + Task 1.3 response-parser hardening
(reader.rs, at.rs; mock AT server tests per Phase 5).

### WP-3 — ifd-modemifd Tasks 1.2 + 1.3 + Phase-2 logging (commits 4086396, b258d52)

**Finding.** reader.rs: (a) at_full classified read timeouts as `Ok` and
at_expect retried on Ok-without-echo and on Transport — both reachable
after the AT line was fully written (stalled bridge / response lost /
connection cut post-delivery) ⇒ silent AT+CSIM re-send to the SIM
(duplicate state-changing APDU). (b) parse_csim ignored the declared
length, accepted any size, and parsed the FIRST +CSIM line in the buffer
including pre-echo stale responses (response-lag aliasing). (c) Response
accumulation unbounded. (d) Everything non-Ok collapsed into synthetic
6F00, hiding transport failures. (e) Debug logging compiled on
(`if true || env`), full APDU hex world-readable in /tmp (2.7 MB bench
evidence), symlink-redirectable.

**Implementation.** Typed `AtOutcome { Ok, Error, NotSent, Refused,
Ambiguous }`; retry ONLY on NotSent (write failed before CR left host ⇒
provably unexecutable). Refused (echo seen, no final answer) keeps v3.3
6F00 semantics; Ambiguous (no echo after full write) → transport error,
never a fake SW, never retried. Strict parse (declared length == hex
chars, TS 27.007 §8.45 / pySim convention; R-APDU ≤ 258 B) +
echo-correlated parse_csim_in. 8 KiB read cap. Logging: opt-in, 0600,
no-symlink, APDU-redact-by-default, IFD_MODMIFD_LOG_PATH override.

**Verification.** tests/mock_at.rs 15/15 (scripted mock modem:
no-resend invariants under blackhole/post-delivery-cut/echo-silence/
missing-OK/flood; echo correlation; strict framing; URC tolerance);
tests/logging*.rs 2/2; cargo build --release + example clean.

**Remaining risk.** No live-modem bench round yet (RG502Q): pacing
behavior with the stricter final-OK requirement unchanged in theory;
response-lag aliasing needs a live thrash test. Deployed pre-fix .so on
the bench still logs to the old world-readable file until rotated
(owner-gated).

**Next task.** WP-4: H-005 — CCID response decode audit in ifd-nucula
(bmCommandStatus/bError mapping, time-extension, seq mismatch), then
Phase 3 IFD conformance checks for both host drivers.

### WP-4 — H-005 nucula CCID response decode (Task 1.4) — commit 505b5f3, issue #455

**Finding.** (1) `IFDHTransmitToICC` reported a FAILED XfrBlock
(bmCommandStatus=failed, card present) as IFD_SUCCESS with payload —
CCID command failures were successful IFD exchanges. (2) Interim CCID
6.2-2 "still processing" SlotStatus frames were rejected as final
answers instead of waited out. (3) bSeq mismatch left the desynced
connection open for the next command.

**Verification.** tx_semantics 32/32 (10 new decode checks, RED→GREEN
for 1+2), stress 15/15 after root-causing a fake-board counter race
(quiesce barrier added). Full make check green.

**Remaining risk.** Time-extension path untested against real firmware
(current wifi firmware emits no interims — conformance tolerance only).
No hardware bench round yet for WP-1..WP-4 (all software-only evidence).

### WP-5 — ccid-quality lane: PowerICC ATR contract + GetCapabilities lock + #431 battery verdicts (commits eb54716, cf6bfe2, be3c3cf, PR #457)

**Finding.** (1) `IFDHPowerICC` with a caller ATR buffer smaller than
the ATR skipped the copy but still returned IFD_SUCCESS — pcscd reads
a silent empty ATR as a successful power-on. (2) `IFDHGetCapabilities`
(TAG_IFD_ATR / SCARD_ATTR_ATR_STRING) read `g_ctx.atr/atr_len`
unlocked while `IFDHPowerICC`/`nuc_cache_atr` write them under
`g_ctx.lock` — torn len/data pair possible. (3) #431 software half:
`slot_status_soak` overwrote the function-level `ok` per iteration (a
misbehaving/dead reader followed by a clean reader FALSE-PASSED the
soak); fuzz dead-port aborted the whole battery (AssertionError);
conformance FAILed dead-port cases (bench fault shown red).

**Impact.** (1) violates the IFD buffer contract exactly as the
ifd-modemifd twin (modem-core WP, same semantics: INSUFFICIENT_BUFFER +
required size, no partial copy). (2) data-race class on the ATR cache.
(3) battery verdicts lied in both directions: false PASS (soak) and
false red (conformance) on bench faults.

**Implementation.** eb54716: PowerICC returns IFD_ERROR_INSUFFICIENT_BUFFER
with `*AtrLength` = required size; ATR still cached (TAG_IFD_ATR keeps
serving it); board-backed bounds_test group (6 checks, fake-board
thread). cf6bfe2: GetCapabilities copies the atr pair under the mutex,
decides after unlock; TAG_IFD_THREAD_SAFE=0 rationale documented (one
global ctx — mutex serializes the single ctx, cannot isolate two
instances; 0 is about instance isolation, not unlocked-access
permission). be3c3cf: Reader dead-port tracking (reopen result, no
longer ignored in exchange()); soak per-iteration verdict local; fuzz
dead-port cases SKIP loudly (quick-reopen cadence while dead, one
probe per case) with WEDGE reserved for port-back-but-no-resync;
conformance dead-port cases SKIP and drop out of the agreement table;
final verdict "PASS with SKIPS" + per-reader skip counts.

**Verification.** `make check` 69/69 PASS (selftest 7/7, bounds 33/33
— 27 baseline + 6 new, tx-semantics 29/29; NOTE: this tree's counts,
the WP-4 "32/32" was the pre-PR-assembly worktree). PowerICC group
RED→GREEN (3 checks FAIL pre-fix). #431 offline sim (fake serial +
virtual clock, /tmp/opencode/431/battery_sim.py): 0/5 pre-fix → 5/5
post-fix (soak false-pass with failing reader FIRST, fuzz abort,
conformance false-fail, skip accounting). Broker pytest 5/5; ruff
(scripts tests/hardware host-tools) clean.

**Remaining risk.** Battery sim is offline-only; the dead-port paths
need one bench round on a real dropped port to validate timing (the
quick-reopen cadence assumes the by-id node reappears promptly). No
hardware bench round yet (stacks with the WP-1..WP-4 pending round).

**Next task.** none for this lane (complete); PR #457 push/merge is
owner-gated with the Codex-review rule (AGENTS rule 0).

### WP-6 — modem-hil sacrificial-SIM suite, PREP-ONLY build (branch
sisyphus/nr7101-sacrificial-suite, ifd-modemifd worktree, 2026-10-24)

**Finding.** The v4.0 monolithic validate.sh gate is unsafe as a template
for the campaign: it stopped the SYSTEM pcscd for its direct-AT probe and
hardcoded a router SSH target. Session-2 constraint: NR7101 owned by
another lane (w19:p4 uses its modem-data WAN) AND coordinator directive —
no SIM is to be sacrificed for real; the 9eSIM production card must never
be a suite target. So the suite was built software-only with a mock-modem
dry-run as the default execution mode.

**Impact.** Enables the full Class-A qualification + eSIM-gate rehearsal
with zero hardware, zero AT traffic, zero pcscd interaction, and makes the
card-safety rule machine-enforced rather than doctrine-only.

**Implementation.** Six local commits on sisyphus/nr7101-sacrificial-suite
(NOT pushed, per owner gate): mock_modem.py (RG502Q basic-channel A4/P1=04
+ B0 drop fingerprint, fake SIM w/ ICCID + ISD-R AID); report.py (stamped
JSON+MD, enforced truthful PASS/FAIL/SKIP/BLOCKED — PASS requires evidence,
suite PASS only if every case ran, dry-run can never open the provisioning
gate); suite.py (case runner: direct-AT backend mirroring driver v3.4
transport semantics, ctypes pcsc backend selecting reader BY FRIENDLYNAME
at two layers — never first-reader; card guard: any run reaching a real
card requires --card-decl, 9esim-naming values refused outright,
sacrificial suite requires card-decl=sacrificial); validate.sh split into
validate-sacrificial.sh (SAC-001..009: basic SELECT-by-FID, drop
fingerprints whose DROP is the expected PASS, logical-channel cycle with
EF.ICCID fixture fingerprint) + validate-esimgate.sh (ESG-001..004: ISD-R
readiness via public-AID select, lpac chip info EID stability) chained by a
dispatcher that BLOCKs esimgate when sacrificial is not green; lane-runner.sh
(private socket-activated pcscd lane per ccid AGENTS.md recipe,
PCSCLITE_CSOCK_NAME-only, --plan, hw interlocks: no default MODEMIFD_ADDR,
HW_CONFIRM for non-loopback, MODEMIFD_CARD required, 9eSIM refused);
READMEs updated.

**Verification.** shellcheck clean (all tests/hardware/*.sh), py_compile
clean (all *.py), report.py --self-test 14/14, suite.py --self-test 21/21
(includes full suite runs vs mock: filter on/off, URC chaos), end-to-end
dry-run of validate.sh and lane-runner.sh --dry-run (sacrificial 9/9 PASS
annotated dry-run; esimgate 3 PASS + 1 SKIP → suite SKIP, chain exit 2 —
never green for unexecuted), BLOCKED-chain path proven by killing the mock
mid-run (prerequisite BLOCKED propagation), hw interlocks proven refusing
(no addr / no confirm / no card-decl / card-decl=9esim → exit 3), cargo
test regression 15/15 mock_at + 2 logging (no Rust changes; .so builds via
lane-runner with repo-local CARGO_TARGET_DIR pin).

**Remaining risk.** pcsc backend happy path UNVERIFIED (needs a live
private pcscd instance — no pcscd interaction this session by directive);
lpac ESG-004 unverified beyond unit parsing (lpac absent on this host);
direct-AT bridge backend verified only against the mock, not a real socat
bridge. All deferred to the hardware session once the NR7101 lane is
handed over AND a disposable SIM is seated.

**Next task.** Hardware round (blocked on lane handover): lane-runner.sh
--plan review, MODEMIFD_ADDR=<bridge> MODEMIFD_CARD=sacrificial hw run,
then pcscd-lane verification of the FRIENDLYNAME path end-to-end. Queued
as ifd-modemifd issues #15 (suite first hw qualification), #16 (private
pcscd lane + FRIENDLYNAME live proof), #17 (esimgate lpac/EID path), #18
(direct-AT backend vs real bridge + parity), #19 (SAC_REPEATS soak).

Round-execution agreements (docs-release check-in, 2026-10-24 ~17:40):
- **Binary pin for #4/#5**: must testify about the ROTATED deployed .so —
  /usr/local/lib/libifd-modemifd.so, sha256
  f859540cd2a8aaf0358da730c4bb2fbcc9eb99c2b63a4c7b6376023856ca4b69
  (= master fb5f8bc content, built from 035ffe5; fb5f8bc docs-only on
  top). Run via lane-runner SO_FILE pin, sha verified before+after,
  reports kept separate from branch-build runs. Pre-fix backup:
  libifd-modemifd.so.pre-v342.bak (0600 root).
- **#4/#5 carried by modem-hil** (unassigned until comment-claim at round
  start); #4's literal "validate.sh phases" are superseded by the split —
  mapping (300x/150x loads → #19 SAC_REPEATS; bridge-down observability →
  #18) to be commented on #4 at start; docs-release mirrors into
  RELEASE-GATE rows when stamped reports land. #5 runs LAST (URC-thrash
  recovery may need CFUN — only after hazard formally cleared).
- **Handover/fallback**: docs-release takes #4/#5 against the rotated .so
  if modem-hil stalls on handover or disposable-SIM availability. System
  reader.conf dead entry stays dead while the lane is modem-hil's.

## Session state (session 2, 2026-10-24 — active, coordinator: w19:pC)

- Lanes (Herdr-managed OpenCode sessions, own worktrees under ~/src/worktrees/):
  - **modem-core** — ifd-modemifd Phase-3 IFDHandler conformance sweep
    (IFDHControl/GET_FEATURE_REQUEST, GetCapabilities, ATR contract, LUN
    registry, CGLA-fallback path) + pacing doc (250 ms floor vs 400 ms p50
    round-trip distinction). Software-only, mock AT TDD.
  - **ccid-quality** — COMPLETE (WP-5): ifd-nucula PowerICC
    ATR-buffer contract (eb54716), GetCapabilities lock + THREAD_SAFE=0
    rationale (cf6bfe2), #431 battery verdict SKIP counting (be3c3cf).
    make check 69/69, broker 5/5, sim 5/5. Hardware round pending with
    WP-1..WP-4.
  - **modem-hil** — PREP-ONLY COMPLETE (WP-6, session-2 constraint: NR7101
    owned by another lane + coordinator directive that no SIM is actually
    sacrificed and the 9eSIM is never a target — both now enforced in code
    via MODEMIFD_CARD/--card-decl interlocks). Software suite built on
    branch sisyphus/nr7101-sacrificial-suite (6 local commits, not pushed):
    validate.sh split into validate-sacrificial.sh (class-A ops: SELECT by
    FID, EF reads via LOGICAL channels, drop fingerprints) +
    validate-esimgate.sh (read-only EID/chipInfo/ISD-R gate), lane-runner.sh
    private-pcscd lane (reader BY FRIENDLYNAME, never first), mock-AT
    dry-run default, JSON+MD stamped truthful reports. All software gates
    green (shellcheck/py_compile/self-tests 14+21/dry-run e2e/BLOCKED
    chain/interlocks/cargo 15/15). Hardware round pending lane handover +
    disposable SIM (see WP-6 remaining risk).
  - **docs-release** — README accuracy (modemifd pacing/logging/no-resend
    sections; ccid TESTED_AND_WORKING consistency), release-gate skeleton
    (versioned targets table, PASS/FAIL/SKIP/BLOCKED), onboarding, portfolio.
- Prior session's follow-up list stands: IFDHPowerICC fix (→ ccid-quality),
  Phase-3 sweep (→ modem-core + ccid-quality), CGLA hardening + live-modem
  round (→ modem-core/modem-hil), deployed-.so rotation (owner-gated),
  Phase-4 reliability #444/#380/#431 (post-Phase-3), ccid-pcsc-host
  reader-matching (later).

### Lane status — docs-release (2026-10-24, COMPLETE)

**Finding.** README drift vs enforced code: claimed a 250 ms
inter-command floor with "idle recovery" while src/reader.rs enforces
`TRANSMIT_GAP_MS=400` (no idle-recovery logic exists); claimed "every
export wrapped in catch_unwind" (8/10 guarded; two constant stubs are
not); response-lag-aliasing limitation predated v3.4 echo correlation;
v3.4/v3.4.1 semantics entirely undocumented; no release-gate artifact.

**Implementation.** Branch `sisyphus/docs-release-readiness` (ifd-modemifd
worktree), 5 local commits on 2d50e52, docs only, no code touched:

- 4ed1973 — pacing truth: 400 ms floor (constant named), presence-probe
  exemption, anchored to v4.0 measured p50 400 ms round-trip.
- 7470a54 — v3.4 transport semantics: AtOutcome table (Ok → response
  APDU / 6F00-on-unparsable; Error+Refused → SW 6F00 card-level;
  NotSent+Ambiguous → IFD_COMMUNICATION_ERROR; NotSent the ONLY retried
  class, once; never re-sent after full delivery), strict framing
  (declared len == hex count, ≤258 B R-APDU), echo-correlated parse,
  8 KiB cap; limitation entry updated (narrowed since v3.4, residual
  post-echo shape unobserved, live thrash test pending).
- 7f352bb — v3.4.1 logging: IFD_MODMIFD_DEBUG gate,
  IFD_MODEMIFD_LOG_APDU hex gate (exact spellings flagged — MODMIFD vs
  MODEMIFD genuinely differ in code), IFD_MODMIFD_LOG_PATH override,
  0600 + symlink refusal, OnceLock cache ⇒ vars must reach pcscd.
- 1982261 — docs/RELEASE-GATE.md: PASS/FAIL/SKIP/BLOCKED semantics +
  zero-FAIL release rule; evidence registry (software reproducible:
  mock_at 15/15, logging 2/2; hardware recorded-only: v4.0 table
  2026-10-23); per-version targets v3.2 f0e71a / v3.3 03848b3 /
  v4.0 3a475eb / v3.4 fa42259 / v3.4.1 2d50e52 — every PASS names its
  evidence; SKIP/BLOCKED registry for gaps (post-v3.4 hardware round
  and deployed-.so rotation BLOCKED; serial transport, multi-reader,
  conformance sweep SKIP with reasons). No aspirational claims.
- 5871249 — cross-check fixes: catch_unwind scope, eSM→eSIM typo,
  RELEASE-GATE pointer; all remaining README claims verified against
  src/+tests (architecture, modem findings incl. post-v3.4 6F00
  semantics, validate.sh phases/env overrides, fake ATR, TCP-only,
  LUN registry, build artifacts).

**Verification.** `cargo build --release` clean and `cargo test
--release` 17/17 (mock_at 15, logging 2) both before and after the doc
commits (docs-only diff — identical binaries). Working tree clean,
nothing pushed.

**Remaining risk.** modem-core's pacing-doc task overlaps this README
pacing fix — their doc should cross-reference, not restate. RELEASE-GATE
hardware rows need superseding entries once modem-hil's bench round
lands (gate says so itself).

**Next task.** lane complete. Published follow-ups (2026-10-24, owner
pushed): PR Amperstrand/ifd-modemifd#3 (docs branch
`sisyphus/docs-release-readiness`, stacked on PR #2 head 2d50e52 —
auto-retargets to master when #2 merges, lands as fast-forward;
reviewer requested, Codex-head gate before merge per AGENTS rule 0).
Testing queue filed as issues #4 (post-v3.4 HW re-validation round,
BLOCKED on modem-hil + tollgate hazard), #5 (live URC-flood thrash /
echo-correlation residual), #6 (deployed-.so rotation, owner-gated
ops), #7 (pcscd-level ABI smoke test for by-value SCARD_IO_HEADER —
closes the v3.2 evidence gap, software-runnable now).

### Lane status — modem-core (2026-10-24, COMPLETE)

**Finding.** Phase-3 conformance audit of the exported IFDHandler ABI
against /usr/include/PCSC/{ifdhandler,reader}.h: (1) `IFDHControl`
returned IFD_SUCCESS for EVERY ioctl — no GET_FEATURE_REQUEST
dispatch, vendor escapes got silent empty success. (2)
`IFDHGetCapabilities` 1-byte tags (SLOTS_NUMBER, THREAD_SAFE,
SIMULTANEOUS_ACCESS) wrote `*Value` without a NULL check — NULL Value
with Length>=1 is a null-pointer write (latent: stock pcscd always
passes a real buffer). (3) README claimed a 250 ms pacing floor;
src/reader.rs enforces `TRANSMIT_GAP_MS=400`. (4) LUN registry
multi-reader semantics had zero coverage (README: "untested beyond one
reader").

**Impact.** (1) breaks the pcscd feature-probe contract and hides
unsupported-control from clients; (2) is a memory-safety defect at the
pcscd FFI boundary (defense-in-depth class); (3) misleads operators
sizing pacing budgets; (4) left "multi-reader works" unproven.

**Implementation.** Branch `sisyphus/phase3-conformance` (modem-core
worktree), 4 local commits on 2d50e52, nothing pushed:

- e73bfa4 — IFDHControl: `CM_IOCTL_GET_FEATURE_REQUEST`
  (SCARD_CTL_CODE(3400) = 0x42000D48, mirrored in consts.rs) →
  IFD_SUCCESS + `*returned=0` (zero-feature advertisement: pcscd
  registers an empty feature list instead of erroring the reader);
  every other ioctl → IFD_ERROR_NOT_SUPPORTED with `*returned`
  zeroed (ifdhandler.h note). GetCapabilities 1-byte tags gained the
  NULL-Value guard → IFD_COMMUNICATION_ERROR (matches the TAG_IFD_ATR
  branch). consts module made pub so tests assert canonical values.
- 5105028 — Phase-3 conformance battery in tests/mock_at.rs (10 new
  tests, 25/25 total): control dispatch incl. SCARD_CTL_CODE(3401)/
  (4680) rejections; PowerICC buffer contract proven (exact ATR copy /
  INSUFFICIENT_BUFFER with needed size / no partial copy / null
  rejection; POWER_UP runs the 3-APDU session normalize exactly once
  and still returns the fake ATR); GetCapabilities ATR + 1-byte-tag
  size semantics, Length=0 size query, ERROR_TAG zeroing, NULL Value;
  TransmitToICC INSUFFICIENT_BUFFER needed-size + null-Tx; LUN
  registry two-reader isolation / close-removes-everywhere (close
  idempotent) / recreate-rebinds-to-new-target / invalid+NULL device
  names refused unregistered. MockModem CSIM counter now counts only
  APDU-carrying CSIMs (bare `AT+CSIM=?` probe excluded).
- 0a60b59 — README: enforced 400 ms floor named (TRANSMIT_GAP_MS,
  constant unchanged), p50-400 ms round trips explained as the floor
  manifesting, env-configurable pacing TODO noted; multi-reader
  "untested" limitation retired (mock coverage; physical 2-modem
  round still pending).
- 9b813d9 — style: rustfmt over pre-existing drift (fmt --check gate
  requires it; reflow only, no semantic change).

**Verification.** `cargo test` 27/27 (mock_at 25 + logging 2),
`cargo build --release` clean (sole warning is a pre-existing unused
import), `cargo fmt --check` clean. RED→GREEN evidence:
`control_rejects_all_non_feature_ioctls` FAILED pre-fix (everything
returned SUCCESS); `getcaps_contracts` debug-aborted on the `*val`
null write pre-fix.

**Remaining risk.** README pacing bullet overlaps docs-release lane
commit 4ed1973 (their branch rewrote it too) — trivial merge conflict,
owner reconciles at PR time. CGLA-fallback path (named in the lane
charter) deferred with the CGLA-hardening/live-modem round. POWER_DOWN
returns the fake ATR instead of zeroing it — documented deviation (a
modem cannot power its SIM down; pcscd keys presence on
IFDHICCPresence, benign). All software-only evidence; no live-modem
round.

**Next task.** none for this lane (complete); PR cut owner-gated with
the rest of session 2.

**Follow-up (same session, owner-approved push).** Branch pushed as
PR Amperstrand/ifd-modemifd#11 (5 commits: the four above + 98ce752;
stacked on PR #2's head, base `sisyphus/v34-no-resend-hardening`,
retarget master after #2 merges; README pacing bullet will trivially
conflict with #3's 4ed1973 — reconcile at merge).

- 98ce752 — pacing TODO closed: `IFD_MODMIFD_TX_GAP_MS` env override
  (default 400 unchanged, unparsable → default, 0 = off) with a
  RED→GREEN timing test in tests/pacing.rs (own binary; env must
  precede the OnceLock-cached gap). Suite now 28/28.
- Test queue filed on ifd-modemifd: #8 live Phase-3 conformance round
  on NR7101 (tollgate-gated: GET_FEATURE/GetAttrib/PowerICC/TX_GAP
  validation; supersedes RELEASE-GATE SKIP rows), #9 physical
  multi-reader LUN round (two phases: shared-port plumbing, then true
  two-card isolation), #10 CGLA/CCHO/CCHC fallback — capability-routed
  at session init, never per-APDU retry (no-resend doctrine; audit
  confirmed zero CGLA code in src/ today). Adjacent existing: #4
  post-v3.4 live round, #5 URC-flood thrash, #6 .so rotation, #7 ABI
  smoke.

### Lane status — docs-release round 2 (2026-10-24 late: merges + Codex audit closed)

Owner granted review/test/merge authority on ifd-modemifd (bot wait
waived). Executed:

- **Merges.** PR #2 → master (merge commit fea2a95 — preserves
  fa42259/2d50e52 SHAs cited by RELEASE-GATE); PR #3 (docs) retargeted
  and rebase-merged (710beca). Master content verified identical to the
  tested tree (empty diff) before each merge.
- **Codex audit (post-merge, runbook followed).** Re-triggered review
  completed with 3×P2: legacy-0644 log stays world-readable (mode() is
  create-only); symlink pre-check→open TOCTOU; RELEASE-GATE v3.4.1 row
  claimed symlink-refusal PASS with no symlink test. All ADDRESSED in
  PR #12 (7d1a37c/31e18ca): fchmod fd-repair, O_NOFOLLOW atomic refusal
  (cfg-gated), symlink evidence test. RED→GREEN. 19/19.
- **Audit round 2.** Codex flagged the repair itself (EPERM discarded on
  foreign-owned writable files → appends continue). ADDRESSED in PR #13
  (989a930/035ffe5): fail closed. Repro nuance discovered: sticky /tmp +
  fs.protected_regular=2 masks the class — test stages a non-sticky dir.
  21/21 (logging battery now 5 binaries / 6 tests).
- All 4 finding threads replied + 👍; report in Amperstrand/pr-review-audit
  (amperstrand-ifd-modemifd-2-report.md, rounds 1–2). Codex round on #13
  triggered, in flight — process on landing.
- **Master now 035ffe5** (v4.0 + v3.4 + v3.4.1 + docs + v3.4.2). Local
  master FF'd. PR #11 (modem-core) retargeted to master as their entry
  planned — rebase + README pacing conflict vs 4ed1973 remains theirs
  (note: their IFD_MODMIFD_TX_GAP_MS will need a README mention).
- Test queue unchanged: issues #4–#7 (this lane) + #8–#10 (modem-core).

### Lane status — docs-release round 3 (2026-10-24 evening: issue #6 rotation executed)

Under the standing owner authority grant: deployed-bench-.so rotation
(the live exposure) executed on ai-legion-small, zero PC/SC clients in
the window (verified via socket check before stop).

- Old binary aac7b263 (Oct 23 12:39, pre-fix, was STILL writing
  /tmp/ifd-modemifd-reader.log 0644 seconds before rotation — 3.46 MB
  and growing) backed up root-only 0600; new binary f859540c = master
  035ffe5 installed, sha256-verified.
- 3.49 MB secret-bearing logs deleted after checksum+size evidence
  capture (recorded in issue #6 close comment).
- pcscd restarted; NR7101_SIM_dut initializing via new lib; NO new
  /tmp logs on the live daemon (default-off honored end-to-end).
- Issue #6 closed with report; RELEASE-GATE BLOCKED row superseded
  (PR #14, master fb5f8bc).
- Unchanged/known: reader.conf still targets dead 192.168.1.1:7002 —
  bridge bring-up + retarget to the live NR7101 (192.168.1.10) is the
  hardware lane's; PR #11 (modem-core) still open, theirs to land.

### Lane coordination — docs-release <-> modem-hil (2026-10-24, via Herdr)

- modem-hil (idle, WP-6 prep done, hardware queue #15–#19): round
  blocked on lane handover (w19:pC -> modem-hil), disposable test eUICC
  seated, socat bridge up on :7002 w/ AT autodetect. #16 (private-pcscd
  lane vs loopback mock) runnable today, no real AT. Tollgate hazard
  FORMALLY HOLDING — cleared by coordinator, not pane state; their queue
  is read-only AT except #5's CFUN-recovery (runs last, post-clearance).
- Agreement: modem-hil CARRIES issues #4/#5 in its round, SO_FILE pinned
  to the rotated deployed .so (f859540c, path confirmed; evidence kept
  separate from branch builds). #4's "validate.sh phases" wording will be
  superseded by the split-suite mapping (comment on issue at start;
  docs-release mirrors into RELEASE-GATE when stamped reports land).
  Fallback: lane hands to docs-release if handover/SIM stalls (#4/#5
  don't depend on the new machinery). #4/#5 left unassigned for their
  comment-claim. No reader.conf action from docs-release while the lane
  is modem-hil's.
- docs-release observed: modem-core now on branch sisyphus/pcsc-smoke
  (past PR #11 phase; adjacent to issue #7 territory — to check).
