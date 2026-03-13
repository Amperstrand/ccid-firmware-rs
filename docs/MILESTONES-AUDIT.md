# MILESTONES Implementation Status Audit
Date: 2026-03-12

## Summary

This audit examines the actual implementation status of milestones M4-M15 based on source code analysis. The findings reveal that significant portions of the PIN pad infrastructure have been implemented, but integration with the CCID protocol layer varies by milestone.

**Overall Status:** ~60% complete across all milestones

| Phase | Milestones | Avg Completion |
|-------|------------|----------------|
| Phase 1: Foundation | M4-M6 | ~75% |
| Phase 2: Integration | M7-M9 | ~70% |
| Phase 3: Advanced | M10-M12 | ~35% |
| Phase 4: Certification | M13-M15 | 0% |

---

## Detailed Findings

### M4: CCID Descriptor Update for PIN Support
- **Status:** PARTIAL
- **Evidence:**
  - `src/device_profile.rs:73-77`: PIN support constants defined (`PIN_VERIFY = 0x01`, `PIN_MODIFY = 0x02`, `PIN_VERIFY_MODIFY = 0x03`)
  - `src/device_profile.rs:168-171`: `lcd_layout` and `pin_support` fields exist in `DeviceProfile` struct
  - `src/device_profile.rs:269-274`: Descriptor generation includes `wLcdLayout` and `bPINSupport`
  - **Cherry ST-2100 (default profile):** Lines 358-359: `lcd_layout: (0, 0)`, `pin_support: 0x00` — **DISABLED**
  - **Gemalto K30 profile:** Lines 476-477: `lcd_layout: (16, 16)`, `pin_support: PIN_VERIFY_MODIFY` — **ENABLED**
  - **dwFeatures:** Line 344-348: Does NOT include `FEAT_LCD` (0x0010_0000) or `FEAT_PIN_PAD` (0x0020_0000) bits
- **Notes:**
  - The descriptor structure supports PIN/display fields, but the default profile has them disabled
  - The Gemalto K30 profile has proper PIN support values but is not the default
  - **Missing:** `dwFeatures` does not advertise LCD (bit 20) or PIN pad (bit 21) capabilities
  - **Recommendation:** Add `FEAT_LCD | FEAT_PIN_PAD` to features for PIN-enabled profiles

---

### M5: PC_to_RDR_Secure Handler Implementation
- **Status:** COMPLETE
- **Evidence:**
  - `src/ccid.rs:902-950`: `handle_secure()` method implemented
  - Lines 920-939: Parses PIN Verify Data Structure using `PinVerifyParams::parse()`
  - Lines 927-930: Stores params in `SecureState::WaitingForPin` for deferred response
  - Lines 940-944: PIN Modify (0x01) returns CMD_NOT_SUPPORTED (intentionally stubbed)
  - `src/pinpad/mod.rs:83-157`: `PinVerifyParams::parse()` correctly parses CCID structure
- **Notes:**
  - PIN Verification is fully implemented
  - PIN Modification is intentionally stubbed (returns CMD_NOT_SUPPORTED)
  - Error handling for malformed requests is in place
  - Integration point for UI trigger exists via `SecureState::WaitingForPin`

---

### M6: Touchscreen PIN Entry UI Integration
- **Status:** COMPLETE
- **Evidence:**
  - `src/main.rs:111-121`: `AppMode` enum with `Normal` and `PinEntry` variants
  - `src/main.rs:534-556`: Main loop detects `is_pin_entry_active()` and transitions to PIN mode
  - `src/main.rs:558-632`: PIN entry mode handles touch events, button presses, timeout
  - `src/main.rs:589-610`: Button press handling for digits, backspace, OK, Cancel
  - `src/main.rs:614-616`: Timeout checking via `context.check_timeout()`
  - `src/pinpad/ui.rs:110-222`: `Keypad` struct with 12 buttons (0-9, OK, Cancel)
  - `src/pinpad/ui.rs:365-413`: `TouchHandler` for touch event processing
  - `src/pinpad/state.rs:59-168`: `PinEntryContext` state machine with timeout support
- **Notes:**
  - Touch events are correctly wired to PinEntryContext
  - Timeout checking is implemented
  - UI is rendered with embedded-graphics on each frame
  - PIN displayed as masked (`****`)

---

### M7: APDU Flow Integration
- **Status:** COMPLETE
- **Evidence:**
  - `src/ccid.rs:979-1041`: `complete_pin_entry()` sends `RDR_to_PC_DataBlock` response
  - `src/ccid.rs:1059-1119`: `process_pin_result()` builds APDU and transmits to card
  - Lines 1073-1078: Uses `VerifyApduBuilder::from_template()` with params from CCID
  - Lines 1083-1097: Calls `driver.transmit_apdu()` to send to card
  - `src/pinpad/apdu.rs:63-105`: `VerifyApduBuilder::build()` constructs VERIFY APDU
  - `src/ccid.rs:1216-1255`: `send_data_block_response()` formats CCID response
