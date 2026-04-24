# CCID Secure PIN Reader - Implementation Milestones

## Executive Summary

This document defines the milestones required to transform the STM32F469-DISCO firmware into a **standards-compliant CCID Class 4 Secure PIN Reader** with touchscreen display and transaction confirmation (SWYS) capabilities.

**Target Device Classification:** CCID Class 4 (Advanced PIN Pad with Graphics Display)

**Current Status:** Both STM32 contact CCID and ESP32 NFC CCID verified working on `main` branch (M0-M3 + ESP32-NFC complete). PIN pad modules exist but are not integrated with CCID protocol layer.

---

## Milestone Overview

```
┌─────────────────────────────────────────────────────────────────────────────────────────┐
│                        MILESTONE ROADMAP                                                │
├─────────────────────────────────────────────────────────────────────────────────────────┤
│                                                                                         │
│  PHASE 1: FOUNDATION (M4-M6)                                                           │
│  ══════════════════════════                                                            │
│  M4: CCID Descriptor Update ──────► M5: PC_to_RDR_Secure Handler ──► M6: PIN Entry UI  │
│       [~2 days]                        [~3 days]                      [~2 days]          │
│                                                                                         │
│  PHASE 2: INTEGRATION (M7-M9)                                                          │
│  ══════════════════════════                                                            │
│  M7: APDU Flow Integration ──────► M8: Error Handling ───────────► M9: Host Testing    │
│       [~2 days]                     [~1 day]                         [~2 days]          │
│                                                                                         │
│  PHASE 3: ADVANCED FEATURES (M10-M12)                                                  │
│  ══════════════════════════════════                                                     │
│  M10: SWYS Transaction Confirm ──► M11: PIN Modification ─────────► M12: Security Hard │
│       [~3 days]                      [~2 days]                          [~3 days]       │
│                                                                                         │
│  PHASE 4: CERTIFICATION PREP (M13-M15)                                                 │
│  ══════════════════════════════════                                                     │
│  M13: Compliance Testing ─────────► M14: Security Audit ──────────► M15: Documentation │
│       [~3 days]                      [~2 days]                          [~1 day]        │
│                                                                                         │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

---

## Prerequisites (Already Complete)

### M0-M3: Basic CCID Reader ✅ COMPLETE

**Status:** Achieved 2026-03-08

**Evidence:**
- Full ATR received from card
- T=1 protocol working with sequence number fixes
- SeedKeeper secure channel + VERIFY_PIN + EXPORT_SECRET working
- `test_pysatochip.py --reader OMNIKEY` passes

**Documentation:** [LEARNINGS.md](../LEARNINGS.md) §"STM32 CCID reader: full success"

---

### ESP32-NFC: ESP32 NFC CCID Integration ✅ COMPLETE

**Status:** Achieved 2026-04-24

**Objective:** Integrate ESP32 + MFRC522 NFC CCID firmware into `main` branch alongside STM32 contact CCID.

**Evidence:**
- `esp32-serial-ccid` branch merged into `main` via fast-forward + rebase
- Both firmware products build and pass all tests from `main`:
  - STM32: 82 host tests pass
  - ESP32: 75 host tests pass
  - Vendored iso14443-rs: 52 host tests pass
  - ESP32 Xtensa release build passes
- **Hardware verification on 2026-04-24 (both readers simultaneously on same host):**

| Reader | Transport | Card | ATR | Protocol |
|--------|-----------|------|-----|----------|
| Cherry SmartTerminal ST-2xxx (STM32) | USB CCID | ComSign eID (contact) | `3B D5 18 FF 81 91 FE 1F C3 80 73 C8 21 10 0A` | T=1, IFSC=254 |
| GemPCTwin serial (ESP32 + MFRC522) | Serial CCID | NXP P71 SmartMX3 JCOP4 (NFC) | `3B 85 80 01 80 73 C8 21 10 0E` | T=0/T=1 |

**What was integrated:**
- `esp32-ccid/` package: ESP32 firmware with MFRC522 (I2C) and PN532 (SPI) NFC backends
- `vendor/iso14443-rs/`: Patched ISO 14443 protocol crate (local patches for PcdSession, timeouts, FSC)
- `vendor/mfrc522/`: Patched MFRC522 driver crate
- Vendored dependencies tracked in git (no nested `.git`, no build artifacts)
- CI coverage for both products (STM32 + ESP32 + iso14443 host-test jobs)

**Known issues:**
- FTDI FT232 chip wedged by espflash DTR/RTS toggles — physical USB replug required after flash
- `esp-idf-svc` requires ESP-IDF 5.2.4+ (pinned via `esp_idf_version = "tag:v5.2.4"`)

**Next steps (refactoring toward shared architecture):**
- Extract shared CCID protocol constants/types into a shared crate (GitHub issue #10)
- Split STM32 `ccid.rs` (1270 lines) and `smartcard.rs` (936 lines) into focused modules (issues #9, #8)
- Unify ATR parsing (issue #6)
- Add STM32 CCID handler unit tests (issue #7)

---

## Phase 1: Foundation

### M4: CCID Descriptor Update for PIN Support

**Objective:** Update CCID class descriptor to advertise PIN pad and display capabilities to the host.

**Duration:** ~2 days

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| `bPINSupport = 0x03` in descriptor | `lsusb -v` shows "PIN Support: 0x03" |
| `wLcdLayout` set to touchscreen dimensions | `lsusb -v` shows non-zero LCD layout |
| `dwFeatures` bit 18 (0x00040000) set for LCD | Descriptor dump shows LCD support bit |
| Device still enumerates as CCID | `lsusb` shows device, `pcsc_scan` detects reader |
| No regressions in basic CCID functions | M0-M3 tests still pass |

**Tasks:**
1. Update `src/ccid.rs` CCID_CLASS_DESCRIPTOR_DATA:
   - Line ~165: Change `bPINSupport` from `0x00` to `0x03`
   - Line ~164: Set `wLcdLayout` to `0x1010` (16 lines × 16 chars placeholder)
   - Line ~153: Add bit 18 to `dwFeatures` (0x00040000 for LCD)
2. Rebuild and flash firmware
3. Verify with `lsusb -v -d <vid:pid>`
4. Run regression tests (M0-M3)

**Internal References:**
- [docs/PINPAD-ARCHITECTURE.md](PINPAD-ARCHITECTURE.md) §6 - CCID Specification Details
- [PINPAD-IMPLEMENTATION-PLAN.md](../PINPAD-IMPLEMENTATION-PLAN.md) - Phase 1

**External References:**
- [CCID Rev 1.1 Specification](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf) - Section 5.1 "Class Descriptor"
- [CCID Descriptor Statistics - Ludovic Rousseau](https://blog.apdu.fr/posts/2014/01/ccid-descriptor-statistics-bpinsupport/)

**Dependencies:** None (can start immediately)

---

### M5: PC_to_RDR_Secure Handler Implementation

**Objective:** Implement the CCID `PC_to_RDR_Secure` (0x69) command handler to receive PIN verification requests from the host.

**Duration:** ~3 days

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| Handler parses PIN Verification Data Structure | Unit test passes with known-good data |
| Handler extracts APDU template correctly | Template bytes match input |
| Handler extracts PIN constraints (min/max/timeout) | Parsed values match input |
| Handler returns `CMD_NOT_SUPPORTED` only for unsupported sub-commands | Supported operations proceed |
| Handler returns proper error for malformed requests | Returns error code, not crash |

**Tasks:**
1. Replace stub in `src/ccid.rs:415-418` with real handler
2. Implement PIN Verification Data Structure parsing (CCID §6.1.11)
3. Implement PIN Modification Data Structure parsing (CCID §6.1.12) 
4. Add validation for bmFormatString, wPINMaxExtraDigit
5. Create integration point for PIN entry UI trigger
6. Write unit tests for parsing

**Data Structure (CCID §6.1.11):**
```
Offset 0:  bTimerOut              (1 byte)  - Timeout in seconds
Offset 1:  bmFormatString         (1 byte)  - PIN format flags
Offset 2:  bmPINBlockString       (1 byte)  - PIN block length
Offset 3:  bmPINLengthFormat      (1 byte)  - PIN length format
Offset 4-5: wPINMaxExtraDigit     (2 bytes) - Max/min PIN length
Offset 6:  bEntryValidationCondition (1 byte) - Validation trigger
Offset 7:  bNumberMessage         (1 byte)  - Messages to display
Offset 8-9: wLangId               (2 bytes) - Language ID
Offset 10: bMsgIndex              (1 byte)  - Message index
Offset 11: bTeoPrologue           (1 byte)  - TPDU prologue
Offset 12+: abPINApdu             (var)     - APDU template
```

**Internal References:**
- [src/pinpad/mod.rs](../src/pinpad/mod.rs) - `PinVerifyParams::parse()` already implemented
- [docs/PINPAD-ARCHITECTURE.md](PINPAD-ARCHITECTURE.md) §6 - Full structure reference
- [PINPAD-IMPLEMENTATION-PLAN.md](../PINPAD-IMPLEMENTATION-PLAN.md) - Data structures

**External References:**
- [CCID Rev 1.1 §6.1.11](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf) - PIN Verification Data Structure
- [PC/SC Part 10 v2.02.09](https://pcscworkgroup.com/Download/Specifications/pcsc10_v2.02.09.pdf) - IFDs with Secure PIN Entry

**Dependencies:** M4 (descriptor must advertise PIN support first)

---

### M6: Touchscreen PIN Entry UI Integration

**Objective:** Connect existing touchscreen UI modules to the CCID handler for PIN capture during secure entry.

**Duration:** ~2 days

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| Touch screen shows numeric keypad on PIN request | Visual verification |
| Touch events correctly detected and debounced | Test touches register correctly |
| PIN digits displayed as masked (`****`) | Visual verification |
| OK button triggers submission | Press OK, PIN captured |
| Cancel button aborts entry | Press Cancel, returns cancelled error |
| Timeout triggers abort | Wait timeout period, verify abort |
| PIN buffer cleared after use | Memory inspection or defmt log |

**Tasks:**
1. Wire `handle_secure()` to create `PinEntryContext`
2. Add UI mode switching in main loop (USB poll vs PIN entry)
3. Connect touch events to `PinEntryContext::add_digit()`
4. Implement timeout checking in main loop
5. Connect OK/Cancel buttons to state machine
6. Verify secure PIN buffer clearing

**Internal References:**
- [src/pinpad/ui.rs](../src/pinpad/ui.rs) - Touchscreen UI with embedded-graphics
- [src/pinpad/state.rs](../src/pinpad/state.rs) - State machine implementation
- [examples/display_touch.rs](../examples/display_touch.rs) - Display/touch init reference

**External References:**
- [embedded-graphics docs](https://docs.rs/embedded-graphics/) - UI rendering
- [STM32F469-DISCO User Manual](https://www.st.com/resource/en/user_manual/um1862-discovery-kit-with-stm32f469ni-mcu-stmicroelectronics.pdf) - Hardware details

**Dependencies:** M5 (handler must trigger UI)

---

## Phase 2: Integration

### M7: APDU Flow Integration

**Objective:** Complete the full flow from PIN capture to VERIFY APDU transmission to card.

**Duration:** ~2 days

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| VERIFY APDU constructed correctly | Compare with expected format |
| APDU sent to card via ISO 7816 | Card receives and processes |
| Card response returned to host | Response bytes match card output |
| Works with OpenPGP card VERIFY | `gpg --card-edit` authenticate works |
| Works with SeedKeeper VERIFY_PIN | `pysatochip` VERIFY_PIN works |

**Tasks:**
1. Connect `PinEntryContext` completion to `VerifyApduBuilder`
2. Implement APDU construction with entered PIN
3. Send APDU via `SmartcardDriver::transmit_apdu()`
4. Format response as `RDR_to_PC_DataBlock`
5. Handle both user PIN (P2=0x81) and admin PIN (P2=0x83)

**APDU Format (OpenPGP):**
```
CLA=00 INS=20 P1=00 P2=81 Lc=NN [PIN_BYTES]
      │       │     │     │     └─ PIN in ASCII (for bmFormatString=0x82)
      │       │     │     └─ Length of PIN
      │       │     └─ 0x81=User PIN, 0x83=Admin PIN
      │       └─ VERIFY instruction
      └─ Class byte
