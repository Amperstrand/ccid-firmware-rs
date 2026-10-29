# GP-CONFORMANCE-LANE — #139 GlobalPlatform conformance lane (READ-ONLY)

TRACK L deliverable (issue #139): the GP Card Specification study, the
GlobalPlatformPro source map, the #132 decisive-experiment runbook, and
the read-only probe skeleton (`host-tools/gp_probe.py`).

> **FRAMING — this lane is READ-ONLY by construction.** No GP
> authentication attempts were made or are needed for anything in this
> document. Every fact below comes from (a) upstream open source
> (GlobalPlatformPro, cited by file:line at a pinned commit), (b) the
> bench card ledger already on record (issues #127/#132/#144/#155,
> `docs/HIL-VERIFICATION-PROTOCOL.md` §1), or (c) free public proxies
> (GPPro wiki). The card-handling doctrine (AGENTS.md) applies to every
> future run of anything described here: SC-class commands
> (INITIALIZE UPDATE / EXTERNAL AUTHENTICATE / SC-gated GET DATA) are
> budget-bearing, never looped, and every attempt is ledgered.

## 0. Ground rules (why none of this touches firmware)

The reader-role vs standalone-role doctrine (AGENTS.md, owner directive
2026-10-12) is load-bearing for this lane: **the CCID reader is a
transparent TPDU-level pipe; the host owns the GP secure channel.**
Every GP APDU below travels host↔card through our relay untouched —
`smartcard.rs` `transmit_raw` forwards whole T=1 blocks; the reader never
inspects, reassembles or answers GP content (proof table in
`docs/t1_relay_132_rootcause.md` §0). The purpose of this document is
UNDERSTANDING the wire traffic our pipe carries (and giving the #132-class
forensics a repeatable runbook) — quote-locking nothing wire-side because
GP bytes are host↔card, not ours.

## 1. The bench J3R180 fleet mapped to the GP Card Specification

### 1.1 What a J3R180 is in GP terms (verified exposure)

All bench J3R180s are NXP P71D321 silicon (JCOP4, fab batch 6782)
carrying a GP Card Specification secure element. Their observed GP
exposure, with the authority for each row:

| GP surface | Bench value | Authority |
|---|---|---|
| ISD AID | `A000000151000000` (GP 2.1.1 ISD RID prefix) — answers SELECT with 9000 on the bench twins; the legacy `A000000003000000` OP-CardManager AID is 6A82 on these cards | bench 2026-10-12, `tests/hardware/nfc/cardlib.py:26-31`; GPPro's fallback ISD constant is the same bytes (`GPData.java:31`) |
| ISD discovery | an empty SELECT (`00 A4 04 00` + Le) returns an FCI the host parses for the ISD AID; GPPro relies on this and has a JCOP-unfused (6A82) workaround and a 6A87 (SmartJac) fallback | `GPSession.java:113-168` |
| Card manager state | `gp -l` prints `ISD: A000000151000000 (OP_READY)` on keyed cards | wiki "Secure Channel Establishment"; bench #132 close-out |
| SCP version | SCP02 (`INITIALIZE UPDATE` response byte 11 = `02`) on every fingerprinted card; key version byte 10 = `FF` (keys live at version 255) | #127 table, #155; layout per `GPSession.java:363-367, 404-417` |
| SCP 'i' parameter | read by hosts only for SCP03 (byte 12 when byte 11 = `03`, `GPSession.java:412-414`; SCP02 → i=0, `GPSecureChannelVersion.java:40-42`). The bench "i-params" fingerprints (`04,33,F5,DB` / `17,06,98,0D`) are raw response bytes 12..15 as logged by the #127 probe — a **family marker of unverified offset semantics**, not a parsed GP field. Pinning the exact offsets is what `gp_probe.py --parse-init-update` is for (offline decode of an already-captured hex string) | #127, #155; this doc §2.4 |
| CPLC (GET DATA `9F7F`) | keyless read, case-2 short-Le (`80 CA 9F 7F 00`), BER-TLV `9F7F <len> <42B>`; field layout ICS-fab/type, OS id/date/level, ICF fab/type/**serial(4)**/batch(2) — identity pin needs ≥32 hex chars (through ICF serial) | GP layout locked in-repo at `tests/hardware/nfc/j3r180_gpg_lifecycle.py:149-155` (GP Card Spec v2.3 §11.9 / Table 11-86); GPPro sends the same shape (`GPData.java:366-368, 417-421`) |
| GET STATUS | **rides the secure channel in practice** — GPPro lists `-l` among the options that force authentication (`GPTool.java:1272-1281`) and sends GET STATUS only through the wrapped channel (`GPSession.java:1043-1048, 1092-1116`). Keyless GET STATUS is card-policy-dependent; on these cards expect the 69xx refusal class. Tolerated GET STATUS errors: 6A88 (no data), 6A86/6A81 for P1=0x10 — "GPC 2.3.1 Table 11-39" per `GPSession.java:1064-1077` | GPPro source |
| SC-gated GET DATA | `00E0` Key Info Template is sent keyless by GPPro `gp -i` (`GPData.java:358-362, 417-421`) — but the bench doctrine classes GET DATA 00E0 as SC-class because the locked card 50947320 answers `69FF` to it (#144) and budget discipline is bench-first | #144; GPPro source |

**SCP03 exposure: none observed.** Every INIT_UPDATE fingerprint on
record is SCP02 (byte 11 = 02). If a card ever reports SCP03, the host
response shape changes (byte 12 = 'i', optional pseudorandom-challenge
bit 0x10 → +3-byte sequence, S16 bit 0x01 → 16-byte challenges; response
29(32)/45(48) bytes — `GPSession.java:363-396`) and key material must be
AES, not 3DES. The bench AUTH triple is 16-byte 3DES-class (three
distinct 16-byte keys), i.e. SCP02-shaped; no SCP03 keyset exists
in-repo. S16 mode comes from GP Amendment D v1.1.1 (wiki "Secure Channel
Establishment", "SCP03 with enhanced security (S16 mode)").

### 1.2 The card ledger — key history per CPLC (READ-ONLY compilation)

Compiled from `docs/HIL-VERIFICATION-PROTOCOL.md` §1.2 (the registry),
#127 (the inventory + key-hunt history), #132 (path-dependence + Teapot
proof), #144 (the lock incident), #155 (the 50906420 family fingerprint).
**Key = CPLC serial/batch. Nothing below required a new auth attempt.**

| Card (serial/batch) | Pre-pers | INIT_UPDATE fingerprint | Key history / auth state | Ruling (doctrine) |
|---|---|---|---|---|
| `50947320/6782` | `1619` | SCP02, keyver FF | **ISD LOCKED — permanent** (#144: ~450 abandoned INIT_UPDATEs exhausted the JCOP error budget; `69FF` pre-auth at key-ver 00 AND 01; survives VCC power-off; no host-side recovery exists — GPPro wiki: "Too many tries (usually 3 or 10) with incorrect keys can irreversibly lock (brick) your card … Only the card vendor can help you.") | applet fixture only (SmartPGP v1.23.0 lives on it); NO GP install/delete; no SC-class attempts even though nothing left to lose |
| `50942720/6782` | `1647` | SCP02, keyver FF, bench "i" `04,33,F5,DB` | **AUTH triple PROVEN** — `gp -l` full success via ACR1252 (#132); the original two Cherry-path mismatches were the #132 transport bug (root-caused, fixed 46c4701, proven by the Teapot lifecycle) | keyed GP-management card; budget: 2 mismatches + successes |
| `50796920/6782` | (factory card) | OP_READY | **AUTH triple PROVEN** through OUR Cherry reader: clean SCP02 auth + full Teapot CAP lifecycle first attempt (#132 close-out; `tests/hardware/labgrid/test_gp_lifecycle_hil.py` CPLC-gates on this serial) | the dedicated install/delete target |
| `50972320/6782` | `164B` | SCP02, keyver FF, bench "i" `04,33,F5,DB` | keys unknown — 3 auth attempts burned under the org-wide key hunt; **verdict via commercial OmniKey (stands)** | read-only relay test card; budget spent, do not repeat; later re-designated m5stick lifecycle pilot (HIL-VERIFICATION-PROTOCOL §1.2) |
| `50861320/6782` | `1633` | SCP02, keyver FF, bench "i" `17,06,98,0D` | keys unknown — **verdict SUSPECT**: only ever tested through our m5stick firmware (the #101 UART-RX-truncation class); must be retested through a commercial reader before it stands (#132) | read-only until re-verdicted; never a budget source for speculative auth |
| `50906420/6782` | n/a (post-#127) | SCP02, keyver FF, bench "i" `04,33,F5,DB` (family match with 50942720 per #155) | unknown — not in the #127 ledger; default-ISD SELECT answers `6A82`; one transport-class gp failure occurred with **no EXT_AUTH sent — no budget burned** (#155) | do NOT run gp against it (speculative auth); one INIT_UPDATE-only fingerprint on record |
| nucula-coil card (serial pending, #327) | ships SmartPGP v1.23.2.0 preloaded | — | AUTH triple presumed, unprobed (budget discipline) | applet fixture; full-CPLC capture queued (#327) |

The fixed org keyset (single source of truth chain:
`tests/soak/soak_02_globalplatform.py` → `tests/hardware/nfc/J3R180-RUNBOOK.md`
§Keys → `j3r180_gpg_lifecycle.py:47-52`) is **CPLC-allowlist-gated**
(`J3R180_CPLC_ALLOWLIST`, `--allow-unverified-cplc` as the only
override — `j3r180_gpg_lifecycle.py:158-211`, regression-locked by
`tests/hardware/nfc/test_j3r180_ledger.py`). GPPro's equivalent safety
posture is the opposite default: **it silently defaults to the test key
`404142434445464748494A4B4C4D4E4F` when no keys are given**
(`GPTool.java:346-354`, `PlaintextKeys.java:25-28`) — which is exactly
how budget gets burned by accident; never rely on gp's default.

### 1.3 What INIT_UPDATE fingerprinting costs (and what it doesn't)

The #127 fleet was fingerprinted with **single** raw `80 50 00 00 08
<8 random bytes>` probes, no EXTERNAL AUTH — recorded then as "no retry
counters consumed". The #144 incident later proved the honest nuance:
**an abandoned INIT_UPDATE challenge is still a budget-bearing
authentication event on JCOP-class ISDs** — a single one is the
documented identification pattern (#127), a loop of them is the lockout
mechanism (#144). The doctrine synthesis (AGENTS.md safe/unsafe table):
INIT_UPDATE = SC-class = budgeted singles, ledgered, never looped;
fingerprinting a NEW card is ONE aimed single, decided against the
family evidence (the #155 coin-flip framing).

## 2. GlobalPlatformPro source map

### 2.0 Provenance

All citations: **martinpaljak/GlobalPlatformPro, branch `next`, commit
`dd1a4c6e324fdbe47bf9b7f5e0073f5eeeb2498c` (2026-09-29, "Build
updates.")** — `library/src/main/java/pro/javacard/gp/*.java` and
`tool/src/main/java/pro/javacard/gptool/GPTool.java`, fetched raw from
GitHub on 2026-10-29. The bench tool is gp.jar v25.10.20 (CalVer build
of the same project); the studied flow is the current `next` CLI
(`gp`/`GlobalPlatformPro` replaces the historical `GPTool` main name —
same package). Line numbers are that commit's. Wiki citations:
github.com/martinpaljak/GlobalPlatformPro/wiki (Keys; Secure Channel
Establishment), read 2026-10-29.

Spec-number policy (docs/SPEC-SOURCES.md): GP Card Specification clause
numbers appear here ONLY where GPPro itself prints them inline (each
with the GPPro file:line) or where already locked in-repo (CPLC layout).
Everything else cites GPPro code — the exact bytes our host emits are
the operational truth anyway.

### 2.1 `gp -i` / `--info` — the KEYLESS flow

`GPTool.java:324-326`: `-i` runs `GPData.dump(channel)` on the **plain
channel, before any authentication** (the wiki states it explicitly:
"This command can be used without opening a secure channel"). Sequence:

1. **ISD discovery** (`GPSession.discover`, `GPSession.java:113-168`):
   empty SELECT `00 A4 04 00`+Le (line 119) → parse FCI `6F`/`84` for
   the ISD AID (161-165). Workarounds: 6A82 → SELECT the JCOP
   identification AID `A000000167413000FF` and read the fuse byte
   (123-135, "Unfused JCOP detected"); 6A87 → connect to the default
   ISD `A000000151000000` (138-142; constant at `GPData.java:31`); 6283
   passes through with "Card Manager is LOCKED" (144-148).
2. **GET DATA ladder** (`GPData.dump`, `GPData.java:318-363`) — each via
   `getData()` (`GPData.java:417-432`): CLA `80` first, and **on error
   retry once with CLA `00`** (420-422); 6A88 = "N/A", other SW =
   "not supported" (both non-fatal):
   - CPLC `80 CA 9F 7F` (366-368, `failsafe=true`)
   - IIN `00 42`, CIN `00 45`, KDD `00 CF`, SSC `00 C1` (325-344)
   - Card Data `00 66` (347-350), Card Capabilities `00 67` (353-356,
     with a nested-0x67 parser bugfix noted at 253)
   - Key Info Template `00 E0` (358-362; `GPSession.java:294` fetches it
     again post-auth for the dual key-encoding check)

So "the `gp -i` equivalent" as a command list is: `SELECT(empty)`,
then GET DATA `{9F7F, 0042, 0045, 00CF, 00C1, 0066, 0067, 00E0}` with
the 80→00 CLA fallback. Nothing in it initiates a secure channel — but
note which responses are card-gated (§1.1 last row: 00E0 on the locked
bench card answers 69FF).

### 2.2 `gp -l` / `--list` — the AUTHENTICATED flow

1. Same discovery SELECT as above (`GPTool.java:285-292`).
2. **Authentication is forced**: `-l` is in `needsAuthentication`'s list
   (`GPTool.java:1272-1281`) → `gp.openSecureChannel(keys, null, null,
   mode)` (`GPTool.java:379-389`, with the inline comment "IMPORTANT
   PLACE. Possibly brick the card now, if keys don't match"). See §2.4.
3. **GET STATUS ladder** (`gp.getRegistry()` at `GPTool.java:866-867`
   → `GPSession.java:1027-1033` → `getStatus()` 1092-1116), all through
   the wrapped channel:
   - `80 F2 02 4F00` P1=0x80 (ISD) — line 1096; P2=0x02 requests TLV
     tags (`getConcatenatedStatus`, 1043-1045); a 6A86 answer triggers
     ONE re-issue with P2=0x00 (no tags; 1055-1059)
   - P1=0x40 (apps + security domains) — 1100
   - P1=0x10 (load files with modules; profile-gated) — 1107
   - P1=0x20 (load files) — 1112
   - SW 6310 = more data: continuation loop with `P2|0x01` until 9000
     (1083-1088); 6A88 tolerated as "no data" (1065-1068);
     6A86/6A81 on P1=0x10 demoted to debug (1069-1077, citing
     "GPC 2.3.1 Table 11-39")
4. Formatting: `GPCommands.listRegistry` (`GPCommands.java:48-82`) —
   the `ISD:`/`APP:`/`PKG:`/`SSD:` lines, lifecycle strings, privileges.

### 2.3 INITIALIZE UPDATE — exact bytes both directions

Command (`GPSession.java:338-343`): `80 50 <P1=key version> <P2> <8-byte
host challenge>` where P2 = key ID for SCP01 else `00` (line 340); host
challenge random via `GPCrypto.random` (332-336). The only auto-retry:
SW 6700 → assume implicit S16, **fresh** 16-byte challenge, one re-send
(346-352).

Response layouts — verbatim from the upstream comment
(`GPSession.java:363-367`), the field map the #132 forensics used:

```
SCP01:    kdd(10) | key info(2) | card challenge(8)        | card cryptogram(8)         = 28
SCP02:    kdd(10) | key info(2) | seq(2) | card ch.(6)     | card cryptogram(8)         = 28
SCP03 S8: kdd(10) | key info(3) | card challenge(8)        | card cryptogram(8) | seq(3)= 29(32)
SCP03 S16: same with 16-byte challenges                    = 45(48)
key info = kvn | scp | i(scp03)
```

Parse order (`GPSession.java:398-445`): KDD = bytes 0..10 (400);
key version byte 10 (404); SCP byte 11 (408); 'i' byte 12 only when
SCP=03 (412-414); card challenge bytes 12..20 for SCP02 (420 — the
8-byte field whose first 2 bytes ARE the sequence counter, seq sliced
at 12..14 line 430); card cryptogram next 8 bytes (424); length checked
against the SCP-class table (374-396) — a 27/29-byte answer to an
SCP02 exchange is itself a finding. Explicit key-version mismatch is
its own error (448-451).

**KDD on these cards:** the 10 bytes carry ICF fabricator/type +
serial + batch (bench: `47 90 D3 21 50 94 73 20 67 82`-shaped — which
is why #132 could predict "KDD offset 6: expect 27" for card
`50 94 27 20`). Diversification uses the KDD (§2.6); the AUTH-triple
cards run NO diversification (plain per-card keys, no `-key-kdf`).

### 2.4 EXTERNAL AUTHENTICATE + the mutual-auth verification ORDER (#139's question)

The order, source-pinned (`GPSession.openSecureChannel`):

1. Derive session keys (477-488; SCP02 context = the sequence counter).
2. **Verify the CARD cryptogram FIRST** (490-507):
   `mac_3des(S-ENC, host_challenge ‖ card_challenge)` (493-494) vs the
   response field; mismatch →
   `"Card cryptogram invalid!"` + `"!!! DO NOT RE-TRY THE SAME
   COMMAND/KEYS OR YOU MAY BRICK YOUR CARD !!!"` (501-504) — **thrown
   before any EXTERNAL AUTHENTICATE is sent**.
3. Only then compute the host cryptogram
   `mac_3des(S-ENC, card_challenge ‖ host_challenge)` (516-518) and send
   **EXTERNAL AUTHENTICATE `84 82 P1 00 <8-byte host cryptogram>`**
   (528-530) where P1 = security-level bits (CLR 00 / MAC 01 / ENC 02 /
   RMAC 10 / RENC 20 — `GPSession.java:1118-1134`); card rejection →
   `"EXTERNAL AUTHENTICATE failed"` (531) — GPException carries the SW
   (6982 = security status not satisfied per `GPData.java` SW table).

**Implication for the #132 bench wording.** The bench reports said
"host-cryptogram mismatch (AUTH)" — there is no such string in GPPro.
The only mismatch-class failure before EXT AUTH is `Card cryptogram
invalid!` — a **host-side** computation over the received INIT UPDATE
response bytes. Therefore the failing exchanges never reached the card's
own host-cryptogram check: the divergence lived in the INIT_UPDATE
response as received (wrong bytes, or right bytes associated with the
wrong host challenge). With keys proven correct via ACR1252 (#132
evidence table), that pinned the fault to the transport — consistent
with the confirmed mechanism (H1: the 500 ms first-byte timeout orphaned
a late INIT_UPDATE response and let libccid pair a stale/late block with
a fresh retry's challenge; `docs/t1_relay_132_rootcause.md`). **Rule for
future triage: name the exact gp error string in the report — "Card
cryptogram invalid" vs "EXTERNAL AUTHENTICATE failed" vs "INITIALIZE
UPDATE failed, card LOCKED?" (6982/6984, `GPSession.java:354-357`) are
three different evidence classes** (transport/wrong-keys vs card-side
rejection vs lockout).

### 2.5 SCP02 crypto (what a transport bug would have to preserve)

- Session keys: `3DES-CBC(derivation_data, card_key, IV=0)` with
  derivation data = `constant(2) ‖ seq(2) ‖ 00×12`; constants
  ENC=`01 82`, MAC=`01 01`, RMAC=`01 02`, DEK=`01 81`
  (`PlaintextKeys.java:35-41` map, `362-372` derivation;
  `GPCrypto.mac_3des` 156-166).
- Card cryptogram: `mac_3des(S-ENC, host_ch ‖ card_ch)`; host cryptogram
  same MAC over the swapped concatenation (`GPSession.java:493-497,
  516-518`).
- Wrapped APDUs (`SCP02Wrapper.java`): CLA bit 0x04 set, MAC = 3DES
  over `CLA‖INS‖P1‖P2‖Lc‖data` with ICV chained; first command ICV=0,
  subsequent ICV encrypted under S-MAC (comment + 74-96); DES3-CBC
  pad80 for ENC level (98-104). This is the per-APDU profile the
  T=1 relay must carry byte-exact.
- Key diversification: opt-in (`-key <kdf>:<master>`; templates
  `emv`/`visa2`/`visa`/`kdf3` at `PlaintextKeys.java:50-59`; wiki Keys
  §Diversification: the KDD comes from the INIT UPDATE response and
  the algorithm "is not strictly standardised"). The bench AUTH triple
  is used WITHOUT diversification — three literal keys.

### 2.6 gp's retry policy (what it will and won't re-send)

- INIT UPDATE: sent **once**; the single re-send class is the 6700→S16
  hop with a fresh challenge (`GPSession.java:346-352`). No
  wrong-key retry loop exists in the tool — the failure path is the
  brick-warning exception (501-504).
- EXTERNAL AUTHENTICATE: once (529-531).
- GET DATA: one CLA-fallback re-send (`GPData.java:420-422`); GET
  STATUS: the 6310 continuation is spec-mandated pagination, plus one
  no-tags fallback on 6A86 (§2.2) — post-auth reads, no budget risk.
- The tool defers to the wiki doctrine: "Too many tries (usually 3 or
  10) with incorrect keys can irreversibly lock (brick) your card"
  (Keys page, repeated three times). Host-side APDU-level retransmit
  (if any) lives below GPPro in apdu4j/pcsc and surfaces as comm
  errors, not as new auth attempts.

## 3. The #132 decisive experiment — reusable bench runbook

**Status:** #132 is CLOSED — root cause confirmed (H1: 500 ms
first-byte timeout + Ok-on-timeout contract in `transmit_raw`; fix
46c4701; hardware proof: clean SCP02 auth + full Teapot lifecycle
through the Cherry reader first attempt). This section is the
**reusable form** of the staged procedure from the root-cause dig
(§3 of `docs/t1_relay_132_rootcause.md`, preserved on #132):
it is what to run for the NEXT path-dependent GP failure (or a
regression repro of this one). Stages 1-2 spend **zero** auth budget;
stage 3 spends exactly **one**. Total: ≤1.

### 3.0 Pre-flight gates (all stages)

1. **No-blind-INIT_UPDATE guard** — before any stage 3 decision, the
   card must be ledger-identified:
   `python3 host-tools/gp_probe.py --dry-run` (offline check), then on
   hardware with `--execute` (SELECT ISD + GET DATA CPLC only) and match
   the CPLC against §1.2. Unregistered CPLC → STOP; a new card needs an
   owner ruling on ONE fingerprint single (the #155 framing), never an
   improvised `gp -l`.
2. **Seat check**: cards move (HOP-1) — verify the EXPECTED card's CPLC
   through the reader you are about to test, not the last-seen seat.
3. **Place + pcscd discipline**: `labgrid-client -p stm32-ccid acquire`;
   every pcscd stop/start under `flock /run/lock/bench-pcscd.lock`
   (AGENTS.md bench rules); reader picked by STABLE identity
   (`--reader "...(ST2XXX-001)..."`, index resolved fresh).
4. **Baseline the counters**: CCID Escape 0xD0 before/after every stage
   (`apdu_tx`/`apdu_rx` must balance — relay-transparent evidence).

### 3.1 Stage 1 — zero-attempt instrumented read-only session

Card in slot, NO auth, NO INIT UPDATE (a fingerprint single is neither
needed nor spent here):

```bash
# RTT tap (terminal 2):
DEFMT_LOG=debug probe-rs run --chip STM32F469NI \
  target/thumbv7em-none-eabihf/release/ccid-firmware
# read-only session (terminal 1) — via the probe script:
python3 host-tools/gp_probe.py --execute --reader "Cherry ... (ST2XXX-001) ..." \
  --verbose   # SELECT ISD + GET DATA CPLC, nothing else
```

Watch the RTT log for the H1/H2/H3 signatures (observable names from
`smartcard.rs`): `Rx timeout …ms SR=…` (first-byte timeout — the H1
smoking gun), `USART err SR=… byte=…` (PE bit0 / ORE bit3 — H2/H3),
`xfr: tx=…c frx=…c …` cycle stamps (`frx`/168 MHz = µs to first
response byte — the latency histogram), and 0xD0 counter balance.
**Verdict table:** any first-byte timeout during a card exchange ⇒
receive-layer timing bug (H1 class) — fix, stage 3 optional. PE on
specific byte values (the `0x27`-pattern class) ⇒ electrical (H2).
Clean stage 1 ⇒ stage 2.

### 3.2 Stage 2 — zero-attempt cross-reader byte diff

Same card, same read-only command set, through the commercial oracle
(ACR1252): capture both sides as JSON in the `ccid_capture_diff.py`
format (host-tools, issue #143) — deterministic fields compared, random
fields declared and masked:

```bash
python3 host-tools/gp_probe.py --execute --reader "<oracle reader>" \
  --json-out oracle.json
python3 host-tools/ccid_capture_diff.py cherry.json oracle.json
```

CPLC data is deterministic: **any** byte difference at equal length —
or a length difference — isolates the corruption without touching the
ISD keys. Record the verdict either way (a clean diff is evidence for
the stage-3 escalation, not a nothing).

### 3.3 Stage 3 — the ONE authenticated attempt, fully tapped

Preconditions: stage 1+2 evidence banked; card is ledger-keyed (§1.2
ruling column); budget ruling recorded (one attempt). Then, in three
terminals, all under the bench flock:

```bash
flock /run/lock/bench-pcscd.lock systemctl stop pcscd pcscd.socket
LIBCCID_DEBUG_LEVEL=7 pcscd -f -d 2>&1 | tee .omo/evidence/gp-stage3-pcscd.log   # T1
DEFMT_LOG=debug probe-rs run --chip STM32F469NI target/.../ccid-firmware        # T2 (RTT)
gp -l -v -d --reader "<Cherry, resolved fresh>" \
   --key-enc 5A9E… --key-mac 7CCC… --key-dek B040…                              # T3 — ONE shot
flock /run/lock/bench-pcscd.lock systemctl start pcscd pcscd.socket
```

(The keyset is the org dev triple from the single-source chain §1.2 —
never gp's default key, §2.2.) **Diff coordinates on the INIT_UPDATE
R-APDU** (field map §2.3): KDD[0..10] — on the #132 card family expect
serial byte `27` at offset 6; keyver[10]=FF; scp[11]=02; seq[12..14];
challenge/cryptogram (random content — check LENGTH, 28 total);
SW=9000. Against the ACR capture of the same card.

**Discriminator — the T=1 activity AROUND the response** (the LRC
wall, `docs/t1_relay_132_rootcause.md` §0):

| Observation | Verdict |
|---|---|
| R-block / S(RESYNC) / retransmissions in the libccid trace before the auth failure | comm-class (H1/H2/H3 family) — the "mismatch" was a recovered-but-misassociated or paraphrased exchange |
| Byte diff in KDD/seq inside a block libccid ACCEPTED (valid LRC, no R-blocks) | LRC-invisible corruption — escalate: Escape 0xD1 coredump during a read-only repro, audit the USB driver path |
| Exact gp error string (§2.4) | name it in the report — three distinct evidence classes |
| `Rx timeout`/`frx` >500 ms stamps in RTT during INIT_UPDATE | H1 timing reproduction — receive-layer fix, no further auth needed |

### 3.4 Evidence filing

`.omo/evidence/<topic>/` per the durable-artifacts rule (AGENTS.md
§"Durable session artifacts"): pcscd log, RTT capture, the two capture
JSONs, the diff output, 0xD0 counter dumps, the exact gp command line +
version, and the CPLC identity of the card. Issue comment same-session;
budget ledger updated (§1.2) if an attempt was spent.

## 4. `host-tools/gp_probe.py` — the read-only probe skeleton

Implements ONLY the zero-risk set against a pcscd reader:
SELECT ISD with the full AID, GET DATA CPLC (`80 CA 9F 7F 00`), and an
opt-in single-shot keyless GET STATUS probe; plus offline decoders
(CPLC unwrap/field map, INIT_UPDATE response parser for captured hex)
and a JSON capture mode compatible with `ccid_capture_diff.py`.

Safety model (pinned by `tests/tools/test_gp_probe.py`):

- **Dry-run is the default** — without `--execute --reader <name>` the
  tool only prints/validates APDUs and decodes captures; it cannot
  touch hardware (pyscard imported only inside the execute path).
- **Fail-closed APDU allowlist** — every built APDU passes a guard that
  rejects SC-class and state-changing commands (INIT UPDATE 0x50,
  EXT AUTH 0x82, PUT KEY 0xD8, INSTALL 0xE6, LOAD 0xE8, STORE DATA
  0xE2, GET DATA outside the keyless allowlist); there is no code path
  that builds them.
- GET STATUS (`80 F2 …`, INS 0xF2) is a read but rides the SC on these
  cards (§1.1) — it is behind `--allow-get-status`, single-shot,
  refusal-tolerant, never retried.

```bash
python3 host-tools/gp_probe.py --dry-run                  # APDU table only
python3 host-tools/gp_probe.py --parse-cplc 9F7F2A4790…  # offline decode
python3 host-tools/gp_probe.py --parse-init-update <hex>  # offline decode
python3 host-tools/gp_probe.py --execute --reader "…" --json-out c.json
python3 -m pytest tests/tools/test_gp_probe.py -q          # offline gates
```

## References

- GlobalPlatformPro `next` @ `dd1a4c6` — all file:line citations above.
- GPPro wiki: Keys; Secure Channel Establishment (read 2026-10-29).
- GP Card Spec clause numbers as printed inline by GPPro: chaining
  "GPC 2.3.1 11.1.5.1" (`GPSession.java:542-543`), GET STATUS errors
  "GPC 2.3.1 Table 11-39" (`GPSession.java:1072`), SELECT warning
  Table 11-83 (`GPData.java:41`), CPLC v2.3 §11.9 / Table 11-86
  (locked in-repo, `j3r180_gpg_lifecycle.py:149-155`).
- Bench ledger: issues #127 #132 #144 #155, `docs/HIL-VERIFICATION-PROTOCOL.md`
  §1, `tests/hardware/nfc/J3R180-RUNBOOK.md`,
  `docs/t1_relay_132_rootcause.md` (+ its preserved copy on #132),
  `host-tools/ccid_capture_diff.py` (#143 capture corpus).
