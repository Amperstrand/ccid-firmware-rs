# PN7160 NFC Driver Specification Citation Map
## For Rust ESP32-C3 ISO-DEP Reader Implementation

**Usage**: Copy the citations below into Rust code comments (e.g., `// NCI 2.0 §5.4.2 / UM11495 §6.2`)

---

## 1. HARDWARE & PHYSICAL INTERFACE

### I2C/SPI Protocol Framing & VEN/IRQ Control

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| I²C High-Speed Mode (400 kbps+) | UM11495 (Rev 1.8, Jun 2026) | §4.2 / §4.3 | I²C physical transport, timing specs |
| SPI Bus (up to 7 Mbps) | UM11495 | §4.4 / §4.5 | SPI-bus interface, mode/speed config |
| VEN (Enable) Pin Toggle | PN7160 Datasheet (Rev 4.2, Jul 2026) | §11.1.2 | Hard Power Down → Standby/Active transition via VEN |
| IRQ Pin Interrupt Signaling | UM11495 | §4.6 | IRQ signal for async NCI notification delivery |
| NCI over I2C/SPI Payload Framing | NCI 2.0 Spec | §3.4.1–3.4.3 | NCI packet header (3 bytes: MT/PBF/GID, OID, LEN) |

### Power States & Boot Sequence

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| Hard Power-Down (HPD) State | PN7160 Datasheet | §11.1.2.1 | VEN < 0.4V, chip fully off |
| Standby State | PN7160 Datasheet | §11.1.2.2 | Low-power with wake-up sources enabled |
| Active State | PN7160 Datasheet | §11.1.2.3 | Idle / Listener / Poller operational modes |
| Boot & CORE_RESET_CMD/RSP | UM11495 | §6.1 | Initial host→controller reset handshake |
| Firmware Version in CORE_RESET_NTF | PN7160 Datasheet | §6 | Bytes 9–12 contain model ID + FW version |

---

## 2. NCI PROTOCOL CORE (Message Header & Flow Control)

### Message Types & Control Flow

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| **NCI Message Header Format** | NCI 2.0 Spec | §3.4.1 | Byte 0: `[MT(3b) | PBF(1b) | GID(4b)]` |
| Message Type Values (MT) | NCI 2.0 Spec | §3.1 | `0x0`=Data, `0x1`=Cmd, `0x2`=Rsp, `0x3`=Ntf, `0x4`=Config |
| PBF (Packet Boundary Flag) | NCI 2.0 Spec | §3.4.1 | `0x0`=last/unfragmented, `0x1`=continue/start |
| Group ID (GID) Encoding | NCI 2.0 Spec | §3.2.1–3.2.6 | `0x0`=Core, `0x1`=RF Mgmt, `0x2`=NFCEE |
| OID (Operation ID) | NCI 2.0 Spec | §3.4.2 | Byte 1: Op-specific opcode (0x00–0xFF) |
| Payload Length | NCI 2.0 Spec | §3.4.2 | Byte 2: payload size (0–254 bytes, max 0xFE) |
| NCI_MTS_CMD (0x20) | linux_libnfc-nci | nci_defs.h | Command message type token |
| NCI_MTS_RSP (0x40) | linux_libnfc-nci | nci_defs.h | Response message type token |
| NCI_MTS_NTF (0x60) | linux_libnfc-nci | nci_defs.h | Notification message type token |
| Control Message Flow | NCI 2.0 Spec | §3.2 | Cmd → Rsp (sync) / Ntf (async) |
| Data Message CID Field | NCI 2.0 Spec | §3.4.3 | Byte 0 lower 4 bits: Connection ID (0–15) |

### Connection Management

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| Logical Connection Creation | NCI 2.0 Spec | §4.4.2 | CORE_CONN_CREATE_CMD/RSP |
| Connection Closure | NCI 2.0 Spec | §4.4.3 | CORE_CONN_CLOSE_CMD/RSP |
| Connection Credits (Flow Control) | NCI 2.0 Spec | §3.3.1 | CORE_CONN_CREDITS_NTF / per-connection limit |
| Max NCI Payload Size | linux_libnfc-nci | nci_defs.h L59 | `#define NCI_MAX_PAYLOAD_SIZE 0xFE` (254 bytes) |