```

**Internal References:**
- [src/pinpad/apdu.rs](../src/pinpad/apdu.rs) - `VerifyApduBuilder` implementation
- [src/smartcard.rs](../src/smartcard.rs) - `transmit_apdu()` method
- [PROTOCOL_SEEDKEEPER.md](../PROTOCOL_SEEDKEEPER.md) - SeedKeeper protocol

**External References:**
- [OpenPGP Card Spec §7.2](https://www.gnupg.org/ftp/specs/OpenPGP-smart-card-application-3.4.1.pdf) - VERIFY command

**Dependencies:** M6 (PIN capture must work)

---

### M8: Error Handling and Edge Cases

**Objective:** Implement robust error handling for all PIN entry failure modes.

**Duration:** ~1 day

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| User cancel returns CCID_ERR_PIN_CANCELLED (0xEF) | Press Cancel, verify error code |
| Timeout returns CCID_ERR_PIN_TIMEOUT (0xF0) | Let timeout expire, verify error code |
| Invalid length returns proper error | Enter too few digits, verify error |
| Card error propagates correctly | Wrong PIN SW=63Cx returns to host |
| No crash on any error path | Fuzz test with invalid inputs |

**Tasks:**
1. Map `PinResult` enum to CCID error codes
2. Implement error response formatting
3. Add defensive validation in parsing
4. Test all error paths
5. Add defmt logging for debugging

**Error Code Mapping:**
| PinResult | CCID Error Code |
|-----------|-----------------|
| Success | 0x00 |
| Cancelled | 0xEF |
| Timeout | 0xF0 |
| InvalidLength | 0x07 (CMD_ABORTED) |

**Internal References:**
- [src/ccid.rs](../src/ccid.rs) - Error code constants
- [src/pinpad/mod.rs](../src/pinpad/mod.rs) - `PinResult` enum

**Dependencies:** M7 (basic flow must work)

---

### M9: Host-Side Testing and Validation

**Objective:** Validate the complete PIN entry flow with standard host software.

**Duration:** ~2 days

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| GnuPG card authenticate works | `gpg --card-edit` → admin → PIN entry |
| OpenSC tools work | `pkcs15-tool --verify-pin` succeeds |
| pysatochip works with PIN pad | Script triggers device PIN entry |
| pcsc_scan shows PIN capabilities | Output shows PIN support |
| No errors in pcscd logs | `/var/log/syslog` clean |

**Test Commands:**
```bash
# Check descriptor
lsusb -v -d <vid:pid> | grep -A10 "CCID"

