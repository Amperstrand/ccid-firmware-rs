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

## Session state (session 2, 2026-10-24 — active, coordinator: w19:pC)

- Lanes (Herdr-managed OpenCode sessions, own worktrees under ~/src/worktrees/):
  - **modem-core** — ifd-modemifd Phase-3 IFDHandler conformance sweep
    (IFDHControl/GET_FEATURE_REQUEST, GetCapabilities, ATR contract, LUN
    registry, CGLA-fallback path) + pacing doc (250 ms floor vs 400 ms p50
    round-trip distinction). Software-only, mock AT TDD.
  - **ccid-quality** — ifd-nucula follow-ups on PR #457 branch: IFDHPowerICC
    ATR-buffer-too-small → IFD_ERROR_INSUFFICIENT_BUFFER; IFDHGetCapabilities
    atr read without mutex; #431 false-positive battery verdicts (software
    half). Full local gates (make check, workspace tests, fmt, clippy ×2).
  - **modem-hil** — SOLE hardware owner. NR7101 read-only capability
    inventory (fixture identity/fingerprint FIRST, PIN state without retry
    consumption, AT-endpoint discovery), then Class-A smoke suite +
    validate.sh split (sacrificial-SIM qualification vs eSIM-provisioning
    qualification; private pcscd instance; JSON+MD stamped reports in
    persistent dirs; never-green-for-unexecuted).
  - **docs-release** — README accuracy (modemifd pacing/logging/no-resend
    sections; ccid TESTED_AND_WORKING consistency), release-gate skeleton
    (versioned targets table, PASS/FAIL/SKIP/BLOCKED), onboarding, portfolio.
- Prior session's follow-up list stands: IFDHPowerICC fix (→ ccid-quality),
  Phase-3 sweep (→ modem-core + ccid-quality), CGLA hardening + live-modem
  round (→ modem-core/modem-hil), deployed-.so rotation (owner-gated),
  Phase-4 reliability #444/#380/#431 (post-Phase-3), ccid-pcsc-host
  reader-matching (later).