- **Notes:**
  - Full flow from PIN capture to VERIFY APDU to card response is implemented
  - Uses `VerifyApduBuilder` correctly
  - Response formatted as `RDR_to_PC_DataBlock`
  - Both user PIN (P2=0x81) and admin PIN (P2=0x83) supported via template

---

### M8: Error Handling and Edge Cases
- **Status:** COMPLETE
- **Evidence:**
  - `src/ccid.rs:120-122`: CCID error codes defined
    - `CCID_ERR_PIN_CANCELLED = 0xEF`
    - `CCID_ERR_PIN_TIMEOUT = 0xF0`
  - `src/ccid.rs:1004-1037`: `complete_pin_entry()` maps PinResult to CCID errors:
    - `PinResult::Success` → `COMMAND_STATUS_NO_ERROR`, error 0
    - `PinResult::Cancelled` → `COMMAND_STATUS_FAILED`, `CCID_ERR_PIN_CANCELLED (0xEF)`
    - `PinResult::Timeout` → `COMMAND_STATUS_FAILED`, `CCID_ERR_PIN_TIMEOUT (0xF0)`
    - `PinResult::InvalidLength` → `COMMAND_STATUS_FAILED`, `CCID_ERR_CMD_ABORTED`
  - `src/pinpad/mod.rs:28-39`: `PinResult` enum defined
  - `src/pinpad/state.rs:83-96`: Validation for min PIN length
- **Notes:**
  - All error codes correctly mapped
  - Edge cases handled (invalid length, timeout, cancel)
  - Card errors propagate correctly via APDU response

---

### M9: Host-Side Testing and Validation
- **Status:** PARTIAL
- **Evidence:**
  - `tests/hardware/README.md`: Documents manual test procedures
  - `tests/hardware/seedkeeper_non_destructive.py`: Basic read-only test script
  - `tests/hardware/sysmocom_sim_non_destructive.py`: Basic read-only test script
  - **Missing:** No PIN pad specific test scripts
  - **Missing:** No pcscd integration tests for PIN entry
- **Notes:**
  - Hardware test infrastructure exists but is basic
  - Tests are read-only and do not exercise PIN entry flow
  - No validation documentation for GnuPG, OpenSC, or pysatochip PIN pad
  - **Recommendation:** Create PIN pad specific test script

---

### M10: SWYS Transaction Confirmation
- **Status:** NOT STARTED
- **Evidence:**
  - No code found for transaction data display
  - No pre-PIN confirmation state in `PinEntryState` enum
  - `src/pinpad/state.rs:15-29`: States are Idle, WaitingForPin, Completed, Cancelled, Timeout, InvalidLength — no Confirmation state
- **Notes:**
  - Would require parsing transaction data from `bNumberMessage`/`wLangId` fields
  - Would require new UI screen for confirmation
  - Would require state machine modification

---

### M11: PIN Modification Support
- **Status:** NOT STARTED
- **Evidence:**
  - `src/ccid.rs:940-944`: PIN Modify explicitly returns `CCID_ERR_CMD_NOT_SUPPORTED`
  - No PIN Modification Data Structure parsing
  - No CHANGE REFERENCE DATA APDU construction
  - No three-phase PIN entry (current, new, confirm)
- **Notes:**
  - Handler exists but is stubbed
  - Would require parsing CCID §6.1.12 structure
  - Would require UI for three-phase entry

---

### M12: Security Hardening
- **Status:** PARTIAL
- **Evidence:**
  - **Volatile clearing:** `src/pinpad/mod.rs:201-207, 259-268`: Uses `core::ptr::write_volatile` for secure clearing
  - **Drop implementation:** `src/pinpad/mod.rs:259-268`: `PinBuffer` implements `Drop` for auto-clearing
  - **secure_clear function:** `src/pinpad/mod.rs:271-277`: Available for manual clearing
  - **Missing:** No `zeroize` crate (not in Cargo.toml)
  - **Missing:** No constant-time comparison (not needed for PIN pad entry — PIN is sent to card, not compared locally)
  - **Logging:** No explicit PIN exclusion from defmt logs found, but PIN values not logged in main.rs/ccid.rs
- **Notes:**
  - Volatile clearing is correctly implemented
  - Drop trait ensures cleanup on scope exit
  - No zeroize dependency (manual implementation is acceptable)
  - Touch coordinates are not logged (verified in main.rs)
  - **Recommendation:** Add explicit comment that PIN values must never be logged