# Test with pcsc_scan
pcsc_scan

# Test with OpenSC
pkcs15-tool --verify-pin --pin-padding 0

# Test with GnuPG
gpg --card-edit
> admin
> passwd
# Should trigger PIN entry on device

# Test with pysatochip
python3 test_pysatochip.py 1234 --reader STM32
```

**Internal References:**
- [tests/hardware/README.md](../tests/hardware/README.md) - Hardware test procedures
- [GEMALTO_KNOWN_GOOD_SUMMARY.md](../GEMALTO_KNOWN_GOOD_SUMMARY.md) - Reference implementation

**External References:**
- [OpenSC Wiki - Pinpad Readers](https://github.com/OpenSC/OpenSC/wiki/Pinpad-Readers)
- [GnuPG Card Edit Manual](https://www.gnupg.org/documentation/manuals/gnupg/Card-edit-Menu.html)

**Dependencies:** M8 (error handling must work)

---

## Phase 3: Advanced Features

### M10: Sign What You See (SWYS) Transaction Confirmation

**Objective:** Implement transaction confirmation display before PIN entry for secure signing workflows.

**Duration:** ~3 days

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| Transaction data displayed on touchscreen | Visual verification |
| User must confirm before PIN entry | Cannot skip confirmation |
| Confirm/Cancel buttons work | Test both paths |
| Works with Bitcoin signing workflow | Specter-DIY test |
| Transaction data matches what card signs | End-to-end verification |

**Tasks:**
1. Parse transaction data from `bNumberMessage`/`wLangId` fields
2. Design confirmation UI layout
3. Implement pre-PIN confirmation state
4. Add confirmation to state machine
5. Test with Specter-DIY or similar

**UI Flow:**
```
┌─────────────────────────────────────┐
│     Confirm Transaction            │
│                                     │
│  Amount:  0.001 BTC                │
│  To:      bc1q...                  │
│  Fee:     0.00001 BTC              │
│                                     │
│  Is this correct?                  │
│                                     │
│    [CANCEL]        [CONFIRM]       │
└─────────────────────────────────────┘
           │
           │ After CONFIRM
           ▼