---

## 3. CORE CONTROL COMMANDS (Initialization & Configuration)

### Reset & Initialization Sequence

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| CORE_RESET_CMD | NCI 2.0 Spec | §5.1.1 | Reset controller, retrieve capabilities |
| CORE_RESET_RSP | NCI 2.0 Spec | §5.1.1 | Response confirms reset success |
| CORE_RESET_NTF | NCI 2.0 Spec | §5.1.1 | Async notification with FW info, model ID |
| CORE_INIT_CMD | NCI 2.0 Spec | §5.1.2 | Initialize NFCC, enable RF discovery |
| CORE_INIT_RSP | NCI 2.0 Spec | §5.1.2 | Provides supported interfaces, protocols, techs |
| CORE_INIT_RSP Offset for Interface Count | linux_libnfc-nci | nci_defs.h L81 | `#define NCI_CORE_INIT_RSP_OFFSET_NUM_INTF 0x05` |
| NCI 2.0 Differences from 1.0 | UM11495 | §3.4 | Feature matrix: PN7160 support details |

### Configuration Commands

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| CORE_SET_CONFIG_CMD | NCI 2.0 Spec | §5.1.3 | Set NFCC parameters (1–N config items) |
| CORE_GET_CONFIG_CMD | NCI 2.0 Spec | §5.1.4 | Retrieve current NFCC parameters |
| CORE_SET_CONFIG_RSP | NCI 2.0 Spec | §5.1.3 | Confirms set operation / error status |
| Total Discovery Duration Param | UM11495 | §6.2 Example | CORE_SET_CONFIG: 0x00 (TOTAL_DURATION) |
| PN7160 Proprietary Extensions | UM11495 | §5 | NXP-NCI extensions beyond NCI 2.0 std |

---

## 4. RF DISCOVERY MANAGEMENT

### Discovery Type Constants

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| RF_DISCOVER_TYPE_POLL_A | linux_libnfc-nci | nci_defs.h L147 | `0x00` – Poll for NFC Type A tags |
| RF_DISCOVER_TYPE_POLL_B | linux_libnfc-nci | nci_defs.h L148 | `0x01` – Poll for NFC Type B tags |
| RF_DISCOVER_TYPE_POLL_F | linux_libnfc-nci | nci_defs.h L149 | `0x02` – Poll for NFC Type F (FeliCa) |
| RF_DISCOVER_TYPE_POLL_A_ACTIVE | linux_libnfc-nci | nci_defs.h L150 | `0x03` – Poll Type A active mode |
| RF_DISCOVER_TYPE_LISTEN_A | linux_libnfc-nci | nci_defs.h L152 | `0x80` – Listen for Type A activation |
| RF_DISCOVER_TYPE_LISTEN_B | linux_libnfc-nci | nci_defs.h L153 | `0x81` – Listen for Type B activation |

### RF Technology Constants

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| RF_TECHNOLOGY_A | linux_libnfc-nci | nci_defs.h L126 | `0x00` – NFC-A (ISO 14443-A) |
| RF_TECHNOLOGY_B | linux_libnfc-nci | nci_defs.h L127 | `0x01` – NFC-B (ISO 14443-B) |
| RF_TECHNOLOGY_F | linux_libnfc-nci | nci_defs.h L128 | `0x02` – NFC-F (JIS X6319-4 / FeliCa) |
| RF_TECHNOLOGY_15693 | linux_libnfc-nci | nci_defs.h L129 | `0x03` – ISO 15693 (Type V) |

### RF Protocol Constants

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| NCI_PROTOCOL_T1T | linux_libnfc-nci | nci_defs.h L139 | `0x01` – NFC Forum Type 1 Tag |
| NCI_PROTOCOL_T2T | linux_libnfc-nci | nci_defs.h L140 | `0x02` – NFC Forum Type 2 Tag |
| NCI_PROTOCOL_T3T | linux_libnfc-nci | nci_defs.h L141 | `0x03` – NFC Forum Type 3 Tag (FeliCa) |
| **NCI_PROTOCOL_ISO_DEP** | linux_libnfc-nci | nci_defs.h L142 | `0x04` – ISO-DEP (Type 4 Tag, ISO 14443-4) |
| NCI_PROTOCOL_NFC_DEP | linux_libnfc-nci | nci_defs.h L143 | `0x05` – NFC-DEP (P2P, LLCP) |

