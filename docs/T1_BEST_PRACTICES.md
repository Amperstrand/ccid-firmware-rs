# ISO 7816-3 T=1 in CCID Readers — Best-Practices Reference & Gap Analysis

Status: research deliverable (2026-10-10). No hardware was touched; no commits made.
Audience: anyone changing `firmware/esp32-ccid/src/t1.rs`, the esp32 CCID handler,
or the STM32 T=1 path (`firmware/ccid-firmware/`).

Sources used (all read in full or in the cited region):

| Source | Path / ref | What it is |
|---|---|---|
| Our esp32 T=1 endpoint | `firmware/esp32-ccid/src/t1.rs` | Firmware-resident T=1 responder (card role) for the libccidtwin wire |
| Our esp32 handler | `firmware/esp32-ccid/src/ccid_handler.rs` | XfrBlock routing, T=1 arming, relay pipeline |
| Our esp32 ATR synth | `firmware/esp32-ccid/src/mfrc522_driver.rs` | `build_pcsc_atr_from_historical()` |
| Our STM32 T=1 engine (dead code) | `firmware/ccid-firmware/src/t1_engine.rs` | Full initiator engine, currently uncalled |
| Our STM32 drivers | `firmware/ccid-firmware/src/smartcard.rs`, `smartcard_bitbang.rs`, `smartcard_common.rs` | USART / bitbang frontends, IFSD negotiation, raw pipe |
| Our STM32 CCID core | `firmware/ccid-firmware/src/ccid_core.rs`, `device_profile.rs` | XfrBlock, SetParameters T=1, dwFeatures |
| osmo-ccid-firmware | `reference/osmo-ccid-firmware/` @ `639db14` | Reference CCID implementation (see §5 — **T=0 only**) |
| openct proto-t1.c | `LudovicRousseau/CCID` `src/openct/proto-t1.c` (master, fetched 2026-10-10); same lineage as `OpenSC/openct` `src/ifd/proto-t1.c` | **The T=1 engine libccidtwin actually runs** on the host |
| ccid ifdhandler.c | `LudovicRousseau/CCID` `src/ifdhandler.c` (~line 1224) | Where the host learns IFSC (ATR TA3) and when it runs `t1_negotiate_ifsd` |
| ST USB CCID middleware | `STMicroelectronics/stm32-mw-usb-device` `Class/CCID/Inc/usbd_ccid_smartcard_template.h` | Independent bit-layout cross-check |
| ISO/IEC 7816-3 | :1997 + :2006 editions, clause 11 | The spec itself (paywalled; clause numbers below are cross-verified against openct's in-code rule citations and ST's template) |

> **Spec-citation caveat.** Sub-clause numbers of ISO 7816-3 clause 11 shifted between
> the 1997 and 2006 editions. openct's code cites the 1997 *Rules* (6, 6.2, 6.3, 6.4,
> 7.2, 7.4.2, 9) which the 2006 edition restructures into prose under §11.5/§11.6.
> This document cites the section numbers used consistently by our own `t1.rs`
> study comments and by ST's template (§11.3 block structure, §11.4.2 PCB,
> §11.5.3 recovery rules, §11.6.2 IFS/WTX, §11.6.3 chaining, §11.6.4 waiting
> times) and names the 1997 Rules explicitly where they matter. Where a byte-level
> fact is asserted, it is triangulated across at least two independent
> implementations (openct + ST + our code), not from memory of the PDF.

---

## 1. Who terminates T=1 — the three architectures

A CCID reader can sit at any of three exchange levels (CCID 1.1 §5.1 / dwFeatures
bits 16..18), and the T=1 responsibility follows the level:

| Model | CCID exchange level | Who frames T=1 | Examples |
|---|---|---|---|
| A. Host-driven, character level | 0x0000_0000 | Host builds every block; reader relays bytes | osmo-ccid-firmware (`dwFeatures` low byte 0x00 → character level, `ccid_host/ccid_main_functionfs.c:98`, `sysmoOCTSIM/usb_descriptors.c:184`) |
| B. Host-driven, TPDU level | 0x0001_0000 | Host's T=1 engine (libccid `t1.c`/openct `proto-t1.c`) sends whole T=1 blocks as XfrBlock payloads; reader relays blocks | **Our STM32 firmware** (all three profiles, `device_profile.rs:410,501,541` `FEAT_LEVEL_TPDU`); GemPC Twin (the reader the esp32 emulates) |
| C. Reader terminates T=1, Short/Extended APDU | 0x0002/0x0004_0000 | Reader runs T=1 toward the card; host sees plain APDUs | Typical USB CCID readers; **our esp32 firmware is a hybrid**: it *advertises* TPDU level like a GemPC Twin, but — because the "card" is an NFC ISO-DEP device that cannot speak T=1 on that wire — it **terminates the host's T=1 locally** in `t1.rs` and relays bare APDUs to the NFC pipeline |

