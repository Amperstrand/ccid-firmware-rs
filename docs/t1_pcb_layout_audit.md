# T=1 PCB bit-layout audit — serial dialect vs ISO 7816-3 §11 (greatspectations)

Status: audit deliverable (2026-10-20). Trigger: bench wire captures 2026-10-20
showing the serial (GemPC Twin / libccidtwin) emission carrying I-block response
PCB = `0x10` for one exchange and `0x40` for the next, read at the bench as
"bit4 set — an N(R)-ish artifact" that libccidtwin "tolerated".

Audience: anyone changing `firmware/esp32-ccid/src/t1.rs` or claiming its PCB
bytes are non-conformant.

## TL;DR verdict

| Question | Verdict |
|---|---|
| Is `0x10` in a serial-dialect I-block PCB an artifact / RFU violation? | **No — conformant.** In the T=1 I-block layout, the bit at weight `0x10` (b5) is **N(R)**, not RFU. `0x10` = I(N(S)=0, M=0, N(R)=1): the legal piggybacked acknowledgment of the host's just-completed ns=0 chain. |
| Is `0x40` conformant? | **Yes.** `0x40` = I(N(S)=1, M=0, N(R)=0). |
| Did libccidtwin "tolerate" a spec violation? | **No.** Nothing was violated; additionally the host engine never inspects I-block bits b5..b1 on receive (it checks only N(S) at b7, LRC, and NAD). |
| Does `t1.rs::i_pcb` serve BOTH the serial T=1 and the RF ISO-DEP dialect? | **No — premise error.** `i_pcb` has zero callers on the RF path. The ISO 14443-4 PCB is built by the `iso14443` crate (git dep rev `268db79`), whose layout is different (see §2.2). No builder split is warranted. |
| Behavior change needed? | **No.** The audit concludes the emission is conformant under the repo's established, triangulated spec reading; per the task's own gate ("if and only if the audit concludes bit4 emission is non-conformant"), no second/split commit exists. Two stale doc comments that still described the **pre-7f136a0 buggy layout** were fixed (§5). |

## 1. What the code emits today, per dialect

### 1.1 Serial dialect (ISO 7816-3 T=1, card/responder role vs libccidtwin)

Single emission path:

```
ccid_handler.rs:348  self.t1.card_response(payload, took)   (XfrBlock relay answer)
  └─ t1.rs:381  T1Endpoint::card_response
       ├─ t1.rs:440  take_next_chain_block  → block(i_pcb(self.ns, more, self.host_ns), inf)
       └─ t1.rs:421  final_empty_block      → block(i_pcb(self.ns, false, self.host_ns), &[])
```

```rust
fn i_pcb(ns: u8, more: bool, nr: u8) -> u8 {
    (ns << 6) | ((more as u8) << 5) | (nr << 4)
}
```

Served over UART0/SPP/CDC by every classic main (`serve_uart` family), i.e. the
GemPC-Twin wire pcscd's `libccidtwin` terminates.

### 1.2 RF dialect (ISO 14443-4 ISO-DEP, PCD role vs the NFC card)

The MFRC522 path frames its blocks through the **iso14443 crate**:

- `mfrc522_driver.rs` imports `iso14443::type_a::{Frame, PcdSession, …}`; the
  driver's own comments record the resulting wire bytes
  (`mfrc522_driver.rs:597`: "Re-create session with None so I-blocks use
  PCB=0x02 (no CID)"; `:598`: "from_connect hardcodes Some(cid) → would produce
  PCB=0x0A").
- The crate's `src/type_a/pcb.rs` (rev `268db79`) defines the flags:

```rust
const BLOCK_NUMBER = 0b00000001;
const CID_FOLLOWING = 0b00001000;
const NAD_FOLLOWING = 0b00000100;
const CHAINING      = 0b00010000;
const BLOCK_TYPE_MASK = 0b11000000;
```

`grep` evidence for the premise correction: outside `t1.rs` itself, no file
references `i_pcb`; `mfrc522_driver.rs`/`nfc.rs` import `crate::t1::*` **only**
for the `now_us`/`elapsed_ms_since` timer helpers. The two dialects share no
PCB builder today, so a "split the PCB builders" change would be splitting
something that is already split.

### 1.3 Sibling check (fix-all-siblings protocol)

`firmware/ccid-firmware/src/t1_engine.rs` (STM32, initiator role toward a
contact card) builds `PCB_I_BLOCK | (ns << 6) | I_M_CHAIN(0x20)` and
`PCB_R_BLOCK | (nr << 4)` — it transmits **no** I-block N(R), exactly like the
openct initiator builder (§3.2). Consistent with the serial endpoint; no
divergent sibling exists.

## 2. What each spec requires

### 2.1 Provenance statement (specquotes policy)

**No verbatim text of ISO/IEC 7816-3 is available in this repository.** The
standard is a paywalled PDF; `docs/quote-sources/` tracks transcriptions only
for UM11495 and the M5Stack AXP192 datasheet, and this audit adds none —
inventing "verbatim" §11.4.2 text is prohibited (specquotes.toml policy; commit
25056daf removed non-verbatim markers for exactly this reason).

What IS tracked and quotable verbatim:

| Source | Pin | Role |
|---|---|---|
| `reference/CCID/src/openct/proto-t1.c` + `proto-t1.h` | submodule `1a0f75d1` (libccid 1.7.1-8-g1a0f75d) | **The T=1 engine the host actually runs** (libccidtwin terminates our serial dialect with this code) |
| `docs/T1_BEST_PRACTICES.md` | tracked in-repo (2026-10-10) | Prior triangulated §11.4.2 reading: openct + ST `usbd_ccid_smartcard_template.h` + G-01/G-02 bench evidence |
| `docs/quote-sources/` | — | No 7816-3 entry exists |

The bit layout below is therefore pinned against the tracked implementation
references and the repo's established triangulated reading — **not** against a
verbatim spec transcription. That is the honest provenance limit of this audit.

### 2.2 The two layouts side by side

T=1 I-block PCB (serial dialect; b8..b1, ISO bit numbering — b1 is LSB):

| b8 | b7 | b6 | b5 | b4 | b3 | b2 | b1 |
|----|----|----|----|----|----|----|----|
| 0 | N(S) | M | N(R) | 0 | 0 | 0 | 0 |

- Type bit: b8 = 0 for I-blocks.
- N(S), N(R): 1-bit (modulo-2) send/receive sequence numbers.
- M: chaining ("more") bit.
- b4..b1: reserved, sent as 0.
- N(R) occupies **the same position (b5, weight 0x10) in I-blocks and
  R-blocks** — the position is host-proven for R-blocks (§3.2, G-01 bench
  proof) and carried to I-blocks by the repo's triangulated reading.

Companion block layouts (same b8..b1, b1 = LSB convention):

- R-block: b8=1, b7=0, b6=0 (host rejects `pcb & 0x20` set on receive),
  b5=N(R), b4..b3=0, b2..b1 = error (00 none/next, 01 EDC error, 10 other).
- S-block: b8=1, b7=1, b6 = response(1)/request(0) — weight 0x20,
  `T1_S_RESPONSE` — b5..b3=0, b2..b1 = type (00 RESYNC, 01 IFS, 10 ABORT,
  11 WTX).

ISO 14443-4 I-block PCB (RF dialect, per the iso14443 crate): b8b7 = block
type (`00` = I), b5 = chaining (0x10!), b4 = CID-following, b3 = NAD-following,
b2 = 1 (I-block marker), b1 = block number. Base I-PCB `0x02`.

**Same weight, opposite meaning:** `0x10` is *chaining* in ISO-DEP but *N(R)*
in T=1; `0x20` is *M* in T=1 but part of the R/S type field in ISO-DEP. This is
why the bench byte `0x10` looked like "noise" to an ISO-DEP-trained eye while
being a perfectly standard T=1 piggyback ACK — and why the two builders must
never be conflated.

### 2.3 Verbatim quotes (openct `proto-t1.c` / `proto-t1.h`, pin 1a0f75d1)

Bit-position authority (the macros the host compiles against):

```c
/* I block */
#define T1_I_SEQ_SHIFT		6
```

```c
/* R block */
#define T1_IS_ERROR(pcb)	((pcb) & 0x0F)
#define T1_EDC_ERROR		0x01
#define T1_OTHER_ERROR		0x02
#define T1_R_SEQ_SHIFT		4
```

```c
/* S block stuff */
#define T1_S_IS_RESPONSE(pcb)	((pcb) & T1_S_RESPONSE)
#define T1_S_TYPE(pcb)		((pcb) & 0x0F)
#define T1_S_RESPONSE		0x20
```

```c
#define T1_I_BLOCK		0x00
#define T1_R_BLOCK		0x80
#define T1_S_BLOCK		0xC0
#define T1_MORE_BLOCKS		0x20
```

How the host builds blocks (`t1_build`) — note it transmits **no I-block
N(R)**:

```c
	/* Add the sequence number */
	switch (t1_block_type(pcb)) {
	case T1_R_BLOCK:
		pcb |= t1->nr << T1_R_SEQ_SHIFT;
		break;
	case T1_I_BLOCK:
		pcb |= t1->ns << T1_I_SEQ_SHIFT;
		t1->more = more;
		DEBUG_COMM2("more bit: %d", more);
		break;
	}
```

How the host validates what it receives — R-blocks are checked strictly
(`t1_transceive`, T1_R_BLOCK case):

```c
			if ((sdata[LEN] != 0x00)	/* length != 0x00 (illegal) */
				|| (pcb & 0x20)			/* b6 of pcb is set */
			   )
```

I-blocks are checked ONLY on the send-sequence bit (the card's N(S) at b7,
read via `t1_seq` → `(pcb >> T1_I_SEQ_SHIFT) & 1`; `T1_I_BLOCK` case):

```c
			if (t1_seq(pcb) != t1->nr) {
				DEBUG_COMM("wrong nr");
```

S(IFS response) is checked by exact equality (`t1_negotiate_ifsd`):

```c
				|| (sdata[PCB] != (T1_S_BLOCK | T1_S_RESPONSE | T1_S_IFS))) /* wrong PCB */
```

### 2.4 Host strictness matrix (what a strict host may reject)

| Received block | openct/libccidtwin checks | Strictness for our emission |
|---|---|---|
| I-block | N(S) at b7 only; LRC; NAD. b5..b1 **never inspected** | Our `0x10`/`0x40` pass; even genuinely RFU-set bits would pass (this is why G-02 was graded MED, not HIGH) |
| R-block | LEN = 0, b6 (0x20) must be clear, sequence at b5 | Our `0x80|nr<<4|err` conforms (fixed by G-01/7f136a0) |
| S(IFS response) | **Exact byte** `0xE1` (`T1_S_BLOCK|T1_S_RESPONSE|T1_S_IFS`) | Our `s_pcb` = `0xC0|resp<<5|type` conforms (0xE1) |

A hypothetical stricter-than-openct host that validated T=1 I-block reserved
bits would still accept our bytes: b4..b1 are emitted 0, and b5 is N(R) by
spec. The bench "tolerated bit4 noise" framing inverts the reality: the bit is
legal, and the host does not even look at it.

## 3. Observed bench bytes (2026-10-20), decoded per bit

| Capture | PCB | b8 | b7 N(S) | b6 M | b5 N(R) | b4..b1 | Reading |
|---|---|---|---|---|---|---|---|
| I-block response #1 | `0x10` | 0 | 0 | 0 | **1** | 0000 | I(0, M=0, N(R)=1) — ACK of the host's completed ns=0 chain |
| I-block response #2 | `0x40` | 0 | 1 | 0 | 0 | 0000 | I(1, M=0, N(R)=0) — next exchange, our N(S) toggled |

These are byte-exact with the pinned host-visible expectations of
`single_apdu_roundtrip_and_sequence_toggle` (t1.rs tests: first reply
`i_pcb(0, false, 1)`, second reply `i_pcb(1, false, 0)`) and with the
post-7f136a0 G-02 fix. The endpoint computes `nr = host_ns` — after a host
chain completes, `host_ns` has toggled to 1, so the first response carries
N(R)=1 by construction. Nothing on the wire is accidental.

## 4. Conformance verdict, per bit (serial I-block)

| Bit(s) | Spec requirement (per §2.2 reading) | Our emission | Verdict |
|---|---|---|---|
| b8 | 0 | 0 | ✅ |
| b7 | N(S), toggles per completed chain (§11.6.3) | `ns << 6`, toggled in `take_next_chain_block`/`final_empty_block` | ✅ |
| b6 | M = 1 while chaining | `more << 5`, set while `send_chain` has remainder | ✅ |
| b5 | N(R) = next expected N(S) of the peer | `nr << 4` (`self.host_ns`) | ✅ — this is the bench `0x10` bit |
| b4..b1 | RFU, shall be 0 | never set (`i_pcb` ORs only bits 6/5/4-shifted fields) | ✅ |

R-block and S-block cross-checks: `r_pcb = 0x80|(nr<<4)|err` (b6 clear, LEN 0)
✅; `s_pcb = 0xC0|(resp<<5)|type` → S(IFS response) = `0xE1` exact-match ✅.
Both were locked by `pcb_bit_layout_locked_to_iso_7816_3_table_45` since
7f136a0/2026-10-12.

## 5. Findings fixed by this audit commit (docs/comments only, no behavior change)

1. **t1.rs module header documented the pre-fix layout.** It read
   `I-block: 0 N(S) 0 M N(R) — 0x00 | ns<<6 | m<<5 | nr`,
   `R-block: … 0x80 | nr<<3 | err` and `S … response = request | 0x10` — the
   G-01/G-02-era bit positions (N(R) at weight 0x08, S-response at 0x10) that
   7f136a0 fixed in code but nobody updated in the header. Corrected to the
   Table-45 layout with explicit bit names.
2. **`parse_pcb` comment used 0-indexed bit names** ("N(R) at b4") inside a
   spec context that names b1 the LSB — clarified to name positions by weight.
3. **The audit premise** ("`i_pcb` used for both dialects") is disproven and
   recorded here so the next session doesn't re-derive it.

## 6. Test additions (this commit)

- `serial_i_pcb_matches_7816_3_section_11_quote` — GREAT EXPECTATION test:
  all eight (N(S), M, N(R)) combinations pinned as **byte literals**
  (0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70), plus the per-bit
  properties (b8=0, b7=N(S), b6=M, b5=N(R), b4..b1 = 0) over the full domain,
  plus round-trip through `parse_pcb`. If this test breaks, the breaker must
  re-verify against §2.2/§3 quotes — never "fix" the test to match new
  unverified bytes.
- `bench_capture_2026_10_20_pinned_conformant_serial_i_pcbs` — reproduces the
  bench sequence context and pins the two captured bytes (`0x10`, then `0x40`)
  with their exact N(S)/M/N(R) provenance.
- `specquotes.toml` gains `[sources.T1]` (marker `// T1:` →
  `reference/CCID/src/openct/proto-t1.c`), and the PCB builders now carry
  verbatim `// T1:` quotes so the drift job locks comment↔reference agreement.

## 7. Residual ambiguity (stated, not guessed)

- The **I-block N(R) position specifically** cannot be re-derived from openct
  alone (the host neither sends nor checks it — §3.2 quotes). It rests on the
  repo's triangulation (ST middleware template + spec reading recorded in
  `docs/T1_BEST_PRACTICES.md` §2.2, locked at 7f136a0) plus the
  same-position-as-R-blocks consistency argument. A verbatim ISO 7816-3
  §11.4.2 transcription into `docs/quote-sources/` would close this
  conclusively; that requires the licensed PDF (owner action).
- Sub-clause drift between the 1997/2006 editions of clause 11 is documented
  in `docs/T1_BEST_PRACTICES.md`; this audit follows the repo's established
  §11.4.2 numbering.