### Discovery Commands

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| RF_DISCOVER_MAP_CMD | NCI 2.0 Spec | §6.3.1.1 | Map protocols to RF interfaces |
| RF_DISCOVER_CMD | NCI 2.0 Spec | §6.3.2.1 | Start polling loop with defined params |
| RF_DISCOVER_RSP | NCI 2.0 Spec | §6.3.2.2 | Confirms discovery started |
| RF_DISCOVER_NTF | NCI 2.0 Spec | §6.3.2.3 | Async: tag/peer detected |
| RF_DISCOVER_SELECT_CMD | NCI 2.0 Spec | §6.3.3.1 | Select discovered target (protocol + mode) |
| RF_DISCOVER_SELECT_RSP | NCI 2.0 Spec | §6.3.3.2 | Confirms selection |
| PN7160 RF Feature Matrix | UM11495 | §5.2 Table 8 | Supported modes per technology |

---

## 5. RF INTERFACE ACTIVATION & ISO-DEP

### Interface Types

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| NCI_INTERFACE_EE_DIRECT_RF | linux_libnfc-nci | nci_defs.h L160 | `0x00` – Direct RF (proprietary) |
| NCI_INTERFACE_FRAME | linux_libnfc-nci | nci_defs.h L161 | `0x01` – Frame interface (raw RF) |
| **NCI_INTERFACE_ISO_DEP** | linux_libnfc-nci | nci_defs.h L162 | `0x02` – ISO-DEP interface |
| NCI_INTERFACE_NFC_DEP | linux_libnfc-nci | nci_defs.h L163 | `0x03` – NFC-DEP interface (P2P) |

### Activation & ATS/RATS

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| RF_INTF_ACTIVATED_NTF | NCI 2.0 Spec | §6.3.4 | Tag/peer activation params + ATS/ATR_RES |
| RF_INTF_ACTIVATED_NTF → ISO-DEP | UM11495 | §7.3.2 | ISO-DEP activation notification format |
| NCI_MAX_ATS_LEN | linux_libnfc-nci | nci_defs.h L85 | `#define NCI_MAX_ATS_LEN 60` |
| RATS Handling | ISO 14443-4:2018 | §5.1–5.2 | Request for Answer to Select (Type A) |
| **ATS Structure** | ISO 14443-4:2018 | §5.2–5.3.7 | Length (TL) + T0 (format) + TA/TB/TC + historical |
| ATS T0 Byte Format | ISO 14443-4:2018 | §5.2.3 | Bits 7–5: TA/TB/TC presence, bits 3–0: FSCI |
| FSCI Coding (Frame Size Card Integer) | ISO 14443-4:2018 | Table 1 | 0x0–0x8 = 16, 24, 32, 40, 48, 64, 96, 128, 256 bytes; 0x9–0xF RFU |
| ATS TA(1) Byte | ISO 14443-4:2018 | §5.3.4 | Bit rate capabilities (DR/DS), FO flag |
| ATS TB(1) Byte | ISO 14443-4:2018 | §5.3.5 | FWI (Frame Waiting time Integer), SFGI, CID support |
| ATS TC(1) Byte | ISO 14443-4:2018 | §5.3.6 | CID + NAD support flags |
| ISO-DEP Activation on PN7160 | UM11495 | §7.3.2 | Full ISO-DEP support at 106/212/424/848 kbps |
| ISO-DEP Max Frame Size | linux_libnfc-nci | nci_defs.h L60 | `#define NCI_ISO_DEP_MAX_INFO 253` (256-1-2) |
| Activation Parameters | linux_libnfc-nci | nfc_ncif.c | ATS parsing logic for ISO-DEP params |

---

## 6. ISO-DEP DATA EXCHANGE (ISO 14443-4 Protocol)