---

### M13: CCID Compliance Testing
- **Status:** NOT STARTED
- **Evidence:**
  - No USB-IF CCID Gold Tree tests
  - No PC/SC Workgroup tests
  - No compliance test documentation
- **Notes:**
  - Would require external test tools
  - Would require documentation of test results

---

### M14: Security Audit Preparation
- **Status:** NOT STARTED
- **Evidence:**
  - No threat model document
  - No security architecture diagram
  - No known limitations document
  - `docs/SECURITY_AND_BOOTLOADER.md` exists but covers bootloader, not PIN pad security specifically
- **Notes:**
  - Would require dedicated security documentation
  - Would require formal threat modeling

---

### M15: Release Documentation
- **Status:** NOT STARTED
- **Evidence:**
  - `README.md` exists (project overview)
  - No `USER_GUIDE.md`
  - No `API.md`
  - No `CHANGELOG.md`
  - No `KNOWN_ISSUES.md`
- **Notes:**
  - Documentation infrastructure not in place
  - Would require comprehensive documentation effort

---

## Recommendations

### High Priority (Required for Basic PIN Pad Operation)

1. **M4 Fix:** Add `FEAT_LCD | FEAT_PIN_PAD` to `dwFeatures` for PIN-enabled profiles
   - File: `src/device_profile.rs`
   - Lines: ~344-348 for Cherry, ~461-466 for Gemalto K30

2. **M4 Fix:** Enable PIN support in default profile or document profile selection requirement
   - Default Cherry profile has `pin_support: 0x00`
   - Users must use `--no-default-features --features profile-gemalto-pinpad` for PIN functionality

3. **M9:** Create PIN pad integration test script
   - Test `pcsc_scan` shows PIN capabilities
   - Test OpenSC `pkcs15-tool --verify-pin`
   - Test GnuPG card authenticate

### Medium Priority (Advanced Features)

4. **M10:** Implement SWYS transaction confirmation
   - Add `Confirmation` state to `PinEntryState`
   - Parse transaction data from CCID fields
   - Create confirmation UI screen

5. **M11:** Implement PIN Modification
   - Parse PIN Modification Data Structure (CCID §6.1.12)
   - Add three-phase PIN entry state machine
   - Construct CHANGE REFERENCE DATA APDU

### Low Priority (Certification)

6. **M12 Enhancement:** Add explicit security logging policy
   - Add comments documenting PIN data must never be logged
   - Consider adding compile-time assertions

7. **M13-M15:** Certification preparation
   - Create compliance test suite
   - Write threat model documentation
   - Complete user/developer documentation

---

## Test Coverage Summary

| Component | Unit Tests | Integration Tests | Hardware Tests |
|-----------|------------|-------------------|----------------|
| PinVerifyParams parsing | ✅ Yes | - | - |
| PinEntryContext state machine | ✅ Yes | - | - |
| VerifyApduBuilder | ✅ Yes | - | - |
| Keypad/TouchHandler | ✅ Yes | - | - |
| CCID Secure handler | - | - | ❌ No |
| Full PIN entry flow | - | - | ❌ No |

---

## Code Quality Notes

1. **Well-structured modules:** `pinpad/` directory has clear separation (mod.rs, state.rs, ui.rs, apdu.rs)
2. **Comprehensive unit tests:** State machine and APDU builder have good test coverage
3. **Defensive programming:** Error handling is thorough throughout
4. **Feature gating:** Display/PIN functionality properly gated behind `display` feature
5. **Documentation:** Code comments and doc strings are present and helpful

---

## Appendix: File Reference Summary

| Milestone | Key Files | Key Lines |
|-----------|-----------|-----------|
| M4 | `src/device_profile.rs` | 73-77, 168-171, 269-274, 358-359, 476-477 |
| M5 | `src/ccid.rs`, `src/pinpad/mod.rs` | 902-950, 83-157 |
| M6 | `src/main.rs`, `src/pinpad/state.rs`, `src/pinpad/ui.rs` | 111-121, 534-632, 59-168, 110-222 |
| M7 | `src/ccid.rs`, `src/pinpad/apdu.rs` | 979-1119, 1216-1255, 63-105 |
| M8 | `src/ccid.rs`, `src/pinpad/mod.rs` | 120-122, 979-1041, 28-39 |
| M9 | `tests/hardware/` | README.md, *.py |
| M10 | - | Not implemented |
| M11 | `src/ccid.rs` | 940-944 (stub only) |
| M12 | `src/pinpad/mod.rs` | 201-207, 259-277 |
| M13-M15 | - | Not started |