The esp32 hybrid (model B on the wire, model C internally) is the interesting one:
`T1Endpoint` (`t1.rs:133`) plays the **card role** in T=1 toward libccidtwin. Every
design decision below must be read in that frame: *we are a T=1 card as far as the
host can tell.*

## 2. ISO 7816-3 §11 quick reference (verified tables)

### 2.1 Block structure (§11.3)

```
NAD(1) | PCB(1) | LEN(1) | INF(LEN) | EDC(1)
```
- LEN is INF length; 0x00 legal (R/S blocks), 0xFF **illegal** (openct rejects it on receive).
- EDC: **LRC** = XOR of all preceding bytes (default). CRC-16 exists as an option
  (negotiated via ATR/bmTCCKST1 EDC bit); neither osmo nor our firmware implements
  CRC — libccidtwin hardcodes LRC (`t1_init` → `IFD_PROTOCOL_T1_CHECKSUM_LRC`).
- NAD: VPP/SAD/DAD bits. Everything in practice uses NAD = 0x00; openct validates
  `NAD == swap_nibbles(dad)` (i.e. 0 for the card address) and errors otherwise.
  CCID SetParameters `bNadValue` "shall be 0" — our STM32 validates this
  (`ccid_core.rs:623`).

### 2.2 PCB encodings (§11.4.2)

```
I-block: 0 N(S) M 0 N(R) 0 0 0        = 0x00 | ns<<6 | M<<5 | nr<<4
R-block: 1 0 N(R) 0 err               = 0x80 | nr<<4 | err      (err: 00=OK/next, 01=EDC error, 10=other error)
S-block: 1 1 0 R 0 0 0 type           = 0xC0 | resp<<5 | type   (R=1 marks the response)
         type: 00=RESYNC 01=IFS 10=ABORT 11=WTX
```

**The load-bearing detail: R-block N(R) lives at bit 4 (value 0x10), not bit 3.**
Confirmed by:

- openct/ccid `proto-t1.c`: `#define T1_R_SEQ_SHIFT 4` and `t1_build()`:
  `pcb |= t1->nr << T1_R_SEQ_SHIFT`.
- ST's CCID middleware template: `T1_R_SEQ_SHIFT 0x10 /* N(R) position (b5) */`.
- I-block N(R) at bit 4 (value 0x10). Notably **openct never transmits an I-block
  N(R)** (`t1_build` only ORs `ns<<6` and the M-bit) — its own wire bytes are
  zero in bits 4..0 — and it never *checks* a received I-block's N(R). But the
  bit position is still the spec answer for conforming peers.

So: `R(N(R)=1)` = **0x90**, `R(1, EDC)` = **0x91**, `R(1, other)` = **0x92**.
`S(WTX request)` = 0xC3, `S(WTX response)` = 0xE3, `S(IFS request)` = 0xC1 /
response 0xE1, `S(RESYNC req/resp)` = 0xC0/0xE0, `S(ABORT req/resp)` = 0xC2/0xE2.

### 2.3 S-block INF rules (§11.6.2 and openct's receive validation)