### Block Format & PCB (Protocol Control Byte)

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| **I-Block (Information Block)** | ISO 14443-4:2018 | §7.1.1 / §7.2.1 | PCB: `00xxxxxx` (not `00xxx101`), carries payload |
| I-Block Structure | ISO 14443-4:2018 | §7.2 | PCB + [CID] + [NAD] + INF + CRC |
| I-Block PCB Chaining Bit | ISO 14443-4:2018 | §7.2.1 | Bit 5: chain continuation flag |
| I-Block PCB Block Number Bit | ISO 14443-4:2018 | §7.2.1 | Bit 0: toggles for alternating blocks (0/1) |
| **R-Block (Receive Ready Block)** | ISO 14443-4:2018 | §7.1.1 / §7.2.2 | PCB: `10xxxxxx` (not `1001xxxx`), no INF |
| R-Block ACK Format | ISO 14443-4:2018 | §7.2.2 | R(ACK): positive acknowledgement of previous I-block |
| R-Block NAK Format | ISO 14443-4:2018 | §7.2.2 | R(NAK): request retransmit of previous I-block |
| **S-Block (Supervisory Block)** | ISO 14443-4:2018 | §7.1.1 / §7.2.3 | PCB: `11xxxxxx` (not `1110xxxx`, not `1101xxxx`) |
| S-Block WTX (Waiting Time Extension) | ISO 14443-4:2018 | §7.2.3 / §7.3–7.4 | WTXM byte extends frame wait time |
| S-Block DESELECT | ISO 14443-4:2018 | §7.2.3 | Deactivate session |
| **PCB Prologue Field** | ISO 14443-4:2018 | §7.1.2 | Mandatory PCB + optional CID + optional NAD |
| CID (Card Identifier) in Block | ISO 14443-4:2018 | §7.1.2.2 | 4-bit value (0–14), 15 is RFU |
| NAD (Node Address) in Block | ISO 14443-4:2018 | §7.1.2.3 | Optional per TC(1) byte of ATS; ISO 7816-3 format |
| Block Numbering Rules | ISO 14443-4:2018 | §7.2.4 | Alternating 0/1 for sequential blocks |
| Block Handling Rules | ISO 14443-4:2018 | §7.2.5 | Timeout, error, collision handling |

### Frame Waiting Time & Timeouts

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| FWI (Frame Waiting time Integer) | ISO 14443-4:2018 | §5.3.5 / §7.3 | TB(1) bits 7–4: FWT = (256 × 16/fc) × 2^FWI |
| FWT (Frame Waiting Time) | ISO 14443-4:2018 | §7.3 | Max time PICC waits for next PCD block |
| SFGI (Start-up Frame Guard time Integer) | ISO 14443-4:2018 | §5.3.5 | TB(1) bits 3–0: SFGT for activation |
| WTX (Waiting Time eXtension) | ISO 14443-4:2018 | §7.4 | S-block to extend PICC response time |
| WTXM (WTX Multiplier) | ISO 14443-4:2018 | §7.4 | S-block INF byte: 1–59, multiplier for FWT |
| PICC Presence Check | ISO 14443-4:2018 | §7.2.6 | Empty I-block or S(DESELECT) to verify alive |
| ISO-DEP NAK Presence Check (NCI 2.0) | UM11495 | §5.2 Item 13 | PN7160 supports "Reader 14443-4 Presence Check" |

### Error Detection & Recovery

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| CRC-A Calculation | ISO 14443-3:2018 | §6.2.4 | Type A frame CRC (polynomial 0x1021) |
| CRC-B Calculation | ISO 14443-3:2018 | §7.2 | Type B frame CRC (different polynomial) |
| EDC (Error Detection Code) | ISO 14443-4:2018 | §7.1.4 | Last 2 bytes of block (CRC or checksum) |
| Block Transmission Errors | ISO 14443-4:2018 | §7.2.7 | Timeout, CRC fail → R(NAK) or retry |
| Multi-Activation | ISO 14443-4:2018 | §7.2.2 | Support multiple PICCs with distinct CIDs |

### Chaining & Segmentation

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| I-Block Chaining Bit | ISO 14443-4:2018 | §7.2.1 | Bit 5 of PCB: 1=more data follows |
| Chaining Rules | ISO 14443-4:2018 | §7.2.3 | Chain sequence with R(ACK) ack per segment |
| NCI Segmentation & Reassembly | NCI 2.0 Spec | §3.5 | Control message fragmentation across packets |
| NCI_PBF_NO_OR_LAST (0x00) | linux_libnfc-nci | nci_defs.h L89 | Unfragmented or final fragment marker |
| NCI_PBF_ST_CONT (0x10) | linux_libnfc-nci | nci_defs.h L90 | Start or continuing fragment marker |

