# CCID Reader — Comparison with Other Implementations in This Project

This document compares and contrasts the **ccid-reader** firmware (STM32F469, Rust, removable-card reader) with other CCID-related implementations present in the workspace. It is intended for maintainers and anyone evaluating design choices or reusing patterns across projects.

---

## 1. Overview

| Project | Role | Language | Hardware | Slot model | Protocol level |
|--------|------|----------|----------|------------|----------------|
| **ccid-reader** | Reader firmware | Rust (no_std) | STM32F469-DISCO | Single slot, removable card | Short APDU (T=0 + T=1) |
| **osmo-ccid-firmware** | Reader firmware / host gadget | C | sysmoOCTSIM (8-slot) or Linux FunctionFS | Multi-slot (8) | TPDU / APDU via slot ops |
| **CCID (libccid)** | Host driver | C | Host only | N/A (talks to readers) | TPDU, Short APDU, Extended APDU |
| **canokey-stm32** | Token firmware | C | STM32 | No slot (embedded “card”) | CCID interface only; logic in canokey-core |
| **GoKey** | Token firmware | Go (TamaGo) | USB armory Mk II | No slot (embedded “card”) | CCID interface; OpenPGP/FIDO/age |

---

## 2. ccid-reader (this firmware)

**Location:** [ccid-reader/](ccid-reader/)

**Purpose:** Turn an STM32F469 board with a smartcard slot into a USB CCID reader that pcscd/libccid can use. Target behaviour: “boring” generic reader — Short APDU, T=0 and T=1, one removable card.

**Architecture:**

- **Single slot:** One physical slot (GPIO card-detect, one USART for ISO 7816). Slot state: Absent / PresentInactive / PresentActive.
- **Synchronous command handling:** One CCID command at a time. `cmd_busy` blocks new commands until the current response (including TX fragmentation and ZLP) is fully sent.
- **Short APDU only:** Host sends raw APDU in XfrBlock; firmware runs T=0 or T=1 engine and returns R-APDU. Extended APDU (payload > 261 bytes) rejected.
- **Three USB endpoints:** Bulk IN, Bulk OUT, Interrupt IN (NotifySlotChange).
- **No heap:** `no_std`, fixed buffers (e.g. 271-byte CCID message buffer).

**Contrasts:**

- **vs osmo:** We have one slot and synchronous handling; osmo has eight slots and async slot ops (e.g. `xfr_block_async`). We reused osmo’s command→response-type rules (gen_err_resp) and 3-state ICC status.
- **vs libccid:** We are the device; libccid is the host. We implement the same message layouts and descriptor fields so that libccid’s generic path works.
- **vs CanoKey/GoKey:** We are a **reader** (external card); they are **tokens** (the “card” is inside the device). They have no card-detect GPIO, no slot FSM, and no physical T=0/T=1 to a separate chip.

---

## 3. osmo-ccid-firmware

**Location:** [osmo-ccid-firmware/](osmo-ccid-firmware/) (ccid_common/, sysmoOCTSIM/)

**Purpose:** CCID device implementation for the sysmoOCTSIM 8-slot reader, or as a Linux userspace USB gadget (FunctionFS). Reference for reader-side CCID behaviour.

**Architecture:**

- **Multi-slot:** `NR_SLOTS = 8`; each slot has its own `ccid_slot` (icc_present, icc_powered, cmd_busy, pars, proposed_pars, default_pars).
- **Slot ops abstraction:** Commands are dispatched to slot operations (e.g. `icc_power_on_async`, `xfr_block_async`, `set_params`). Power-on and XfrBlock can be **asynchronous** (return “busy”, complete later via callback).
- **Message construction:** Central helpers (e.g. `ccid_gen_data_block_nr`, `ccid_gen_slot_status_nr`, `ccid_gen_parameters_t0_nr`) and **gen_err_resp** map each command type to the correct response type (DataBlock / SlotStatus / Parameters / Escape).
- **Dynamic allocation:** Uses `msgb` (osmocom message buffers) and allocators; not no_std.
- **ISO 7816 below:** iso7816_fsm.c, iso7816_3.c, cuart; platform-specific UART/card control behind an abstraction.

**What we took from osmo:**

- Message type and error code constants (ccid_proto.h).
- Rule “which command gets which response type on error” (gen_err_resp).
- 3-state ICC status (NO_ICC / PRES_INACT / PRES_ACT) and cmd_busy semantics.
- Parameter encode/decode patterns (Get/SetParameters, T=0 vs T=1).

**Where we differ:**

- We do **not** use async slot ops; we block in the handler until the response is queued (then TX is fragmented in poll).
- Single slot only; no per-slot pars/proposed_pars/default_pars.
- We implement T=0 and T=1 engines ourselves (Rust); osmo uses a separate ISO 7816 FSM and TPDU/APDU layer.

**Learnings and differences (parameters):**

- We do **not** apply host **SetParameters** or **ResetParameters** to runtime. Osmo uses `proposed_pars` (updated by SetParameters) and `default_pars` (applied on ResetParameters), and applies them to the next T=0/T=1 exchange (e.g. baud, WI, IFSC). Our firmware responds to GetParameters/SetParameters/ResetParameters with **ATR-derived parameters only** and never changes baud, WI, or IFSC based on host-supplied data. This is intentional: we use the card’s ATR (and PPS/IFSD negotiation) as the single source of truth. ResetParameters in our implementation returns the current ATR-derived params; it does not “reset to defaults” or re-apply osmo-style default_pars. If host-driven parameter changes are ever needed, the osmo pattern (proposed_pars, apply on next exchange) can be adopted.