| S-block | INF length | Legal INF range | Notes |
|---|---|---|---|
| RESYNC | 0 | — | Only the **initiator** may send it; openct treats a RESYNC *from* the card as protocol failure → host initiates its own resync |
| IFS | 1 | 0x01..0xFE (0x00 and 0xFF illegal) | Sender announces **its own receive size**; receiver must answer with S(IFS response) echoing the value |
| ABORT | 0 | — | openct answers and aborts the chain (Rule 9) |
| WTX | 1 | m = 1..59 | Sender asks for m×BWT; receiver grants S(WTX response m') with m' ≤ m |

### 2.4 Waiting times (§11.6.4)

- **BWT** (block waiting time, receiver's patience for the first byte of a block):
  `BWT = 11 etu + 2^BWI × 960 etu(work)`. BWI from ATR TB3 high nibble (default 0).
- **CWT** (character waiting time, inter-byte within a block):
  `CWT = (2^CWI + 11) etu`. CWI from TB3 low nibble (default 0 → 12 etu!). CWT is
  *smaller* than BWT — a slow-but-alive card between bytes is a WTX case, not a
  CWT violation.
- **BGT** (block guard time): ≥ 22 etu between the last byte received and the first
  byte sent in the other direction.
- ATR TB3 absent → IFSC = 32, BWI = 0, CWI = 0 (all defaults). IFSC/IFSD range
  01..FE with practical max **254**.

### 2.5 Error-recovery ladder (§11.5.3; 1997 Rules as cited by openct)

1. Bad block (LRC/parity/LEN/NAD) → send **R(N(R), EDC error)** asking for
   retransmission (Rule 7). If *your own* last block was an R-block, resend it
   unchanged (Rule 7.2).
2. After **3 failed retries** (Rule 7.4.2) → initiator sends **S(RESYNC request)**,
   resets both N(S)/N(R) to 0, expects S(RESYNC response) (Rules 6, 6.2, 6.3).
3. After **3 failed resyncs** (Rule 6.4) → declare the card dead (openct: state
   `DEAD`, `t1_transceive` returns error; every subsequent call fails until reset).
4. Chaining ACK semantics: the receiver of a chained I-block answers **R(N(R))**
   where N(R) = the N(S) of the chain in progress; N(S) toggles only after the
   final block (M=0) of a chain (§11.6.3).

---

## 3. What the host actually runs: libccidtwin = ccid's openct proto-t1.c

`pcscd` + `libccidtwin.so` (the driver our esp32 firmware faces) builds the
`src/openct/proto-t1.c` engine from the ccid driver source. Verified behaviors
that our firmware MUST satisfy (function names from the fetched master copy;
our `t1.rs:118-120` corroborates line-level details from the bench strace):

| Host behavior | Where | Consequence for our endpoint |
|---|---|---|
| Opens T=1 with `S(IFS request FE)` via `t1_negotiate_ifsd()` when the reader does **not** advertise `AUTO_IFSD` | `ifdhandler.c` ~1224 area → `t1_negotiate_ifsd()` | We must answer a byte-exact `00 E1 01 FE LRC`. We do (`t1.rs:257-264`, bench-verified) |
| Response validation is strict: PCB must equal `S|response|IFS`, LEN=1, INF must equal the requested IFSD, exact frame length `4 + rc_bytes`, checksum OK; 3 tries then DEAD | `t1_negotiate_ifsd()` | Any deviation kills the reader. Our reply passes |
| Host's own send chunk size `t1->ifsc` comes from **ATR TA3** (`get_IFSC()`), else stays **32** | `ifdhandler.c:~1224` | Our synthetic ATR has no TA3 → **host chains every APDU > 32 bytes across many I-blocks** |
| I-blocks sent by the host carry `ns<<6 | M<<5` **only** (no N(R) bits) | `t1_build()` | Never expect N(R) from the host; our N(R) position is never checked by this host — but see conformance gap G-02 |
| Received R-block checks: LEN must be 0, PCB bit 0x20 must be clear, `t1_seq(pcb) == t1->ns` unless mid-chain (more) | `t1_transceive()` T1_R_BLOCK case | **Our R-block bit layout was on the critical path** — see G-01 (**FIXED**: 7f136a0) |
| Received I-block checks: only `t1_seq(pcb) == t1->nr`; on chain M=1 → send R(N(R)); on M=0 → done | T1_I_BLOCK case | Our ns toggling must be exactly per §11.6.3 (it is) |
| WTX: on S(WTX request m) → set `t1->wtx = m`, reply S(WTX response m); next `t1_xcv` multiplies the read timeout by m | T1_S_WTX case + `t1_xcv()` | Our fixed m=2 doubles the host patience exactly once — legal but see G-04 |
| IFS *request from the card*: sets `t1->ifsc = INF`, replies S(IFS response); INF 0x00/0xFF → resync | T1_S_IFS case | **The sanctioned way to stop the host chaining at 32**: we may announce 254 |
| RESYNC request from card: forbidden → host initiates its own resync | T1_S_RESYNC case | We must never send S(RESYNC request) (we don't) |
| Retry ladder: `retries = 3` per step (Rule 7.4.2), `resyncs = 3` per transceive (Rule 6.4), then DEAD | `t1_transceive()` | A session survives ≤ 3 resyncs per APDU; systematic R-block errors kill it — this is the wedge mechanism in G-01 |
| NAD must equal `swap_nibbles(dad)` (= 0x00 for us); LEN 0xFF illegal | top of `t1_transceive()` | We always send NAD=0 ✓ |

**Bench cross-reference (AGENTS.md, issue #101):** pcscd connect + SELECT/APDUs
work; the known caveat is that heavy/fuzz traffic "wedges up to 3/27 with >5 s
recovery" and the m5stick SmartPGP install stalls on the ~470-block CAP upload.
Section 6 shows a protocol-level mechanism (G-01) that reproduces exactly this
signature; it is testable without new hardware.

## 4. What osmo-ccid-firmware actually implements (spoiler: no T=1)

The task premise ("their T=1/TPDU-to-APDU translation, chaining, WTX, IFSD
negotiation, R-block recovery") does not hold at the pinned submodule revision
`639db14` — **osmo-ccid-firmware has no T=1 engine at all**. The evidence:

- `ccid_common/ccid_device.c:494,509` — GetParameters / ResetParameters: `/* FIXME: T=1 */`, always returns T=0 parameter structures.
- `ccid_common/ccid_slot_fsm.c:83` — default parameters: `/* FIXME: T=1 */`.
- `ccid_common/ccid_slot_fsm.c:417` — `set_params`: `if (proto != CCID_PROTOCOL_NUM_T0) return -7;` → **SetParameters(T=1) is rejected**.
- Descriptors: `dwMaxIFSD = 0` (`ccid_host/ccid_main_functionfs.c:95`, `sysmoOCTSIM/usb_descriptors.c:175`), character-level exchange (`dwFeatures` low byte 0x00).
- `iso7816_fsm.c` (1743 lines) is the byte-level **T=0** engine: ATR FSM
  (TS/T0/TA..TD/TCK, convention detection), PPS FSM, and a TPDU FSM built around
  T=0 procedure bytes. No I/R/S block appears anywhere.
- Their XfrBlock path (`ccid_slot_fsm.c:346-378`) is TPDU T=0 only:
  `wLevelParameter` must be 0, dwLength ≤ 260 (CCID 6.1.4 note), dispatches one
  TPDU into the FSM; responses come back as APDU-level `ccid_gen_data_block`.

What osmo *is* worth copying (patterns, not T=1 mechanics):

| Pattern | osmo location | Relevance to us |
|---|---|---|
| Event-driven ISO FSM (never block the serving loop) | `iso7816_fsm.c` states | Our esp32 endpoint is synchronous-by-reply; fine for a responder, but the PN7160 re-arm work (AGENTS #107) shows the cost of blocking calls in the poll path |
| Watch-time expiry → **deactivate + ICC_MUTE** error mapping | `ccid_slot_fsm.c:215-227` (`ISO7816_E_WTIME_EXP` → `CCID_ERR_ICC_MUTE` + power down) | A T=1 session that exhausts the resync ladder should surface as ICC_MUTE, not a generic error |
| `proposed_pars` — commit parameters only after the card confirms (PPS/IFS) | `ccid_slot_fsm.c:279-333` | Matches CCID SetParameters semantics; our esp32 handler accepts-and-forgets (G-04/G-09) |
| ATR-derived timing tables + WT calculation in one place | `iso7816_3.h:58-98` (`iso7816_3_calculate_wt`, Fi/fmax/Di tables) | Our STM32 `atr_params` (bwi/cwi/ifsc) exists; BWT now drives the first-byte wait (46c4701), CWT still unused (G-07 residual) |
| WTIME expiry resets the slot FSM, not just the transfer | `iso7816_fsm.c:473-486` | Post-wedge recovery behavior |

**Bottom line for the comparison table:** on T=1, *our repository already exceeds
osmo-ccid-firmware* — we ship two T=1 implementations (one live responder, one
dead initiator engine) where osmo ships none. The useful osmo content is the
error-mapping and FSM discipline, and their explicit TODO is a reminder that a
TPDU-level CCID stack can defer all of T=1 to the host — exactly what our STM32
build does and our esp32 build cannot (NFC card can't frame T=1).

## 5. Our firmware, mechanism by mechanism

### 5.1 esp32 `t1.rs` — host-facing T=1 responder

| Mechanism | Spec / openct expectation | Our behavior (`t1.rs` refs) | Verdict |
|---|---|---|---|
| Block parse + LRC | §11.3; LEN ≤ 254; LRC XOR | Full prologue parse, LEN ≤ MAX_IFSC (254), LRC verified before any state change (`feed`, 178-190) | ✅ |
| I-block N(S) tracking | §11.6.3: expect host_ns; mismatch → R(N(R)) request retransmit | `ns != host_ns` → `R(host_ns, 0)` (221-224) | ✅ logic; ✅ PCB bits fixed (7f136a0) |
| Host chaining (receive) | ACK each M=1 piece with R(N(R)=same ns); assemble; N(R) toggles host_ns only on final block | Exactly that (228-238); assembly ≤ MAX_RELAY=512 | ✅ semantics; ✅ PCB bits fixed (7f136a0); downstream truncation FIXED (G-03, 2026-10-12) |
| Our chaining (send) | Chunk at receiver's IFS; M=1 until last; ns toggles after final block | `take_next_chain_block` (326-344): chunks at `send_ifsc`, toggles ns on final | ✅ |
| IFS negotiation (host→us) | Answer S(IFS response) echo; INF 01..FE | Echo reply, `send_ifsc = min(inf,254)` (257-264); byte-exact `00 E1 01 FE 1E` bench-verified | ✅; ❌ no INF range check (G-09) |
| IFS announcement (us→host) | Card-role may send S(IFS request 254) to raise the host's send size; host supports it | **Never sent** — host keeps its ATR-derived ifsc=32 and chains | Gap G-05 (with the ATR fix) |
| WTX (us→host) | m ∈ 1..59, granted before the block; host multiplies its read timeout by m | `card_response(took_ms>150)` → S(WTX request **2**), I-block deferred until the grant (307-321, 283-290) | ✅ mechanism; ❌ fixed m=2 and 150 ms threshold detached from BWT (G-04) |
| WTX (host→us) | Responder echoes response | Echoed (271-277) | ✅ |
| R-block (host ACK of our chain) | R(N(R)=next expected) → send next chain block | Any R: `take_next_chain_block()` first, else retransmit last (200-218) | ❌ error-carrying R should retransmit the *same* block (G-06) |
| Sequence violation by host | R(N(R)) request retransmit | `R(host_ns,0)` (221-224) | ✅ logic; ✅ bits fixed (7f136a0) |
| Malformed block (LEN/LRC/oversize) | R(N(R), EDC error) | `bad_block` → `R(host_ns, 1)` (346-349) | ✅ logic; ✅ bits fixed (7f136a0) |
| RESYNC request | Reset N(S)/N(R), clear chains, reply S(RESYNC response) | Full state reset incl. `wtx_pending` (246-256) | ✅ |
| ABORT request | Clear current chain, keep sequence numbers, reply S(ABORT response) | Clears recv_chain only (265-270) | ✅ (echoes INF; host never sends INF≠0 — cosmetic) |
| Unsolicited S response | Retransmit last (interop-safe) | Does (291-299) | ✅ pragmatic |
| S-blocks without active card | Reader's own link-layer duty | Answered while `t1_armed && protocol==1` even with no card (`ccid_handler.rs:278-291`, test `s_block_answered_without_active_card`) | ✅ (issue #101 design note) |
| I-block N(R) piggyback we send | N(R) at bit 4 per spec | `i_pcb` puts nr at bit 4 (7f136a0); host ignores it | ✅ fixed (G-02) |
| Arming/lifecycle | T=1 route only after host selects T=1 | Armed on SetParameters/ResetParameters (ccid_handler.rs:404-415); disarmed on card reset (191); PPS-before-SetParameters regression covered by test (1204-1230) | ✅ |

### 5.2 esp32 handler integration

| Aspect | Behavior | Ref | Verdict |
|---|---|---|---|
| XfrBlock routing | S-request → local answer (even no card); I/R → card-gated; armed only when protocol==1 | `ccid_handler.rs:278-357` | ✅ |
| APDU relay into pipeline | Assembled APDU copied into `[u8; t1::MAX_RELAY]` (was `[u8; 254]` with silent clamp — FIXED 2026-10-12) | `ccid_handler.rs` `xfr_via_t1` | ✅ G-03 resolved |
| Response re-framing | Pipeline DataBlock payload (CCID header stripped) staged into `send_chain`, chunked at `send_ifsc` (≤254) | 333-354, `t1.rs:307-321` | ✅ (512-byte cap on card response, fine for short-APDU NFC) |
| SetParameters T=1 payload | **Not parsed** — arms the route, replies fixed `DEFAULT_T1_PARAMS = [11 00 00 03 00 20 00]` (IFSC=32, BWI=0/CWI=3) regardless of host request | 404-408, 593-613; `ccid_types.rs` (test at 201-208) | ❌ G-04/G-09 |
| Synthetic ATR | `3B 80x 80 01 <hist> TCK` — **no TD (T=0 implied!), no TA3/TB3**; TCK appended even though TCK is only legal when a T≠0 is offered | `mfrc522_driver.rs:281-304` | ❌ G-05 (host: ifsc=32, default BWT, no T=1 preference) |

### 5.3 STM32 path (F469 USART + F746 bitbang)

The architecture today is **model B: transparent TPDU pipe** — decided in commit
`b1e685a` ("Replace T=1 block engine with raw byte forwarding: firmware is a dumb
pipe, host/libccid owns all T=1 framing") after the real T=1 engine was found to
fail against GlobalPlatform traffic at PPS-negotiated baud.

| Aspect | Behavior | Ref | Verdict |
|---|---|---|---|
| dwFeatures | TPDU level (0x0001_0000) on all profiles; AUTO_IFSD deliberately **not** set (device_profile.rs:610-611 assert) so libccid runs its own IFS/WTX dance through the pipe | `device_profile.rs:404-410, 501, 541` | ✅ coherent with the pipe |
| XfrBlock | ≤261 bytes, Short-APDU-shaped payloads passed to `driver.transmit_apdu` | `ccid_core.rs:517-577` | ✅ |
| T=1 data path | `transmit_apdu()` protocol==1 → **`transmit_raw`**: byte pipe both directions; response side uses T=1 prologue (LEN) to know when the card's block ends | `smartcard.rs:619-620, 631-698`; bitbang `smartcard_bitbang.rs:641-642, 676-703` | ✅ for design B |
| `t1_engine.rs` | Complete initiator engine — chaining, R-block recovery, WTX grant, RESYNC handling, BGT — with **zero callers**; compiled under `#![allow(dead_code)]` | `t1_engine.rs:1-2, 129-246` | ⚠️ G-08 (decide keep-or-delete) |
| Activation-time IFS | `power_on` runs `do_ifs_negotiation_t1`: sends S(IFS request **254**) (correct: announces *our* receive size / IFSD) with a PPS-baud fallback ladder | `smartcard.rs:412-484`, `smartcard_common.rs:287-320` | ✅ per §11.6.2; the response INF is the echo of our 254, stored as `self.ifsc` ("card IFSC") — semantically off but unused by the pipe (see G-08 note) |
| ATR handling | TCK verified for T=1 (§8.2.4), protocol from TD1, PPS via FSM, T=1 line params: 1 stop bit, GT=1, NACK off | `smartcard.rs:394-429`, `smartcard_common.rs:273-285` | ✅ |
| SetParameters T=1 | Full CCID §6.1.7 validation: bmTCCKST1 only-EDC-bit, bIFSC 16..=254, bNadValue 0; applies BWI/CWI/EDC/IFSC into `atr_params` | `ccid_core.rs:593-666` | ✅ validation; ❌ applied values unused (G-07) |
| Waiting times | **First-byte FIXED (46c4701)**: BWT-derived (`t1_first_byte_timeout_ms`, floored 500 ms / capped 5 s); inter-byte still fixed 50 ms (USART), 200 ms floor (bitbang), 9 s procedure timeout (`b1e685a`) | `smartcard.rs:672-698, 757-764`, `smartcard_bitbang.rs:725-735` | ✅ first-byte; ❌ inter-byte vs §11.6.4 (G-07 residual); benign for the pipe only because the host's own BWT governs the session |
| CCID Abort / time extension | Abort control request accepted, no async abort; no BWT time-extension signaling | `ccid.rs:306-310`, README audit | ⚠️ acceptable for synchronous single-slot pipe |
| Dead-engine defects (if ever revived) | (a) R-ACK built as `nr<<4` — **correct**, better than the esp32 endpoint; (b) WTX granted but its own receive timeout stays 5000 ms (no m×BWT extension); (c) after MAX_RETRANSMIT it errors out instead of escalating to S(RESYNC request) per Rules 6/7; (d) no card-initiated S(IFS) handling; (e) I-blocks carry no N(R) | `t1_engine.rs:147, 153-157, 165-188, 201, 213, 222-243` | ⚠️ G-08 |

---

## 6. Gap list (severity-ranked)

Severity = combination of protocol correctness, likelihood on the bench, and blast
radius. "esp32" gaps concern `t1.rs`/handler; "STM32" gaps concern the pipe path.

| # | Sev | Where | Gap | Evidence | Fix sketch |
|---|---|---|---|---|---|
| **G-01** | **HIGH** | esp32 `t1.rs:107-109` (`r_pcb`), used at 223, 234, 348 | **RESOLVED 2026-10-10 (7f136a0)** — Table-45 layout landed (`r_pcb = 0x80\|(nr<<4)\|err`), bench-verified byte-exact; byte-literal lock test `pcb_bit_layout_locked_to_iso_7816_3_table_45` added 2026-10-12. Original finding: R-block N(R) encoded at bit 3, spec says bit 4. `r_pcb(1,0)` emitted `0x88` instead of `0x90`: reserved bit b4 is set and a conforming parser decodes N(R)=0. openct (libccidtwin) reads `(pcb>>4)&1`, requires bit 0x20 clear and LEN=0. Consequence: every R-block we send with N(R)=1 — i.e. our ACK of any **odd-N(S) host chain** — is read as "wrong sequence", the host answers R(other-error), we re-send the same broken R (feed's retransmit path), retries exhaust (3), host resyncs; 3 resyncs per transceive → **DEAD**. Host chains whenever APDU > its ifsc (=32, see G-05) and N(S) alternates per chain, so ~every second chained APDU wedges with multi-second recovery. | `proto-t1.c` `T1_R_SEQ_SHIFT 4`; ST template; failure signature matches issue #101's "wedges up to 3/27, >5 s recovery" and the m5stick CAP-upload stall | `fn r_pcb(nr,err) -> 0x80 | (nr<<4) | err`; add a test asserting our R(1) decodes back via `parse_pcb` and equals 0x90/0x91 |
| **G-02** | MED | esp32 `t1.rs:103-105` | **RESOLVED 2026-10-10 (7f136a0)** — `i_pcb` now `(ns<<6)\|(more<<5)\|(nr<<4)`, locked by the byte-literal PCB test. Original finding: I-block N(R) sent at bit 0 (spec: bit 4). Harmless with openct (never parsed) but nonconforming for any stricter host (Windows WUDF, other stacks). | §11.4.2; openct sends zero bits itself | `i_pcb`: `(ns<<6)|(m<<5)|(nr<<4)`; tests at 381/392/436/443/450 need the expected-PCB constants updated |
| **G-03** | **MED-HIGH** | esp32 `ccid_handler.rs:330-332` | **RESOLVED 2026-10-12** — the copy buffer is now `[u8; t1::MAX_RELAY]`, derived from the assembler's own capacity (silent truncation impossible by construction); overflow at the assembler already answers R(other-error); regression test `chained_apdu_over_254_reaches_card_untruncated`. Original finding: Chained-APDU truncation at 254 — `t1.rs` assembles up to MAX_RELAY=512 from host chains, then the handler copied into `[u8;254]` before the pipeline — bytes 255.. silently dropped, the card sees a mangled APDU, the T=1 layer reports success. Any case-3/4 APDU >254 B (GP STORE DATA during CAP install) is corrupted. | assembly cap `t1.rs:228-230` vs copy cap | Size the relay buffer to 261/512; assert assembled length ≤ buffer or answer R(other-error) |
| **G-04** | MED | esp32 `t1.rs:313-318`, `ccid_handler.rs:404-408` | **CODE LANDED 2026-10-12 (bench confirmation pending)** — SetParameters T=1 is parsed and applied (shared core `applied_t1_timing` → endpoint `set_host_timing`); WTX m = ceil((elapsed+100 ms)/BWT) clamped 2..59 with BWT from the negotiated/advertised BWI (shared `t1_bwt_ms`); GetParameters echoes the accepted set. Original finding: WTX policy detached from BWT — hard threshold 150 ms and fixed m=2; if the host's BWT times out before m×BWT covers the card operation (on-card RSA runs seconds), the host aborts mid-WTX. SetParameters T=1 payload was ignored entirely. | §11.6.2/§11.6.4; `t1_xcv` multiplies timeout once per granted WTX | Parse SetParameters T=1 (bmWaitingIntegers) + compute BWT; choose m = ceil(elapsed/BWT)+1, clamp 1..59; reply with the accepted parameters |
| **G-05** | MED | esp32 `mfrc522_driver.rs:281-304` | **CODE LANDED 2026-10-12 (bench confirmation pending)** — the m5stick/mfrc522 synthetic ATR is now `3B (80\|K) 81 31 FE 45 <hist> TCK` (TA3=IFSC 254, TB3=BWI 4/CWI 5, legal TCK), semantic-locked through the shared `parse_atr`. Nucula sibling path deliberately unchanged: its ATS-derived ATR mirrors the ACR reference bytes (d6b3877) — its own bench pass required. Original finding: synthetic ATR declared no T=1 — no TD (T=0 implied — yet a TCK was appended), no TA3 (IFSC) / TB3 (BWI/CWI); host used ifsc=32 (8× the chain round-trips on a slow UART/I2C relay), default BWT, and could not auto-prefer T=1. | `ifdhandler.c` get_IFSC; defaults §11.6.2/§11.6.4 | Emit a T=1-declaring ATR: `TS, T0(K=0), TD1=0x81, TD2=0xB1, TA3=0xFE, TB3=<bench-tuned BWI/CWI>, TCK` (TCK now legal — a T≠0 is offered). Alternative/simpler: card-role S(IFS request 254) after answering the host's IFS request — openct honors it (T1_S_IFS case) |
| **G-06** | MED | esp32 `t1.rs:211-217` | **Error-carrying R answered with the NEXT chain block.** On host R(EDC/other) — "retransmit block N" — the endpoint prefers `take_next_chain_block()` and only retransmits the last block when the chain is empty. Spec §11.5.3: an error R demands the same block again; sending N+1 loses INF data. | §11.5.3 / Rule 7 | Branch on `err != 0` (and N(R) mismatch) → retransmit `last_sent`; only clean R with expected N(R) advances the chain |
| **G-07** | MED | STM32 `ccid_core.rs:649-654` vs `smartcard.rs:662-672` / `smartcard_bitbang.rs:725-735` | **PARTIALLY RESOLVED 2026-10-12 (46c4701)** — first-byte wait now derives from negotiated BWT (`t1_bwt_ms` in `smartcard_common.rs`, host-tested; `t1_first_byte_timeout_ms`, floored 500 ms / capped 5 s). Still open: inter-byte is flat 50 ms (no CWT derivation) and T=0 WWT is flat. Original finding: Negotiated BWI/CWI accepted but never enforced — the pipe ran fixed 500/50/200 ms + 9 s procedure timeout. | §11.6.4; `b1e685a` commit message | Either document "timers are host-owned at TPDU level" at the SetParameters site, or derive first-byte/inter-byte timeouts from `atr_params.bwi/cwi` |
| **G-08** | LOW-MED | STM32 `t1_engine.rs` | **Dead-but-shipped engine with known defects** (no WTX-induced BWT extension at 147/201; no resync escalation after MAX_RETRANSMIT at 153-157; no card S(IFS) handling; no I-block N(R)). Its R-block bit layout (`nr<<4`, line 213) is *correct* — note the irony vs G-01. If anyone revives it for an APDU-level profile, these bite. | callers: none (grep); `#![allow(dead_code)]` | Decide: delete, or fix + wire behind the Short-APDU profile work. Do not leave silently |
| **G-09** | LOW | esp32 `t1.rs:257-264` | **IFS INF not range-checked** (spec 01..FE; 0 → `send_ifsc=0` → zero-length I-block livelock; 0xFF illegal). Also ABORT request INF echoed rather than enforcing LEN=0. Defensive only — openct never sends these. | §11.6.2 | `if !(1..=254).contains(&new_ifs) { return self.bad_block() }` |
| **G-10** | LOW | esp32 `t1.rs:219` | Host I-block N(R) piggyback ignored (`nr: _`). Acceptable for openct (it never sends one); a conforming initiator would expect mis-ACKed blocks to be retransmitted. | §11.6.3 | Optional: validate `nr == ns` on final blocks, answer R(other) on mismatch |
| **G-11** | LOW | both | CCID Abort (bulk ABORT 0x72 + S(ABORT request) chain) not integrated: esp32 has no 0x72 path; STM32 accepts and ignores. Mid-chain host aborts rely on the host-side engine only. | CCID §6.1.13; §11 ABORT rule 9 | Only relevant if async serving lands (post-#90 consolidation) |
| **G-12** | INFO | osmo | osmo-ccid-firmware has no T=1 at all (see §4). Our repo exceeds the reference here; keep the comparison honest in any audit doc. | §4 file:line evidence | — |

### Why G-01 is the one to fix first (bench-predicted)

Trace of a host chain with N(S)=1 (host sending a >32 B APDU on its second-or-later
chain):

1. Host sends I(ns=1, M=1 …). Our ACK is `r_pcb(1,0)` = **0x88**.
2. openct: R-block, LEN=0 ok, 0x20-bit clear ok, `t1_seq(0x88) = (0x88>>4)&1 = 0 ≠ t1->ns = 1`, not mid-chain → "wrong nr" path → host sends R(other-error), expects retransmission.
3. We receive an R-block → `take_next_chain_block()` (none) → re-send `last_sent` = the same **0x88**.
4. Loop until retries=0 → host resync (S(RESYNC)) — we handle that correctly (state reset) — chain restarts at ns=0, our ACK `r_pcb(0,0)`=0x80 is *legal*, chain completes.
5. Next odd-ns chain repeats step 1-4; after 3 resyncs in one transceive → DEAD → pcscd "card unresponsive" until reconnect.

Every observable in this trace matches the recorded bench behavior class (wedge,
~3 occurrences per battery run, >5 s recovery, pcscd unaffected on small APDUs —
small APDUs never chain and thus never elicit our R-blocks).

## 7. Top 5 recommendations

1. **Fix the R-PCB bit layout in `t1.rs`** (`0x80 | nr<<4 | err`) and pin it with a
   round-trip test (`r_pcb(n,e)` must survive `parse_pcb` and match openct's
   decode). One-line change, removes the wedge class G-01. Update the existing
   tests that encode the wrong constant (`t1.rs:424, 440` etc.).
2. **Stop truncating chained APDUs at 254** in `ccid_handler.rs:330` — size the
   relay to the 512-byte assembly (261 suffices for short-APDU) and error loudly
   on overflow instead of corrupting the APDU (G-03).
3. **Fix R-block error semantics**: retransmit `last_sent` on R with `err != 0` or
   unexpected N(R); advance the chain only on a clean R(N(R)) (G-06). Small
   branch in `feed()`, one new unit test.
4. **Declare T=1 properly to the host**: put TD1=0x81 + TA3=0xFE + TB3 in the
   synthetic ATR (and/or send a card-role S(IFS request 254) right after the
   host's IFS negotiation — openct honors it). This cuts host chaining ~8× on the
   slow MFRC522 relay, gives the host a real BWT, and removes the TCK
   inconsistency (G-05).
5. **Derive WTX policy from negotiated BWT**: parse the SetParameters T=1 payload
   (bmWaitingIntegers) instead of ignoring it; pick m = ceil(elapsed/BWT)+1
   (≤59) so on-card RSA survives; echo accepted parameters in GetParameters
   (G-04). On the STM32 side, either delete `t1_engine.rs` or fix its four known
   defects and wire it to the Short-APDU profile — but don't let it rot silently
   (G-08).

## 8. References

- ISO/IEC 7816-3:1997 / :2006, clause 11 (T=1). Paywalled; clause skeleton
  cross-verified against openct's in-code rule citations (Rules 6, 6.2, 6.3, 6.4,
  7.2, 7.4.2, 9) and ST's `usbd_ccid_smartcard_template.h`.
- openct T=1 engine (the binary behavior of libccidtwin):
  `https://github.com/LudovicRousseau/CCID/blob/master/src/openct/proto-t1.c`
  (fetched 2026-10-10; original `https://github.com/OpenSC/openct/blob/master/src/ifd/proto-t1.c`,
  Olaf Kirch 2003, improvements Ludovic Rousseau 2004-2024).
- ccid driver host-side T=1 parameter setup (IFSC from ATR TA3, AUTO_IFSD gate):
  `https://github.com/LudovicRousseau/CCID/blob/master/src/ifdhandler.c` (~line 1224).
- ST CCID middleware (bit-layout template):
  `https://github.com/STMicroelectronics/stm32-mw-usb-device/blob/master/Class/CCID/Inc/usbd_ccid_smartcard_template.h`
- USB CCID Rev 1.1 (dwFeatures exchange levels, SetParameters T=1 structure,
  XfrBlock): `docs/CCID_SPEC_AUDIT.md` in this repo + USB-IF DWG_Smart-Card_CCID_Rev110.
- osmo-ccid-firmware @ `639db14` (submodule): files cited in §4.
- In-repo history: commit `b1e685a` (STM32 transparent TPDU pipe decision),
  issue #101 (libccidtwin T=1 wall + bench caveats), issue #59/#60 context in
  AGENTS.md "Known-Good Bench State".
