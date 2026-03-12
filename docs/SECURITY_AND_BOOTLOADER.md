# Security Architecture and Bootloader Integration

This document consolidates research on security features, firmware update mechanisms, and architecture decisions for the CCID smartcard reader firmware on STM32F469-DISCO.

---

## Table of Contents

1. [Architecture Decision: Sync vs Async](#1-architecture-decision-sync-vs-async)
2. [STM32F469 Security Features](#2-stm32f469-security-features)
3. [Read Out Protection (RDP) Levels](#3-read-out-protection-rdp-levels)
4. [Write Protection](#4-write-protection)
5. [Secure Boot Process](#5-secure-boot-process)
6. [Bootloader Integration Pattern (Specter-DIY)](#6-bootloader-integration-pattern-specter-diy)
7. [Signed Firmware Update Flow](#7-signed-firmware-update-flow)
8. [Implementation Roadmap](#8-implementation-roadmap)

---

## 1. Architecture Decision: Sync vs Async

### Overview

For embedded firmware like a CCID smartcard reader, the choice between synchronous and asynchronous architecture significantly impacts complexity, reliability, and power consumption.

### Synchronous Architecture (Recommended for CCID Reader)

**Characteristics:**
- Sequential execution with blocking operations
- Simple control flow - one thing at a time
- Deterministic timing and behavior
- Easier to reason about and debug

**Advantages for CCID Reader:**
```
USB CCID Request → Block until complete → Process next request
     ↓
  No race conditions between:
  - USB stack
  - Smartcard transactions
  - Touchscreen input
  - Display updates
```

**When to choose sync:**
- Single well-defined protocol (CCID)
- Predictable response times required
- Limited concurrent operations
- Power consumption matters (no busy-waiting)
- Simpler certification/audit path

### Asynchronous Architecture

**Characteristics:**
- Non-blocking operations with callbacks/futures
- Multiple operations in flight
- Event-driven state machines
- Higher complexity, more potential race conditions

**When async makes sense:**
- Multiple independent communication channels
- Latency-sensitive operations that can overlap
- Background tasks (logging, diagnostics)
- Complex UI with continuous updates

### Recommendation

For the CCID smartcard reader with pinpad:

| Feature | Architecture | Rationale |
|---------|-------------|-----------|
| USB CCID handling | **Sync** | Sequential APDU processing is natural |
| Smartcard communication | **Sync** | Transaction boundaries are clear |
| Touchscreen input | **Sync** | Pin entry is sequential, blocking |
| Display updates | **Sync** | UI state follows card/USB state |

**Verdict:** Use synchronous architecture with embassy-executor. The STM32F469 has sufficient performance to handle all operations sequentially without blocking user experience.

---

## 2. STM32F469 Security Features

### Hardware Security Capabilities

The STM32F469NI microcontroller provides several hardware-enforced security mechanisms:

| Feature | Purpose | Hardware Support |
|---------|---------|------------------|
| **RDP (Read Out Protection)** | Prevent flash extraction | Option bytes |
| **WRP (Write Protection)** | Prevent accidental/deliberate flash modification | Option bytes per sector |
| **PCROP (Proprietary Code Read Out Protection)** | Protect specific code regions | Option bytes |
| **BOR (Brown-out Reset)** | Detect power manipulation | Programmable thresholds |
| **Watchdog (IWDG/WWDG)** | Detect code execution anomalies | Independent/Window watchdogs |
| **CRC unit** | Firmware integrity verification | Hardware CRC-32 |
| **True Random Number Generator (TRNG)** | Secure key generation | Hardware entropy source |
| **Hash processor (HASH)** | SHA-1/SHA-256 acceleration | DMA-capable |

### Memory Organization for Security

```
STM32F469NI Flash Memory (2 MB total)

Sector  0: 0x0800_0000 - 0x0800_3FFF  (16 KB)  → Startup/Bootloader Stage 1
Sector  1: 0x0800_4000 - 0x0800_7FFF  (16 KB)  → Key Storage
Sector  2: 0x0800_8000 - 0x0800_BFFF  (16 KB)  → Internal Filesystem
Sector  3: 0x0800_C000 - 0x0800_FFFF  (16 KB)  ↑
Sector  4: 0x0801_0000 - 0x0801_FFFF  (64 KB)  ↓
Sectors 5-21: 0x0802_0000+             → Main Firmware (~1.6 MB)
Sector 22: 0x081C_0000 - 0x081D_FFFF  (128 KB) → Bootloader Copy 1
Sector 23: 0x081E_0000 - 0x081F_FFFF  (128 KB) → Bootloader Copy 2
```

---

## 3. Read Out Protection (RDP) Levels

The STM32F469 supports three RDP levels with progressively stronger protection:

### RDP Level 0 - No Protection

| Property | Value |
|----------|-------|
| Flash access | Full read/write via JTAG/SWD |
| Boot RAM | Accessible |
| Option bytes | Modifiable |
| Use case | Development, debugging |

```
┌─────────────────────────────────────┐
│  RDP Level 0 - Development Mode    │
├─────────────────────────────────────┤
│  JTAG/SWD:  ✓ Full access          │
│  Flash:     ✓ Read/Write/Erase     │
│  Security:  ✗ None                  │
└─────────────────────────────────────┘
```

### RDP Level 1 - Memory Protection

| Property | Value |
|----------|-------|
| Flash access | **Blocked** via JTAG/SWD |
| Boot RAM | Blocked |
| Option bytes | Readable, level changeable |
| Transition to Level 0 | **Mass erase triggered** |
| Use case | Production devices |

```
┌─────────────────────────────────────┐
│  RDP Level 1 - Production Mode     │
├─────────────────────────────────────┤
│  JTAG/SWD:  ✗ Blocked               │
│  Flash:     ✗ No direct access      │
│  Downgrade: ⚠ Triggers mass erase  │
│  Security:  ✓ Memory protected      │
└─────────────────────────────────────┘
```

**Important:** Transitioning from Level 1 → Level 0 triggers an automatic mass erase, destroying all firmware and data. This prevents attackers from extracting firmware by downgrading protection.

### RDP Level 2 - Permanent Protection (IRREVERSIBLE)

| Property | Value |
|----------|-------|
| JTAG/SWD | **Permanently disabled** |
| Boot from RAM | Disabled |
| Option bytes | Locked forever |
| Transition | **Not possible** |
| Use case | High-security final products |

```
┌─────────────────────────────────────────────┐
│  RDP Level 2 - Permanent Lock              │
├─────────────────────────────────────────────┤
│  JTAG/SWD:    ✗ Permanently disabled        │
│  Flash:       ✗ No external access ever     │
│  Downgrade:   ✗ Impossible                 │
│  Debug:       ✗ Forever impossible          │
│  Recovery:    ✗ Brick if firmware broken    │
└─────────────────────────────────────────────┘
```

⚠️ **CRITICAL WARNING:** RDP Level 2 is **IRREVERSIBLE**. Once programmed:
- The device can never be debugged again
- Firmware updates must work flawlessly (no recovery via JTAG)
- A buggy firmware = bricked device
- Use only after extensive testing with Level 1

### RDP Level Selection Guide

| Stage | Recommended RDP | Rationale |
|-------|----------------|-----------|
| Development | Level 0 | Full debugging access |
| Alpha testing | Level 0 | Debug firmware issues |
| Beta testing | Level 1 | Test protection, allow recovery |
| Production pilot | Level 1 | Allow field updates and recovery |
| Production final | Level 2 (optional) | Maximum security, no recovery |

---

## 4. Write Protection

### Purpose

Write protection prevents unauthorized modification of flash sectors:
- Protects bootloader from accidental corruption
- Prevents malware from persisting in flash
- Guards critical configuration sectors

### How It Works

Each flash sector can be individually write-protected via option bytes:

```
Write Protection States:

Not Protected (WRP = 1):
  - Sector can be erased and programmed normally
  
Protected (WRP = 0):
  - Sector cannot be erased or programmed
  - Attempts cause hardware fault
  - Protection can be removed by changing option bytes
```

### Specter-DIY Write Protection Pattern

The Specter bootloader uses this approach:

```c
// During firmware update:
// 1. Temporarily disable write protection for target sector
blsys_flash_write_protect(sector, false);

// 2. Erase and program the sector
flash_erase(sector);
flash_program(sector, data, size);

// 3. Restore write protection if enabled globally
if (WRITE_PROTECTION_ENABLED) {
    blsys_flash_write_protect(sector, true);
}
```

### Recommended Protection Map

```
STM32F469 Flash Layout with Protection:

Sector  0 (Startup):     WP=1, RDP-protected
Sector  1 (Key Storage): WP=1, RDP-protected  
Sectors 2-4 (FS):        WP=0 (modifiable by firmware)
Sectors 5-21 (Main):     WP=0 (updatable by bootloader)
Sector 22 (Bootloader):  WP=1 (only update via bootloader)
Sector 23 (Bootloader):  WP=1 (only update via bootloader)
```

---

## 5. Secure Boot Process

### Boot Chain Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    Power-On Reset                           │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  Stage 1: Startup Code (Non-upgradable)                    │
│  • Located at 0x0800_0000 (Sector 0)                       │
│  • Executes first, cannot be replaced                       │
│  • Verifies bootloader integrity (CRC check)               │
│  • Selects latest valid bootloader copy                     │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  Stage 2: Bootloader (Upgradable, Signed)                  │
│  • Two redundant copies for reliability                     │
│  • Checks for firmware upgrade files on SD card            │
│  • Verifies firmware signatures (ECDSA secp256k1)          │
│  • Enforces version downgrade protection                    │
│  • Validates main firmware integrity before boot           │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  Stage 3: Main Firmware                                    │
│  • CCID reader application                                 │
│  • Pinpad UI handling                                       │
│  • Smartcard communication                                  │
└─────────────────────────────────────────────────────────────┘
```

### Startup Code Responsibilities

The non-upgradable startup code (Specter pattern):

1. **Read integrity check records** from both bootloader copy sectors
2. **Validate each copy:**
   - Verify integrity check record CRC
   - Verify bootloader code CRC matches record
3. **Select bootloader:**
   - Choose valid copy with highest version
   - If both valid and same version, prefer copy 1
4. **Handle failure:**
   - If no valid copy found → infinite loop with LED indication
5. **Transfer control:**
   - Remap interrupt vectors
   - Branch to bootloader entry point

### Bootloader Responsibilities

1. **Check for upgrade:**
   - Mount SD card (FAT32)
   - Find upgrade file matching pattern
   - Verify exactly one matching file exists

2. **Validate upgrade file:**
   - Parse section headers
   - Verify CRC of each section
   - Check version is newer than current
   - Verify platform compatibility

3. **Verify signatures:**
   - Extract fingerprint-signature pairs
   - Match fingerprints to known public keys
   - Verify minimum signature threshold met
   - Perform ECDSA verification

4. **Apply upgrade:**
   - Erase target flash sectors
   - Copy payload from SD to flash
   - Re-verify signatures against flash data (not SD)
   - Write integrity check records

5. **Normal boot:**
   - Verify main firmware integrity
   - Transfer control to main firmware

---

## 6. Bootloader Integration Pattern (Specter-DIY)

### Repository Structure

Specter-DIY integrates the bootloader as part of the main repository:

```
specter-diy/
├── bootloader/              # Bootloader code (not a submodule!)
│   ├── platforms/
│   │   └── stm32f469disco/
│   │       ├── startup/     # Stage 1 - non-upgradable
│   │       └── bootloader/  # Stage 2 - upgradable
│   ├── keys/
│   │   ├── test/           # Known test keys
│   │   ├── selfsigned/     # Your custom keys
│   │   └── production/     # Vendor production keys
│   ├── tools/
│   │   ├── make-initial-firmware.py
│   │   └── upgrade-generator.py
│   └── doc/
└── src/                    # Main firmware
```

### Key Hierarchy

```
┌─────────────────────────────────────────────────────────────┐
│                    Key Hierarchy                            │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  Vendor Keys (Highest Privilege)                           │
│  ├── Can sign: Bootloader + Main Firmware                  │
│  ├── Stored in: Bootloader flash                           │
│  └── Managed by: Device manufacturer                       │
│                                                             │
│  Maintainer Keys (Limited Privilege)                       │
│  ├── Can sign: Main Firmware ONLY                          │
│  ├── Cannot sign: Bootloader                               │
│  └── Managed by: Authorized developers                     │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### Multisignature Support

The bootloader supports configurable signature thresholds:

```c
// Configuration in bootloader
#define BOOTLOADER_MIN_SIGS  2  // Requires 2 vendor signatures
#define FIRMWARE_MIN_SIGS    1  // Requires 1 signature (vendor OR maintainer)

// During verification:
// 1. Count valid signatures matching known public keys
// 2. Check if count >= threshold
// 3. Reject if insufficient signatures
```

**Use cases:**
- **1-of-1:** Single developer, self-signed
- **2-of-3:** Requires 2 of 3 authorized signers
- **1-of-2:** Vendor OR maintainer can release firmware

### Memory Layout (STM32F469NI)

```
Address         Size      Content                 Protection
─────────────────────────────────────────────────────────────
0x0800_0000     16 KB     Startup Code            RDP+WRP
0x0800_4000     16 KB     Key Storage             RDP+WRP
0x0800_8000     96 KB     Internal Filesystem     None
0x0802_0000     1664 KB   Main Firmware           Updatable
0x081C_0000     128 KB    Bootloader Copy 1       RDP+WRP
0x081E_0000     128 KB    Bootloader Copy 2       RDP+WRP
─────────────────────────────────────────────────────────────
Total:          2048 KB   (2 MB flash)
```

---

## 7. Signed Firmware Update Flow

### Complete Update Process

```
┌─────────────────────────────────────────────────────────────┐
│               DEVELOPER WORKSTATION                         │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  Step 1: Compile Firmware                                  │
│  ─────────────────────────                                 │
│  $ make disco USE_DBOOT=1                                  │
│  → Produces: specter-diy.hex                               │
│                                                             │
│  Step 2: Generate Upgrade File                             │
│  ─────────────────────────────────                         │
│  $ python3 upgrade-generator.py gen \                      │
│      -f specter-diy.hex \                                  │
│      -p stm32f469disco \                                   │
│      specter_upgrade.bin                                   │
│  → Produces: unsigned specter_upgrade.bin                  │
│                                                             │
│  Step 3: Get Message to Sign                               │
│  ─────────────────────────────                             │
│  $ python3 upgrade-generator.py message specter_upgrade.bin│
│  → Output: 1.4.0-1sujn22lsgatcpyesj9v8lf4zts6myds0...     │
│            └─ version └─ Bech32-encoded firmware hash      │
│                                                             │
│  Step 4: Sign with Hardware Wallet (Air-Gapped)           │
│  ─────────────────────────────────────────────             │
│  • Transfer Bech32 message to signing device               │
│  • Sign using Bitcoin message signing protocol             │
│  • Export signature in base64 format                       │
│                                                             │
│  Step 5: Import Signatures                                 │
│  ───────────────────────────                               │
│  $ python3 upgrade-generator.py import-sig \               │
│      -s "BASE64_SIGNATURE..." \                            │
│      specter_upgrade.bin                                   │
│  → Repeat for each required signature                      │
│                                                             │
│  Step 6: Deploy                                            │
│  ─────────────                                             │
│  • Copy specter_upgrade.bin to SD card                     │
│  • Insert SD card into device                              │
│  • Reboot device                                           │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│                    DEVICE BOOT                              │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  Step 7: Bootloader Detects Upgrade                        │
│  ─────────────────────────────────────                     │
│  • Mount SD card                                           │
│  • Find specter_upgrade*.bin                               │
│  • Parse section headers                                   │
│                                                             │
│  Step 8: Validate Upgrade                                  │
│  ───────────────────────────                               │
│  • Verify CRC of all sections                              │
│  • Check version > current version                         │
│  • Verify platform compatibility                           │
│                                                             │
│  Step 9: Verify Signatures                                 │
│  ─────────────────────────────                             │
│  • Extract fingerprint-signature pairs                     │
│  • Match fingerprints to known public keys                 │
│  • Verify minimum signature threshold                      │
│  • Perform ECDSA verification                              │
│                                                             │
│  Step 10: Apply Update                                     │
│  ───────────────────────────                               │
│  • Erase target flash sectors                              │
│  • Copy payload from SD to flash                           │
│  • Re-verify signatures against flash data                 │
│  • Write integrity check records                           │
│  • Unmount SD card                                         │
│  • Reboot                                                  │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### Signature Algorithm (secp256k1-sha256)

```
Message Construction:
─────────────────────

1. Calculate per-section hash:
   h_i = SHA-256(header_i || payload_i)

2. Combine section hashes:
   combined = SHA-256(h_0 || h_1 || ... || h_n)

3. Map to 5-bit values for Bech32:
   data = MAP_5BIT(combined)

4. Create human-readable part:
   hrp = BRIEF(section_name) || version || "-"
   Example: "b1.22.134rc5-2.0.1-"

5. Create Bech32 message:
   M = Bech32(hrp, data)

6. Format for Bitcoin message signing:
   m = 0x18 || "Bitcoin Signed Message:\n" || COMPACT_ENCODE(LEN(M)) || M
   
7. Double SHA-256:
   mh = SHA-256(SHA-256(m))

8. Sign with private key:
   signature = SECP256K1_SIGN(private_key, mh)

Signature Format:
─────────────────
Each signature: 80 bytes total
  - Fingerprint: 16 bytes (first 16 bytes of SHA-256(uncompressed_pubkey))
  - Signature:   64 bytes (compact ECDSA signature)
```

### Version Format

Semantic versioning encoded in 32-bit integer:

```
Version: MAJOR.MINOR.PATCH[-rcREVISION]

Encoding:
  MAJOR:   (0-41)   × 10^8
  MINOR:   (0-999)  × 10^5
  PATCH:   (0-999)  × 10^2
  REVISION: 0-98 (rc), 99 (stable)

Examples:
  "1.22.134-rc5" → 0102213405 (0x617A71D)
  "1.4.0"        → 0104000999 (stable, revision=99)
  "12.0.15"      → 1200001599

Maximum version: 41.999.999 → 4199999999
```

---

## 8. Implementation Roadmap

### Phase 1: Basic Secure Bootloader (Weeks 1-2)

```
Goals:
├── Create startup code that verifies and boots main firmware
├── Implement basic CRC integrity checking
├── Support firmware update via SD card (unsigned initially)
└── Test recovery mechanisms

Tasks:
1. Port Specter startup code structure
2. Implement flash memory map for STM32F469
3. Create integrity check record format
4. Add SD card filesystem support (FAT32)
5. Implement firmware versioning
6. Test: Normal boot, update, rollback
```

### Phase 2: Signed Firmware Updates (Weeks 3-4)

```
Goals:
├── Add ECDSA signature verification
├── Implement multisignature support
├── Create signing tools
└── Establish key management workflow

Tasks:
1. Integrate secp256k1 library
2. Implement signature section parsing
3. Add public key storage in flash
4. Port upgrade-generator.py tool
5. Create self-signed key generation workflow
6. Test: Sign firmware, verify on device
```

### Phase 3: Production Hardening (Weeks 5-6)

```
Goals:
├── Enable RDP Level 1 protection
├── Enable write protection
├── Add downgrade attack prevention
└── Create recovery procedures

Tasks:
1. Implement version check records (VCR)
2. Add option bytes programming
3. Create RDP/WRP enable procedures
4. Document recovery using STM32CubeProgrammer
5. Test: Verify protection, test recovery
6. Security review of implementation
```

### Phase 4: Integration with CCID Reader (Weeks 7-8)

```
Goals:
├── Integrate bootloader with main CCID firmware
├── Add firmware update UI on touchscreen
├── Implement version display
└── End-to-end testing

Tasks:
1. Combine bootloader and CCID firmware builds
2. Add touchscreen UI for update status
3. Implement update confirmation screen
4. Add version info display on boot
5. Create release signing workflow
6. Final integration testing
```

### Phase 5: Optional RDP Level 2 (Post-Release)

```
Goals:
├── Evaluate RDP Level 2 necessity
├── Create irreversible protection path
└── Document tradeoffs and decision points

⚠️ ONLY after:
  - Extensive field testing with Level 1
  - Verified firmware update reliability
  - Documented recovery impossible
```

### File Structure for Implementation

```
ccid-reader/
├── bootloader/
│   ├── Cargo.toml
│   ├── src/
│   │   ├── startup.rs         # Stage 1 code
│   │   ├── bootloader.rs      # Stage 2 code
│   │   ├── crypto.rs          # Signature verification
│   │   ├── sdcard.rs          # SD card access
│   │   ├── flash.rs           # Flash programming
│   │   └── version.rs         # Version handling
│   ├── keys/
│   │   ├── test/              # Test public keys
│   │   └── selfsigned/        # Production public keys
│   └── tools/
│       ├── make-initial.rs    # Initial firmware builder
│       └── upgrade-gen.rs     # Upgrade file generator
├── firmware/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs            # CCID reader application
│       ├── ccid.rs            # USB CCID protocol
│       ├── smartcard.rs       # Card communication
│       └── ui.rs              # Touchscreen UI
└── memory.x                   # Linker script
```

### Build Commands

```bash
# Build bootloader
cd bootloader
cargo build --release --target thumbv7em-none-eabihf

# Build firmware
cd firmware
cargo build --release --target thumbv7em-none-eabihf

# Create initial firmware (both bootloader + firmware)
cd bootloader/tools
cargo run --bin make-initial -- \
  --startup ../target/thumbv7em-none-eabihf/release/startup.bin \
  --bootloader ../target/thumbv7em-none-eabihf/release/bootloader.bin \
  --firmware ../../firmware/target/.../firmware.bin \
  --output initial_firmware.bin

# Flash to device
st-flash write initial_firmware.bin 0x8000000

# Create signed upgrade
cargo run --bin upgrade-gen -- \
  gen --firmware new_firmware.bin \
  --platform stm32f469disco \
  upgrade.bin

# Sign upgrade (repeat for each required signature)
cargo run --bin upgrade-gen -- \
  message upgrade.bin
# → Transfer message to signing device

cargo run --bin upgrade-gen -- \
  import-sig --signature "BASE64_SIG..." \
  upgrade.bin
```

---

## References

### Source Research

- **Specter-DIY Bootloader:** https://github.com/cryptoadvance/specter-diy/tree/master/bootloader
- **Specter-Bootloader (standalone):** https://github.com/cryptoadvance/specter-bootloader
- **STM32F469 Reference Manual:** RM0386
- **STM32F469 Datasheet:** DS10314

### Key Documentation Files Reviewed

1. `bootloader/README.md` - Build and usage overview
2. `bootloader/doc/bootloader-spec.md` - Technical specification
3. `bootloader/doc/selfsigned.md` - Custom key setup
4. `bootloader/doc/remove_protection.md` - Recovery procedures
5. `docs/security.md` - Security architecture overview

### Security Considerations

1. **Never skip signature verification** - Always verify against data in flash, not SD card
2. **Test recovery before RDP Level 2** - Once set, no recovery possible
3. **Use multisignature** - Single key compromise should not allow firmware injection
4. **Verify on device, not just during build** - Catch tampering at load time
5. **Downgrade protection** - Prevent rollback to known-vulnerable versions

---

## 9. Secure PIN Entry

For documentation on secure PIN entry and PIN pad architecture, see [PINPAD-ARCHITECTURE.md](PINPAD-ARCHITECTURE.md).

### Key Security Considerations for PIN Entry

1. **PIN Buffer Security**:
   - Store PIN in volatile memory only
   - Use `core::ptr::write_volatile` to prevent compiler optimization
   - Clear buffer immediately after use
   - Implement `Drop` trait for automatic clearing

2. **Display Security**:
   - Show masked PIN (`****`) on display
   - Never log or transmit PIN digits
   - Display transaction data on trusted screen (SWYS)

3. **Touch Isolation**:
   - Touch coordinates never leave the device
   - Only constructed APDU is sent to card
   - Host cannot access raw touch events

4. **CCID Integration**:
   - `PC_to_RDR_Secure` (0x69) handles PIN entry flow
   - Error codes: `0xEF` (cancelled), `0xF0` (timeout)
   - Descriptor fields: `bPINSupport`, `wLcdLayout`, `dwFeatures`

### Reference Implementation

See `src/pinpad/` directory for:
- `mod.rs` - PinBuffer with secure clearing
- `state.rs` - PIN entry state machine
- `ui.rs` - Touchscreen keypad UI
- `apdu.rs` - VERIFY APDU construction