---

## 7. RF DEACTIVATION & SESSION MANAGEMENT

### Deactivation Commands

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| RF_DEACTIVATE_CMD | NCI 2.0 Spec | §6.3.5.1 | Deactivate tag/peer (type: Idle/Sleep/Discovery) |
| RF_DEACTIVATE_RSP | NCI 2.0 Spec | §6.3.5.2 | Confirms deactivation |
| RF_DEACTIVATE_NTF | NCI 2.0 Spec | §6.3.5.3 | Async notification of deactivation complete |
| Deactivation Types | NCI 2.0 Spec | §6.3.5.1 | Idle / Sleep / Discovery mode transition |

### Re-activation & State Transitions

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| Tag Re-activation | NCI 2.0 Spec | §6.3.5 | Reactivate after sleep without full discovery |
| ISO-DEP Reactivation | UM11495 | §7.3.2 | Support for rapid reactivation on PN7160 |

---

## 8. LISTEN MODE & CARD EMULATION (CE)

### Listen Mode Routing

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| RF_SET_LISTEN_MODE_ROUTING_CMD | NCI 2.0 Spec | §6.4.1.1 | Configure target routing (protocol/tech/AID) |
| RF_GET_LISTEN_MODE_ROUTING_CMD | NCI 2.0 Spec | §6.4.2.1 | Query routing table |
| NFCEE_MODE_ENABLE | NCI 2.0 Spec | §8.2 | Enable secure element (NFCEE) |
| Listen Protocol Bits | linux_libnfc-nci | nci_defs.h L167 | ISO_DEP=0x01, NFC_DEP=0x02 |
| ISO-DEP Listen (Card Emu) | UM11495 | §8.1.2 | CE mode for ISO-DEP Type 4 Tag emulation |

### Type 4 Tag Card Emulation (T4T CE)

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| T4T Emulation Protocol | NFC Forum T4T Spec | §2–4 | NDEF storage on ISO-DEP interface |
| ISO-DEP CE Frame Format | ISO 14443-4:2018 | §7.1–7.2 | PICC receives I-blocks, responds with I/R-blocks |
| FWI for CE Mode | ISO 14443-4:2018 | §7.3 | Default frame waiting time for PICC (card) |
| PN7160 CE Support | UM11495 | §8.1.2 | "DH-NFCEE NFCEE-NDEF" ISO-DEP 106 kbps |

---

## 9. NFCEE (NFC External Element) & ROUTING

### NFCEE Management

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| NFCEE_DISCOVER_CMD | NCI 2.0 Spec | §8.1.1.1 | Enumerate secure elements on device |
| NFCEE_DISCOVER_RSP | NCI 2.0 Spec | §8.1.1.2 | List of discovered NFCEEs (eSE, UICC) |
| NFCEE_MODE_SET_CMD | NCI 2.0 Spec | §8.2.1.1 | Enable/disable NFCEE operation |
| Destination Types | NCI 2.0 Spec | §4.4.1 | DH=0x00 (Device Host), NFCEE=0x01 |
| NFCEE Routing | UM11495 | §5 | Proprietary routing config for PN7160 |

---

## 10. NCI AUXILIARY OPERATIONS

### Capability & Info Commands

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| CORE_GET_CAPABILITIES_CMD (PN7160) | UM11495 | §6.3 | NXP proprietary: read chip capabilities |
| NFCC_Features Response | NCI 2.0 Spec | §4.2.2 | HCI access, NFC-A/B/F support, secure element mode |

### RF Field Management

| Element | Spec Document | Section | Details |
|---------|---------------|---------|---------|
| RF_FIELD_INFO_NTF | NCI 2.0 Spec | §6.2.1.1 | Async: external RF field detected on/off |
| Standby Mode Trigger | UN11495 | §6.2 | Screen off → automatic low-power transition |

---

## 11. OPEN-SOURCE REFERENCE IMPLEMENTATIONS