┌─────────────────────────────────────┐
│  Enter PIN                         │
│  PIN: ****                         │
│  ┌───┐ ┌───┐ ┌───┐                │
│  │ 1 │ │ 2 │ │ 3 │                │
│  └───┘ └───┘ └───┘                │
│  ...                               │
└─────────────────────────────────────┘
```

**Internal References:**
- [docs/PINPAD-ARCHITECTURE.md](PINPAD-ARCHITECTURE.md) §5 - SWYS Flow

**External References:**
- [Specter-DIY](https://github.com/cryptoadvance/specter-diy) - Reference SWYS implementation
- [ZKA Secoder Spec](https://www.heckenbuechner-homberg.de/download/SECODER%20G2.pdf) - German banking standard

**Dependencies:** M9 (basic PIN entry must work)

---

### M11: PIN Modification Support

**Objective:** Implement PIN change functionality (CHANGE REFERENCE DATA).

**Duration:** ~2 days

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| `PC_to_RDR_Secure` with modification sub-command works | OpenPGP PIN change succeeds |
| Current PIN entry prompt shown | UI shows "Enter current PIN" |
| New PIN entry prompt shown | UI shows "Enter new PIN" |
| Confirm new PIN prompt shown | UI shows "Confirm new PIN" |
| PINs match validation works | Mismatch returns error |
| OpenPGP passwd command works | `gpg --card-edit` → passwd |

**Tasks:**
1. Parse PIN Modification Data Structure (CCID §6.1.12)
2. Add three-phase PIN entry state (current, new, confirm)
3. Implement PIN match validation
4. Construct CHANGE REFERENCE DATA APDU
5. Test with OpenPGP card

**APDU Format (OpenPGP PIN Change):**
```
CLA=00 INS=24 P1=00 P2=81 Lc=10 [OLD_PIN_8][NEW_PIN_8]
                      │         └─ Old and new PINs
                      └─ 0x81=User PIN, 0x83=Admin PIN