---

## 4. CCID (libccid)

**Location:** [CCID/](CCID/) (src/, readers/)

**Purpose:** Host-side driver used by pcscd to talk to USB CCID readers. Not a reader firmware.

**Architecture:**

- **Host only:** Opens USB devices, sends PC_to_RDR_* commands on Bulk OUT, reads RDR_to_PC_* on Bulk IN. Matches readers by VID/PID (and optionally interface class) via Info.plist / supported_readers.txt.
- **Exchange levels:** Supports TPDU, Short APDU, Extended APDU depending on reader’s dwFeatures. For Short APDU it sends the raw APDU in XfrBlock and expects the reader to do T=0/T=1.
- **Sequence numbers:** Increments bSeq per command and expects the same bSeq in the response.
- **Reader list:** [CCID/readers/supported_readers.txt](CCID/readers/supported_readers.txt) lists VID:PID and display names; this firmware uses `0x08E6:0x3437` for IDBridge CT30/PC Twin compatibility.

**Relationship to ccid-reader:**

- We implement the **device side** of what libccid expects: same 10-byte header, same response types, same descriptor semantics (e.g. dwFeatures, dwProtocols). We did **not** copy libccid code; we use it as the specification for host behaviour and packet layout (see RESEARCH.md, ALIGNMENT.md).

---

## 5. canokey-stm32

**Location:** [canokey-stm32/](canokey-stm32/)

**Purpose:** STM32-based CanoKey token (FIDO2, OpenPGP, PIV, etc.). Presents multiple USB interfaces, including CCID.

**Architecture:**

- **Token, not reader:** No external card slot. The “card” is the CanoKey application logic (in canokey-core). CCID is one interface among others (e.g. CTAP HID).
- **USB resource allocation:** [canokey-stm32/Src/usb.c](canokey-stm32/Src/usb.c) only assigns interface and endpoint indices for the CCID interface; actual CCID handling lives in canokey-core.
- **No slot FSM:** No card-detect, no NotifySlotChange, no multi-slot. Single “virtual” card always present.

**Contrast with ccid-reader:**

- We are a **reader** with a physical slot and real ISO 7816 (USART, T=0/T=1). CanoKey is a **token**; CCID is just one way for the host to send APDUs to the same internal applet layer. We do not share code with canokey-stm32; only the idea “CCID is one interface on the USB device” is common.

---

## 6. GoKey

**Location:** [GoKey/](GoKey/)

**Purpose:** USB smartcard (OpenPGP, FIDO, age) in Go, running on USB armory Mk II with TamaGo. Presents as a CCID device so the host can use it like a card.

**Architecture:**

- **Token, not reader:** Like CanoKey, the “card” is inside the device. [GoKey/internal/ccid/](GoKey/internal/ccid/) implements the CCID command side (ICC_POWER_ON, XFR_BLOCK, GET_SLOT_STATUS, etc.) and delegates to [GoKey/internal/icc/](GoKey/internal/icc/) for the actual “card” logic.
- **Go, userspace-style:** Not no_std; runs under TamaGo with a full runtime. Uses slices and standard libraries.
- **Minimal CCID surface:** Implements the commands needed for OpenPGP/FIDO usage (power on/off, slot status, XfrBlock, Get/SetParameters). No separate T=0/T=1 engine — the “card” responds at APDU level.

**Contrast with ccid-reader:**

- GoKey is a **token** (embedded card); we are a **reader** (external card, real ISO 7816). We implement real T=0/T=1, ATR, PPS, IFSD; GoKey’s “ATR” and behaviour are defined by its application logic, not by a physical chip. We did not reuse GoKey code; the comparison is architectural only.

---

## 7. Summary Table

| Aspect | ccid-reader | osmo-ccid-firmware | libccid (CCID) | canokey-stm32 | GoKey |
|--------|-------------|--------------------|----------------|---------------|--------|
| **Device vs host** | Device (reader) | Device (reader/gadget) | Host (driver) | Device (token) | Device (token) |
| **Slots** | 1 | 8 | N/A | 0 (virtual) | 0 (virtual) |
| **Removable card** | Yes | Yes | N/A | No | No |
| **Command handling** | Sync, cmd_busy | Async slot ops | Sends commands | Delegates to core | Sync, delegates to icc |
| **T=0 / T=1** | In firmware | In ISO7816 FSM | Expects reader | In canokey-core | In icc package |
| **Exchange level** | Short APDU | Configurable | TPDU/Short/Extended | APDU (internal) | APDU (internal) |
| **NotifySlotChange** | Yes (ep_int) | Yes | Consumes | Typically no slot | N/A |
| **Memory model** | no_std, static buffers | malloc/msgb | Host heap | Depends on core | Go heap |
| **Primary reference for us** | — | Command/response types, slot semantics | Packet layout, descriptor | USB layout idea only | Architectural contrast only |

---

## 8. References

- [ccid-reader/VERIFICATION.md](ccid-reader/VERIFICATION.md) — Spec alignment and test steps.
- [ccid-reader/RESEARCH.md](ccid-reader/RESEARCH.md) — What we took from each reference (including osmo and libccid).
- [ccid-reader/ALIGNMENT.md](ccid-reader/ALIGNMENT.md) — Field-by-field alignment with CCID and ISO 7816.