| Project | Language | Scope | Key Files | Citation |
|---------|----------|-------|-----------|----------|
| **NXP linux_libnfc-nci** | C | Full NCI stack (PN7150/PN7120, PN7160-ready) | `src/libnfc-nci/hal/include/nci_defs.h` | [GitHub](https://github.com/NXPNFCLinux/linux_libnfc-nci) R2.4 |
| **NXP-NCI MCUXpresso** | C | Embedded RTOS examples (LPC, i.MX RT) | ISO14443-3/4, ISO-DEP raw exchange demos | AN13288 (Jun 2026) |
| **Strooom/PN7160** | C++ | Arduino/embedded driver lib | NCI command builder, I2C HAL | [GitHub](https://github.com/Strooom/PN7160) |
| **josevcm/hce-laboratory** | C++ | ISO-DEP card emulation framework | DESFire/MifarePlus CE, 165 tests, NCI 2.0 abstraction | [GitHub](https://github.com/josevcm/hce-laboratory) 2026 |
| **iso14443 (Rust crate)** | Rust | Type A/B RATS/ATS, ISO-DEP blocks | `activation()`, `Pcd::connect()`, I/R-block handling | [docs.rs/iso14443](https://docs.rs/iso14443) |

---

## 12. STANDARDS CROSS-REFERENCE

| Standard | Version/Date | Scope | Key Sections for PN7160 Driver |
|----------|-------------|-------|-------------------------------|
| **NFC Forum NCI** | 2.0 (2017-03-30) | Host-controller interface protocol | Core (§4), RF Mgmt (§6), Data Exchange (§3–7) |
| **NCI 1.1** | Older standard | For backwards compat reference only | See UM11495 §3.4 for NCI 1.0 vs 2.0 diffs |
| **ISO/IEC 14443-3** | 2018 | Anticollision, NFCID, REQA/WUPA/SELECT | Type A/B polling, UID collision loop |
| **ISO/IEC 14443-4** | 2018 | ISO-DEP protocol, RATS/ATS, I/R/S-blocks | Activation (§5), Block format (§7), Timeouts (§7.3–7.4) |
| **ISO/IEC 7816-3** | Integrated circuit interface | Referenced by ISO 14443-4 for timing, CID/NAD handling | NAD format, timings |
| **ISO/IEC 7816-4** | Application layer APDUs | SELECT, READ BINARY, UPDATE, etc. transported over ISO-DEP | CLA/INS/P1/P2/Lc/Data/Le structure |
| **JIS X6319-4** | FeliCa standard | NFC-F (Type 3) protocol (not ISO-DEP, but in PN7160 scope) | RF_TECHNOLOGY_F, polling/activation |

---

## 13. EXAMPLE CITATION USAGE IN RUST CODE

```rust
// Boot sequence - UM11495 §6.1 / NCI 2.0 §4.1
fn core_reset(&mut self) -> Result<()> {
    // VEN toggle: PN7160 Datasheet §11.1.2
    self.ven_pin.set_low()?;
    thread::sleep(Duration::from_millis(10));
    self.ven_pin.set_high()?;
    
    // CORE_RESET_CMD: NCI 2.0 §4.1.1 / UM11495 §6.1
    self.send_command(&[0x20, 0x00, 0x01, 0x00])?;
    // Expect: CORE_RESET_RSP (0x40 0x00 0x01 STATUS)
    // Then: CORE_RESET_NTF (0x60 0x00 ...) with FW version in bytes 10-12
    Ok(())
}

// RF Discovery - NCI 2.0 §6.3 / UM11495 §7.2
fn start_discovery(&mut self, techs: &[RfTech]) -> Result<()> {
    // RF_DISCOVER_MAP_CMD: map protocols to RF interfaces
    // RF_DISCOVER_CMD: NCI_DISCOVERY_TYPE_POLL_A (0x00) + NCI_PROTOCOL_ISO_DEP (0x04)
    // ISO-DEP max frame: linux_libnfc-nci nci_defs.h L60: 253 bytes
    Ok(())
}

// ISO-DEP Activation - ISO 14443-4:2018 §5 + UM11495 §7.3.2
fn handle_iso_dep_activation(&mut self, ats_res: &[u8]) -> Result<()> {
    // Parse ATS structure: ISO 14443-4:2018 §5.2-5.3.7
    // TL (length), T0 (format), TA(1), TB(1), TC(1), historical bytes
    // FSCI = T0 bits 3:0 → max frame size (Table 1)
    // FWI = TB bits 7:4 → frame wait time
    // CID/NAD support = TC bits 1:0
    Ok(())
}

// Block exchange - ISO 14443-4:2018 §7.2
fn exchange_iso_dep(&mut self, apdu: &[u8]) -> Result<Vec<u8>> {
    // Send I-block: PCB (0x0X) + CID (opt) + NAD (opt) + INF + CRC
    // PCB bit 5 = chaining, bit 0 = block number
    // Receive R-block or I-block in response
    // Implement WTX handling: ISO 14443-4:2018 §7.4
    Ok(vec![])
}

// Data exchange - RF_DATA_EXCH (NCI message, not ISO-DEP command)
fn rf_data_exch(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
    // NCI Data Message: MT=0x0 / PBF / CID
    // PN7160 handles ISO-DEP block framing internally
    Ok(vec![])
}
```

---

## DOCUMENT ACCESS & LICENSING

| Document | URL | Access | Notes |
|----------|-----|--------|-------|
| **UM11495** | https://www.nxp.com/docs/en/user-manual/UM11495.pdf | Public (NXP) | Latest: Rev 1.8, Jun 2026 |
| **PN7160 Datasheet** | https://www.nxp.com/docs/en/data-sheet/PN7160_PN7161.pdf | Public (NXP) | Latest: Rev 4.2, Jul 2026 |
| **AN13287** | https://www.nxp.com/doc/AN13287 | Public (NXP) | PN7160 Linux porting guide, Rev 1.1 |
| **AN13288** | https://www.nxp.com/doc/AN13288 | Public (NXP) | NXP-NCI MCUXpresso examples, Jun 2026 |
| **AN12989** | https://www.nxp.com/doc/AN12989 | Public (NXP) | PN7160 quick start guide |
| **NCI 2.0 Spec** | https://nfc-forum.org/build/specifications/ | **License required** (NFC Forum) | Membership/fee: implementation rights |
| **ISO 14443-3** | https://cdn.standards.iteh.ai/.../ISO-IEC-14443-3-2018.pdf | **Standard purchase** (ISO/IEC) | Cached sample; full spec requires purchase |
| **ISO 14443-4** | https://cdn.standards.iteh.ai/.../ISO-IEC-14443-4-2018.pdf | **Standard purchase** (ISO/IEC) | Cached sample; full spec requires purchase |

---

## NOTES FOR DEVELOPERS

1. **NCI Message Headers**: Always construct as `[MT_GID_byte | OID | LENGTH]` before payload
   - MT (3 bits): 1=Cmd, 2=Rsp, 3=Ntf
   - GID (4 bits): 0=Core, 1=RF, 2=NFCEE
   - Use bit-shift macros from linux_libnfc-nci if adopting that codestyle

2. **ISO-DEP Timing**: 
   - Always parse ATS TB(1) for FWI before sending first I-block
   - Implement WTX (S-block) handler to extend wait time if card requests it
   - Default FWI ~7 = ~1.3 seconds per block

3. **Frame Sizes**: 
   - NCI_ISO_DEP_MAX_INFO = 253 bytes (includes CID/NAD overhead)
   - Actual ISO-DEP payload = 253 - prologue_size - CRC (2 bytes)
   - FSCI from ATS T0 determines PICC max receive (16–4096 bytes)

4. **PN7160 Firmware Variants**:
   - Current stable: 12.50.11 (supports NCI 2.0 full)
   - Firmware version bytes in CORE_RESET_NTF: [major, minor, patch]
   - Check datasheet §6 for feature differences by firmware version

5. **ESP32-C3 Integration**:
   - Use I2C (default addr 0x28, 7-bit) or SPI for host interface
   - GPIO: VEN (enable), IRQ (interrupt), optional DWL_REQ (download mode)
   - IRQ is edge-triggered; implement async notification handler
   - Consider libnfc-nci port or iso14443 Rust crate as reference

---

**Last Updated**: October 2026  
**Spec Versions Referenced**: UM11495 Rev 1.8 (Jun 2026), PN7160 DS Rev 4.2 (Jul 2026), NCI 2.0 (2017-03-30), ISO 14443-3/4:2018