```

**Internal References:**
- [docs/PINPAD-ARCHITECTURE.md](PINPAD-ARCHITECTURE.md) §6 - PIN Modification structure

**External References:**
- [CCID Rev 1.1 §6.1.12](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf) - PIN Modification Data Structure

**Dependencies:** M9 (basic PIN entry must work)

---

### M12: Security Hardening

**Objective:** Implement security hardening measures for production deployment.

**Duration:** ~3 days

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| PIN buffer uses volatile memory | Compiler doesn't optimize away clears |
| PIN buffer cleared on scope exit | `Drop` implementation verified |
| No PIN data in defmt logs | Check log output |
| Touch coordinates not logged | Check log output |
| Constant-time PIN comparison implemented | Code review |
| Stack/heap clear on reset | Memory inspection |

**Tasks:**
1. Audit all PIN handling code for security issues
2. Implement `zeroize` or volatile clearing for PIN buffer
3. Add constant-time comparison for PIN validation
4. Remove any PIN-related logging
5. Implement secure display buffer clearing
6. Add memory barrier after PIN operations

**Security Checklist:**
- [ ] PIN never transmitted over USB
- [ ] PIN never logged
- [ ] PIN buffer in volatile memory only
- [ ] PIN buffer cleared immediately after use
- [ ] No PIN data in crash dumps
- [ ] Touch events isolated from host

**Internal References:**
- [docs/SECURITY_AND_BOOTLOADER.md](SECURITY_AND_BOOTLOADER.md) §9 - Secure PIN Entry
- [src/pinpad/mod.rs](../src/pinpad/mod.rs) - `secure_clear()` function

**External References:**
- [Common Criteria PP-0083](https://www.commoncriteriaportal.org/nfs/ccpfiles/files/ppfiles/pp0083b_pdf.pdf) - eID reader security requirements
- [PCI PTS v5.1](https://www.pcisecuritystandards.org/document_library/) - PIN security requirements

**Dependencies:** M11 (all PIN features must work)

---

## Phase 4: Certification Preparation

### M13: CCID Compliance Testing

**Objective:** Validate CCID specification compliance using standard test tools.

**Duration:** ~3 days

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| USB-IF CCID Gold Tree tests pass | Test suite output |
| PC/SC Workgroup tests pass | Test suite output |
| All mandatory CCID commands work | Command-by-command verification |
| Descriptor validation passes | `lsusb -v` + spec comparison |
| Interrupt notifications work | Slot change notifications received |

**Test Matrix:**
| Command | Code | Status |
|---------|------|--------|
| PC_to_RDR_IccPowerOn | 0x62 | Must Pass |
| PC_to_RDR_IccPowerOff | 0x63 | Must Pass |
| PC_to_RDR_GetSlotStatus | 0x65 | Must Pass |
| PC_to_RDR_XfrBlock | 0x6F | Must Pass |
| PC_to_RDR_GetParameters | 0x6C | Must Pass |
| PC_to_RDR_SetParameters | 0x61 | Must Pass |
| PC_to_RDR_Secure | 0x69 | Must Pass |
| RDR_to_PC_DataBlock | 0x80 | Must Pass |
| RDR_to_PC_SlotStatus | 0x81 | Must Pass |
| RDR_to_PC_NotifySlotChange | 0x50 | Must Pass |

**Internal References:**
- [VERIFICATION.md](../VERIFICATION.md) - Existing verification procedures

**External References:**
- [USB-IF Compliance Testing](https://www.usb.org/compliance)
- [PC/SC Workgroup](https://pcscworkgroup.com/)

**Dependencies:** M12 (all features complete)

---

### M14: Security Audit Preparation

**Objective:** Prepare documentation and code for security audit.

**Duration:** ~2 days

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| Threat model documented | Document exists and is complete |
| Security boundaries defined | Architecture diagram shows boundaries |
| All crypto operations documented | Code comments and docs exist |
| Known limitations documented | Limitations section exists |
| Test coverage for security paths | Tests exist and pass |

**Deliverables:**
1. Threat Model Document
2. Security Architecture Diagram
3. Cryptographic Operations Summary
4. Known Security Limitations
5. Test Coverage Report

**Internal References:**
- [docs/SECURITY_AND_BOOTLOADER.md](SECURITY_AND_BOOTLOADER.md) - Existing security documentation

**External References:**
- [OWASP IoT Top 10](https://owasp.org/www-project-iot-top-10/)
- [Common Criteria Documentation](https://www.commoncriteriaportal.org/)

**Dependencies:** M13 (compliance verified)

---

### M15: Release Documentation

**Objective:** Complete all user and developer documentation for release.

**Duration:** ~1 day

**Success Criteria:**
| Criteria | How to Verify |
|----------|---------------|
| User guide complete | New user can build and flash |
| API documentation complete | All public APIs documented |
| Build instructions accurate | Fresh clone builds successfully |
| Known issues documented | Issues list exists |
| Changelog updated | All changes recorded |

**Deliverables:**
1. README.md - Project overview and quick start
2. BUILDING.md - Detailed build instructions
3. USER_GUIDE.md - End-user documentation
4. API.md - Developer API reference
5. CHANGELOG.md - Version history
6. KNOWN_ISSUES.md - Current limitations

**Dependencies:** M14 (audit preparation complete)

---

## Milestone Dependencies Graph

```
M0-M3 (Complete)
    │
    ▼
   M4 ──────────────────┐
    │                    │
    ▼                    │
   M5 ◄──────────────────┘
    │
    ▼
   M6
    │
    ▼
   M7
    │
    ▼
   M8
    │
    ▼
   M9
    │
    ├──────────────────┐
    ▼                  ▼
  M10                 M11
    │                  │
    └────────┬─────────┘
             ▼
            M12
             │
             ▼
            M13
             │
             ▼
            M14
             │
             ▼
            M15
