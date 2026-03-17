# CCID Firmware Specifications Reference

This document lists the authoritative specifications and reference implementations for the CCID firmware emulator.

## Official Specifications

### USB CCID Specification

The primary specification for USB Chip/Smart Card Interface Devices.

| Property | Value |
|----------|-------|
| **Name** | Specification for Integrated Circuit(s) Cards Interface Devices |
| **Version** | Revision 1.1 |
| **Date** | April 22, 2005 |
| **Source** | [USB.org Document Library](https://www.usb.org/document-library/smart-card-ccid-version-11) |
| **Direct PDF** | [DWG_Smart-Card_CCID_Rev110.pdf](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf) |

**Key Sections:**
- Chapter 5: CCID Class Descriptor (Table 5.1-1)
- Chapter 6: CCID Message Protocol
- Chapter 7: Class-Specific Requests

### ISO/IEC 7816 Smart Card Specifications

CCID relies on ISO 7816 for physical and logical data exchange.

| Part | Title | Relevance |
|------|-------|-----------|
| Part 1 | Cards with contacts - Physical characteristics | Card dimensions and resistance |
| Part 2 | Cards with contacts - Dimensions and location of contacts | C1-C8 pinout |
| Part 3 | Cards with contacts - Electrical interface and transmission protocols | **Critical**: T=0/T=1 protocols, ATR, voltage levels |
| Part 4 | Organization, security and commands for interchange | APDU structure (CLA, INS, P1, P2, Lc, Data, Le) |

**Source:** [ISO Store](https://www.iso.org/standard/77355.html) (paid standards)

### PC/SC Specifications

Software architecture for smart card readers.

| Part | Title | Relevance |
|------|-------|-----------|
| Part 10 | IFDs with Secure PIN Entry Capabilities | **Critical**: `IOCTL_FEATURE_GET_TLV_PROPERTIES`, PIN pad features, LCD displays |

| Property | Value |
|----------|-------|
| **Version** | 2.02.09 |
| **Source** | [PC/SC Workgroup](https://pcscworkgroup.com/specifications/download/) |
| **Direct PDF** | [pcsc10_v2.02.09.pdf](http://pcscworkgroup.com/Download/Specifications/pcsc10_v2.02.09.pdf) |

### USB ICCD Specification

Lighter alternative to CCID for USB tokens with hard-wired cards.

| Property | Value |
|----------|-------|
| **Name** | Identification Cards - Integrated Circuit(s) Cards Interface Devices - Bridge with USB |
| **Version** | 1.0 |
| **Source** | [USB.org](https://www.usb.org/document-library/integrated-circuit-card-device-iccd-spec-10) |

## Reference Implementations

### CCID Free Software Driver (libccid)

The authoritative open-source CCID driver for Linux/Unix.

| Property | Value |
|----------|-------|
| **Maintainer** | Ludovic Rousseau |
| **Repository** | [salsa.debian.org/rousseau/CCID](https://salsa.debian.org/rousseau/CCID) |
| **Website** | [ccid.apdu.fr](https://ccid.apdu.fr/) |
| **License** | LGPL-2.1-or-later |

**Key Files:**
- `readers/*.txt` - Reader capability database (684+ devices)
- `src/ccid.h` - Feature flag definitions
- `src/commands.c` - CCID command handling

**Included in this project:** `reference/CCID/` (git submodule)

### osmo-ccid-firmware

CCID firmware implementation by Osmocom.

| Property | Value |
|----------|-------|
| **Maintainer** | Osmocom (sysmocom) |
| **Repository** | [gitea.osmocom.org/sim-card/osmo-ccid-firmware](https://gitea.osmocom.org/sim-card/osmo-ccid-firmware) |
| **License** | GPL-2.0-or-later |

**Key Components:**
- `ccid_common/` - Shared CCID protocol code
- `ccid_host/` - Host-side utilities
- `sysmoOCTSIM/` - Firmware for sysmoOCTSIM hardware
- `tests/` - Test patterns and protocol tests

**Included in this project:** `reference/osmo-ccid-firmware/` (git submodule)

### pcsc-lite

PC/SC middleware for Unix-like systems.

| Property | Value |
|----------|-------|
| **Maintainer** | Ludovic Rousseau |
| **Repository** | [github.com/LudovicRousseau/PCSC](https://github.com/LudovicRousseau/PCSC) |
| **Website** | [pcsclite.apdu.fr](https://pcsclite.apdu.fr/) |

**Key Headers:**
- `src/PCSC/reader.h` - PIN pad structures (`PIN_VERIFY_STRUCTURE`, etc.)
- `src/PCSC/ifdhandler.h` - IFD handler interface
- `src/PCSC/winscard.h` - WinSCard API

## Additional Resources

### EMV Contact Interface Specification

For payment card compatibility, EMV provides stricter timing requirements.

| Property | Value |
|----------|-------|
| **Source** | [EMVCo](https://www.emvco.com/emv-technologies/contact/) |
| **Document** | EMV Contact Book 1 (Application Independent ICC to Terminal Interface Requirements) |

### USB 2.0 Specification

Foundation for USB device descriptors.

| Property | Value |
|----------|-------|
| **Relevant Chapter** | Chapter 9 (USB Device Framework) |
| **Source** | [USB.org](https://www.usb.org/document-library/usb-20-specification) |

## Profile Reference Files

Device profiles in this project are aligned with the CCID reader database:

| Profile Feature | CCID Reference File |
|-----------------|---------------------|
| `profile-cherry-smartterminal-st2xxx` | `reference/CCID/readers/CherrySmartTerminalST2XXX.txt` |
| `profile-gemalto-idbridge-ct30` | `reference/CCID/readers/Gemalto_IDBridge_CT30.txt` |
| `profile-gemalto-idbridge-k30` | `reference/CCID/readers/Gemalto_IDBridge_K30.txt` |

> **Note:** The CCID project's reader files are the authoritative source of truth for device capabilities.

---

## Host Software Stack

This firmware is designed to work with standard smart card middleware on major operating systems.

### Linux: pcscd + libccid

The primary development and testing platform.

| Property | Value |
|----------|-------|
| **Package** | `pcscd` (daemon) + `libccid` (driver) |
| **Maintainer** | Ludovic Rousseau |
| **Website** | [pcsclite.apdu.fr](https://pcsclite.apdu.fr/) |
| **License** | GPL-2.0-or-later (pcscd), LGPL-2.1-or-later (libccid) |

**Key Integration Points:**
- Reader enumeration via USB VID/PID
- CCID descriptor parsing for feature detection
- PIN pad capability advertisement via `IOCTL_FEATURE_GET_TLV_PROPERTIES`
- Hot-plug support via udev

**Configuration File:** `/etc/libccid_Info.plist` (reader capabilities)

### Windows: Smart Card Base Components (SCBC)

Microsoft's native smart card framework.

| Property | Values |
|----------|-------|
| **Driver Model** | WDM (Windows Driver Model) |
| **Class** | Smart Card Reader Device Setup Class |
| **GUID** | `{50DD5230-BA8A-11D1-BF5D-0000F805F530}` |
| **Reference** | [Microsoft Smart Card Documentation](https://learn.microsoft.com/en-us/windows-hardware/drivers/smartcard/) |

**Key Integration Points:**
- CCID driver built into Windows (WUDFRd.sys)
- Reader identification via USB descriptors
- PIN pad support via `IOCTL_SMARTCARD_GET_FEATURES`

### macOS: SmartCardServices

Apple's smart card framework.

| Property | Values |
|----------|-------|
| **Framework** | SmartCardServices.framework |
| **Daemon** | `com.apple.CryptoTokenKit.pcscd` |
| **Reference** | [Apple Developer Documentation](https://developer.apple.com/documentation/security/certificate_key_and_trust_services/smart_cards) |

**Key Integration Points:**
- PC/SC compatibility layer
- Built-in CCID driver
- Integration with Keychain and Touch ID

---

## Target Hardware and Cards

### SeedKeeper / Specter DIY

The primary target hardware for this firmware.

| Property | Values |
|----------|-------|
| **Hardware** | Specter DIY Shield Lite + STM32F469-DISCO |
| **Card Type** | SeedKeeper (T=1 only) |
| **Protocol** | ISO 7816-3 T=1 |
| **Documentation** | [SeedKeeper Docs](https://seedkeeper.io/) |
| **APDU Reference** | [Satochip documentation](https://github.com/Toporin/SeedKeeper-docs) |

**Key APDU Commands Used:**
- `VERIFY` (INS=0x20) - PIN verification
- `CHANGE REFERENCE DATA` (INS=0x24) - PIN modification
- `GET DATA` / `PUT DATA` - Data storage
- `SELECT` - Application selection

### Reader Hardware Profiles

Device profiles emulate real commercial readers for plug-and-play compatibility:

| Profile | Emulates | Why |
|---------|----------|-----|
| `profile-cherry-smartterminal-st2xxx` | Cherry SmartTerminal ST-2xxx | PIN pad support |
| `profile-gemalto-idbridge-ct30` | Gemalto IDBridge CT30 | Basic reader (VID:08E6 PID:3437) |
| `profile-gemalto-idbridge-k30` | Gemalto IDBridge K30 | Basic reader (VID:08E6 PID:3438) |

---

## Rust Dependencies

This firmware builds on the Rust embedded ecosystem.

### USB Stack

| Crate | Purpose | License |
|-------|---------|---------|
| [usb-device](https://crates.io/crates/usb-device) | USB device framework | MIT |
| [synopsys-usb-otg](https://crates.io/crates/synopsys-usb-otg) | Synopsys USB OTG driver | MIT/Apache-2.0 |

**Our Fork:** `vendor/synopsys-usb-otg/` with warning suppressions for RAL-generated code.

### HAL and BSP

| Crate | Purpose | License |
|-------|---------|---------|
| [stm32f4xx-hal](https://crates.io/crates/stm32f4xx-hal) | STM32F4 hardware abstraction layer | 0BSD |
| [stm32f469i-disc](https://crates.io/crates/stm32f469i-disc) | STM32F469-DISCO board support | MIT/Apache-2.0 |

**Our Forks:**
- `Amperstrand/stm32f4xx-hal` - SDIO support additions
- `Amperstrand/stm32f469i-disc` - SDIO support additions

### Display (Optional)

| Crate | Purpose | License |
|-------|---------|---------|
| [embedded-graphics](https://crates.io/crates/embedded-graphics) | 2D graphics library | MIT/Apache-2.0 |

---

## Acknowledgments

This project derives ideas, patterns, and protocol behavior from:

1. **osmo-ccid-firmware** (GPL-2.0-or-later)
   - CCID command handling patterns
   - Slot state machine design
   - ATR parsing approach

2. **libccid** (LGPL-2.1-or-later)
   - Reader capability database
   - Feature flag definitions
   - libccid quirks (e.g., SetParameters without bProtocolNum)

3. **pcsc-lite** (GPL-2.0-or-later)
   - PIN pad structure definitions
   - IOCTL definitions

4. **SeedKeeper/Specter DIY**
   - Hardware reference design
   - Card APDU command set

5. **Cherry GmbH** and **Gemalto/Thales**
   - Reader capability specifications (via CCID reader database)