```

---

## Resource Summary

| Phase | Milestones | Duration | Complexity |
|-------|------------|----------|------------|
| **Phase 1: Foundation** | M4-M6 | ~7 days | Medium |
| **Phase 2: Integration** | M7-M9 | ~5 days | Medium |
| **Phase 3: Advanced** | M10-M12 | ~8 days | High |
| **Phase 4: Certification** | M13-M15 | ~6 days | Medium |
| **TOTAL** | 12 milestones | ~26 days | - |

---

## Key External References

### Specifications
- [CCID Rev 1.1](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf) - Primary CCID specification
- [PC/SC Part 10 v2.02.09](https://pcscworkgroup.com/Download/Specifications/pcsc10_v2.02.09.pdf) - Secure PIN entry
- [ISO 7816-3](https://www.iso.org/standard/38770.html) - Smart card protocol
- [OpenPGP Card Spec 3.4](https://www.gnupg.org/ftp/specs/OpenPGP-smart-card-application-3.4.1.pdf) - OpenPGP card

### Security Standards
- [Common Criteria PP-0083](https://www.commoncriteriaportal.org/nfs/ccpfiles/files/ppfiles/pp0083b_pdf.pdf) - eID reader protection profile
- [PCI PTS v5.1](https://www.pcisecuritystandards.org/document_library/) - PIN transaction security
- [FIPS 140-3](https://csrc.nist.gov/publications/detail/fips/140/3/final) - Cryptographic module validation

### Reference Implementations
- [osmo-ccid-firmware](https://github.com/osmocom/osmo-ccid-firmware) - Production CCID firmware
- [Specter-DIY](https://github.com/cryptoadvance/specter-diy) - Bitcoin hardware wallet with SWYS
- [OpenSC](https://github.com/OpenSC/OpenSC) - Host middleware with PIN pad support
- [Ludovic Rousseau's CCID driver](https://github.com/LudovicRousseau/ccid) - Linux CCID driver

### Hardware Documentation
- [STM32F469 Reference Manual RM0386](https://www.st.com/resource/en/reference_manual/rm0386.pdf)
- [STM32F469-DISCO User Manual UM1862](https://www.st.com/resource/en/user_manual/um1862-discovery-kit-with-stm32f469ni-mcu-stmicroelectronics.pdf)
- [FT6X06 Touch Controller Datasheet](https://www.displayfuture.com/Display/datasheet/controller/FT6x06.pdf)

---

## Changelog

| Date | Author | Changes |
|------|--------|---------|
| 2026-03-11 | AI Research | Initial milestone document created |

---

## Next Steps

When ready to begin implementation:

1. **For Prometheus Planning:** Provide this document as context. Each milestone has:
   - Clear objective
   - Success criteria with verification methods
   - Task breakdown
   - Internal documentation links
   - External specification references

2. **Start with M4:** CCID Descriptor Update is the first milestone with no dependencies.

3. **Maintain Session Continuity:** When delegating milestone implementation to agents, use session_id to preserve context.
