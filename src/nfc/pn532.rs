//! PN532 NFC controller — SPI transport layer
//!
//! Low-level driver for communicating with the PN532 over SPI.
//! Implements the PN532 frame protocol (preamble, length, checksum,
//! ACK/NAK detection) and exposes high-level NFC commands needed for
//! ISO 14443-4 (ISO-DEP) card interaction.
//!
//! # SPI Protocol
//!
//! The PN532 SPI interface uses a simple framing scheme on top of
//! standard SPI (CPOL=0, CPHA=0, MSB first, ≤5 MHz):
//!
//! - **Data Write** (`0x01`): Host → PN532 command frame
//! - **Status Read** (`0x02`): Host reads 1 byte; `0x01` = ready
//! - **Data Read** (`0x03`): Host reads response frame
//!
//! # References
//!
//! - PN532/C1 User Manual, Chapter 6 (Host Controller Interface)
//! - PN532/C1 User Manual, Chapter 7 (NFC Commands)

use embedded_hal::blocking::spi;
use embedded_hal::digital::v2::OutputPin;

// ============================================================================
// SPI framing bytes
// ============================================================================

/// SPI command: write a data frame to PN532
const SPI_DATA_WRITE: u8 = 0x01;
/// SPI command: read status register (1 byte; 0x01 = ready)
const SPI_STATUS_READ: u8 = 0x02;
/// SPI command: read a data frame from PN532
const SPI_DATA_READ: u8 = 0x03;

/// PN532 status byte indicating the response is ready
const SPI_STATUS_READY: u8 = 0x01;

// ============================================================================
// PN532 frame constants
// ============================================================================

/// Frame preamble byte
const PREAMBLE: u8 = 0x00;
/// Start code byte 1
const START_CODE_1: u8 = 0x00;
/// Start code byte 2
const START_CODE_2: u8 = 0xFF;
/// Transport Frame Identifier: Host → PN532
const TFI_HOST_TO_PN532: u8 = 0xD4;
/// Transport Frame Identifier: PN532 → Host
const TFI_PN532_TO_HOST: u8 = 0xD5;
/// ACK frame bytes (sent by PN532 after receiving a valid command)
const ACK_FRAME: [u8; 6] = [0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00];

// ============================================================================
// PN532 command codes
// ============================================================================

/// Get PN532 firmware version (IC, Ver, Rev, Support)
const CMD_GET_FIRMWARE_VERSION: u8 = 0x02;
/// Configure the Security Access Module
const CMD_SAM_CONFIGURATION: u8 = 0x14;
/// Configure RF communication parameters
const CMD_RF_CONFIGURATION: u8 = 0x32;
/// Detect and activate passive NFC targets
const CMD_IN_LIST_PASSIVE_TARGET: u8 = 0x4A;
/// Exchange data with an activated target (send/receive APDUs)
const CMD_IN_DATA_EXCHANGE: u8 = 0x40;
/// Deactivate/release an activated target
const CMD_IN_RELEASE: u8 = 0x52;

// ============================================================================
// Constants
// ============================================================================

/// Maximum payload size for PN532 commands/responses
/// (PN532 supports up to 265 bytes in normal frame, minus overhead)
const MAX_FRAME_DATA: usize = 255;

/// Maximum number of SPI status polls before timeout
const MAX_POLL_RETRIES: u32 = 3000;

/// Delay between SPI status polls (microseconds, approximate via busy-wait)
/// TODO: Replace with proper delay when embedded-hal DelayUs is available
const POLL_DELAY_US: u32 = 1000;

// ============================================================================
// Error types
// ============================================================================

/// Errors that can occur during PN532 communication
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pn532Error {
    /// SPI bus read/write failed
    Spi,
    /// CS pin control failed
    CsPin,
    /// PN532 did not become ready within timeout
    Timeout,
    /// Invalid or corrupted response frame
    BadFrame,
    /// ACK frame not received after command
    NoAck,
    /// PN532 returned a NAK or error
    Nak,
    /// Response data exceeds buffer capacity
    BufferOverflow,
    /// PN532 reported an application-level error in InDataExchange
    NfcError(u8),
    /// No NFC target found during InListPassiveTarget
    NoTarget,
    /// PN532 firmware version check failed (not a PN532?)
    BadFirmware,
}

impl core::fmt::Display for Pn532Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Pn532Error::Spi => write!(f, "SPI error"),
            Pn532Error::CsPin => write!(f, "CS pin error"),
            Pn532Error::Timeout => write!(f, "PN532 timeout"),
            Pn532Error::BadFrame => write!(f, "bad frame"),
            Pn532Error::NoAck => write!(f, "no ACK"),
            Pn532Error::Nak => write!(f, "NAK received"),
            Pn532Error::BufferOverflow => write!(f, "buffer overflow"),
            Pn532Error::NfcError(code) => write!(f, "NFC error 0x{:02X}", code),
            Pn532Error::NoTarget => write!(f, "no target"),
            Pn532Error::BadFirmware => write!(f, "bad firmware"),
        }
    }
}

// ============================================================================
// Target information from InListPassiveTarget
// ============================================================================

/// Information about a detected ISO 14443A NFC target.
///
/// Populated from the PN532 InListPassiveTarget response.
#[derive(Debug, Clone)]
pub struct NfcTarget {
    /// Target number assigned by PN532 (1-based, usually 1)
    pub tg: u8,
    /// ATQA (SENS_RES) — 2 bytes indicating card type/capabilities
    pub atqa: [u8; 2],
    /// SAK (SEL_RES) — indicates card protocols (bit 5 set = ISO-DEP capable)
    pub sak: u8,
    /// UID (NFCID1) — 4, 7, or 10 bytes
    pub uid: [u8; 10],
    /// Length of UID in bytes
    pub uid_len: usize,
    /// Whether this target supports ISO-DEP (ISO 14443-4)
    pub is_iso_dep: bool,
}

impl Default for NfcTarget {
    fn default() -> Self {
        Self {
            tg: 0,
            atqa: [0; 2],
            sak: 0,
            uid: [0; 10],
            uid_len: 0,
            is_iso_dep: false,
        }
    }
}

// ============================================================================
// PN532 driver
// ============================================================================

/// Low-level PN532 NFC controller driver over SPI.
///
/// Generic over SPI bus and chip-select (CS) pin types.
/// Requires `embedded-hal` 0.2 blocking SPI traits.
///
/// # Wiring (Bolty-style, ESP32-S3 + PN532)
///
/// | PN532 Pin | ESP32-S3 Pin | Function        |
/// |-----------|-------------|-----------------|
/// | SCK       | GPIO 12     | SPI Clock       |
/// | MOSI      | GPIO 11     | SPI Data Out    |
/// | MISO      | GPIO 13     | SPI Data In     |
/// | SS/CS     | GPIO 10     | SPI Chip Select |
/// | IRQ       | (optional)  | Interrupt       |
/// | RSTO      | (optional)  | Reset Output    |
///
/// > **Note:** Pin assignments are suggestions matching Bolty-style wiring.
/// > Adjust in `main_nfc.rs` for your actual board layout.
pub struct Pn532<SPI, CS> {
    spi: SPI,
    cs: CS,
}

impl<SPI, CS, SpiError, PinError> Pn532<SPI, CS>
where
    SPI: spi::Transfer<u8, Error = SpiError> + spi::Write<u8, Error = SpiError>,
    CS: OutputPin<Error = PinError>,
{
    /// Create a new PN532 driver instance.
    ///
    /// After construction, call [`init()`](Pn532::init) to configure the PN532
    /// for NFC operation.
    pub fn new(spi: SPI, cs: CS) -> Self {
        Self { spi, cs }
    }

    /// Consume the driver and return the SPI bus and CS pin.
    pub fn release(self) -> (SPI, CS) {
        (self.spi, self.cs)
    }

    // ========================================================================
    // Initialization
    // ========================================================================

    /// Initialize the PN532 for NFC ISO 14443A operation.
    ///
    /// This performs:
    /// 1. Firmware version check (validates PN532 connectivity)
    /// 2. SAM configuration (normal mode, no timeout)
    /// 3. RF configuration for ISO 14443A polling
    ///
    /// Call this once after power-on or hardware reset.
    pub fn init(&mut self) -> Result<(), Pn532Error> {
        // Verify PN532 firmware version
        let (ic, ver, rev, _support) = self.get_firmware_version()?;
        if ic != 0x07 {
            // IC = 0x07 indicates PN532
            return Err(Pn532Error::BadFirmware);
        }
        ccid_info!(
            "PN532: firmware IC=0x{:02X} ver={}.{}",
            ic,
            ver,
            rev
        );

        // SAM Configuration: Normal mode, no timeout, IRQ not used
        self.sam_configuration()?;

        // RF Configuration: set max retries for target detection
        // Item 0x05 = MaxRetries: MxRtyATR=0xFF, MxRtyPSL=0x01, MxRtyPassiveActivation=0xFF
        self.rf_configuration(0x05, &[0xFF, 0x01, 0xFF])?;

        Ok(())
    }

    // ========================================================================
    // High-level NFC commands
    // ========================================================================

    /// Get PN532 firmware version.
    ///
    /// Returns `(IC, Ver, Rev, Support)` tuple.
    /// IC should be `0x07` for a genuine PN532.
    pub fn get_firmware_version(&mut self) -> Result<(u8, u8, u8, u8), Pn532Error> {
        let cmd = [CMD_GET_FIRMWARE_VERSION];
        let mut resp = [0u8; 4];
        let len = self.send_command_read_response(&cmd, &mut resp)?;
        if len < 4 {
            return Err(Pn532Error::BadFrame);
        }
        Ok((resp[0], resp[1], resp[2], resp[3]))
    }

    /// Configure the Security Access Module (SAM).
    ///
    /// Sets normal mode with no timeout — the SAM is not used, and the
    /// PN532 operates in direct NFC mode.
    pub fn sam_configuration(&mut self) -> Result<(), Pn532Error> {
        // Mode=0x01 (Normal), Timeout=0x00 (unused), IRQ=0x01 (use IRQ pin)
        let cmd = [CMD_SAM_CONFIGURATION, 0x01, 0x00, 0x01];
        let mut resp = [0u8; 1];
        self.send_command_read_response(&cmd, &mut resp)?;
        Ok(())
    }

    /// Configure RF communication parameters.
    ///
    /// `cfg_item` selects which parameter to configure.
    /// `data` contains the configuration value(s).
    pub fn rf_configuration(&mut self, cfg_item: u8, data: &[u8]) -> Result<(), Pn532Error> {
        let mut cmd = [0u8; 16];
        cmd[0] = CMD_RF_CONFIGURATION;
        cmd[1] = cfg_item;
        let data_len = data.len().min(14);
        cmd[2..2 + data_len].copy_from_slice(&data[..data_len]);
        let cmd_len = 2 + data_len;
        let mut resp = [0u8; 1];
        self.send_command_read_response(&cmd[..cmd_len], &mut resp)?;
        Ok(())
    }

    /// Detect and activate an ISO 14443A passive target.
    ///
    /// Sends InListPassiveTarget for one target at 106 kbps (Type A).
    /// If a card is found, returns its target info; otherwise returns
    /// `Err(Pn532Error::NoTarget)`.
    ///
    /// For ISO-DEP capable cards (SAK bit 5 set), the PN532 automatically
    /// performs the RATS exchange, activating the ISO 14443-4 layer.
    pub fn detect_target_iso14443a(&mut self) -> Result<NfcTarget, Pn532Error> {
        let cmd = [
            CMD_IN_LIST_PASSIVE_TARGET,
            0x01, // MaxTg: detect 1 target
            0x00, // BrTy: 106 kbps Type A (ISO 14443A)
        ];
        let mut resp = [0u8; 64];
        let resp_len = self.send_command_read_response(&cmd, &mut resp)?;

        // Response: [NbTg] [Tg] [SENS_RES (2)] [SEL_RES (1)] [NFCIDLength] [NFCID1...]
        if resp_len < 1 || resp[0] == 0 {
            return Err(Pn532Error::NoTarget);
        }

        if resp_len < 6 {
            return Err(Pn532Error::BadFrame);
        }

        let mut target = NfcTarget::default();
        target.tg = resp[1];
        target.atqa[0] = resp[2];
        target.atqa[1] = resp[3];
        target.sak = resp[4];
        let uid_len = resp[5] as usize;
        if uid_len > 10 || 6 + uid_len > resp_len {
            return Err(Pn532Error::BadFrame);
        }
        target.uid[..uid_len].copy_from_slice(&resp[6..6 + uid_len]);
        target.uid_len = uid_len;

        // SAK bit 5 (0x20) indicates ISO 14443-4 (ISO-DEP) support
        target.is_iso_dep = (target.sak & 0x20) != 0;

        Ok(target)
    }

    /// Exchange an APDU with the currently activated ISO-DEP target.
    ///
    /// Sends `apdu` data to target number `tg` via InDataExchange and
    /// returns the card's response.
    ///
    /// # Arguments
    ///
    /// - `tg`: Target number (from `NfcTarget::tg`, usually 1)
    /// - `apdu`: Command APDU bytes
    /// - `response`: Buffer for the response APDU
    ///
    /// # Returns
    ///
    /// Number of response bytes written to `response`.
    pub fn in_data_exchange(
        &mut self,
        tg: u8,
        apdu: &[u8],
        response: &mut [u8],
    ) -> Result<usize, Pn532Error> {
        // Command: [CMD_IN_DATA_EXCHANGE] [Tg] [DataOut...]
        let mut cmd = [0u8; MAX_FRAME_DATA];
        if apdu.len() + 2 > MAX_FRAME_DATA {
            return Err(Pn532Error::BufferOverflow);
        }
        cmd[0] = CMD_IN_DATA_EXCHANGE;
        cmd[1] = tg;
        cmd[2..2 + apdu.len()].copy_from_slice(apdu);
        let cmd_len = 2 + apdu.len();

        let mut resp = [0u8; MAX_FRAME_DATA];
        let resp_len = self.send_command_read_response(&cmd[..cmd_len], &mut resp)?;

        // Response: [Status] [DataIn...]
        if resp_len < 1 {
            return Err(Pn532Error::BadFrame);
        }

        let status = resp[0];
        if status != 0x00 {
            return Err(Pn532Error::NfcError(status));
        }

        let data_len = resp_len - 1;
        if data_len > response.len() {
            return Err(Pn532Error::BufferOverflow);
        }
        response[..data_len].copy_from_slice(&resp[1..1 + data_len]);
        Ok(data_len)
    }

    /// Release (deactivate) the specified target.
    ///
    /// Pass `tg = 0` to release all targets.
    pub fn in_release(&mut self, tg: u8) -> Result<(), Pn532Error> {
        let cmd = [CMD_IN_RELEASE, tg];
        let mut resp = [0u8; 1];
        let resp_len = self.send_command_read_response(&cmd, &mut resp)?;
        if resp_len >= 1 && resp[0] != 0x00 {
            return Err(Pn532Error::NfcError(resp[0]));
        }
        Ok(())
    }

    // ========================================================================
    // Frame-level SPI protocol
    // ========================================================================

    /// Send a PN532 command and read the response.
    ///
    /// Handles the full protocol sequence:
    /// 1. Build and send command frame
    /// 2. Wait for and verify ACK
    /// 3. Poll for response readiness
    /// 4. Read and parse response frame
    ///
    /// `cmd` should contain the command byte followed by parameters
    /// (TFI is added automatically).
    ///
    /// Returns the number of response data bytes (excluding TFI and
    /// response command byte).
    fn send_command_read_response(
        &mut self,
        cmd: &[u8],
        response: &mut [u8],
    ) -> Result<usize, Pn532Error> {
        self.write_command_frame(cmd)?;
        self.read_ack()?;
        self.wait_ready()?;
        self.read_response_frame(cmd[0], response)
    }

    /// Build and write a command frame to the PN532.
    ///
    /// Frame format:
    /// ```text
    /// [SPI_DATA_WRITE] [PREAMBLE] [START1] [START2] [LEN] [LCS] [TFI] [DATA...] [DCS] [POSTAMBLE]
    /// ```
    fn write_command_frame(&mut self, cmd: &[u8]) -> Result<(), Pn532Error> {
        let data_len = cmd.len() + 1; // +1 for TFI
        if data_len > 255 {
            return Err(Pn532Error::BufferOverflow);
        }

        let len = data_len as u8;
        let lcs = (!len).wrapping_add(1); // LCS = 0x00 - LEN

        // Calculate DCS: 0x00 - (TFI + sum(cmd))
        let mut dcs_sum: u8 = TFI_HOST_TO_PN532;
        for &b in cmd {
            dcs_sum = dcs_sum.wrapping_add(b);
        }
        let dcs = (!dcs_sum).wrapping_add(1);

        // Build the full SPI frame
        let mut frame = [0u8; 264]; // max: 8 overhead + 255 data
        let mut pos = 0;

        frame[pos] = SPI_DATA_WRITE;
        pos += 1;
        frame[pos] = PREAMBLE;
        pos += 1;
        frame[pos] = START_CODE_1;
        pos += 1;
        frame[pos] = START_CODE_2;
        pos += 1;
        frame[pos] = len;
        pos += 1;
        frame[pos] = lcs;
        pos += 1;
        frame[pos] = TFI_HOST_TO_PN532;
        pos += 1;
        frame[pos..pos + cmd.len()].copy_from_slice(cmd);
        pos += cmd.len();
        frame[pos] = dcs;
        pos += 1;
        frame[pos] = PREAMBLE; // postamble
        pos += 1;

        self.cs_low()?;
        let result = self.spi.write(&frame[..pos]).map_err(|_| Pn532Error::Spi);
        self.cs_high()?;
        result
    }

    /// Read and verify the ACK frame from PN532.
    ///
    /// Must be called after writing a command. The PN532 sends a 6-byte
    /// ACK frame (`00 00 FF 00 FF 00`) if the command was received correctly.
    fn read_ack(&mut self) -> Result<(), Pn532Error> {
        self.wait_ready()?;

        self.cs_low()?;
        let mut frame = [0u8; 7]; // SPI_DATA_READ + 6 ACK bytes
        frame[0] = SPI_DATA_READ;
        let result = self.spi.transfer(&mut frame).map_err(|_| Pn532Error::Spi);
        self.cs_high()?;
        result?;

        // Verify ACK (bytes 1-6)
        if frame[1..7] != ACK_FRAME {
            return Err(Pn532Error::NoAck);
        }
        Ok(())
    }

    /// Poll the PN532 status register until ready or timeout.
    fn wait_ready(&mut self) -> Result<(), Pn532Error> {
        for _ in 0..MAX_POLL_RETRIES {
            self.cs_low()?;
            let mut status = [SPI_STATUS_READ, 0x00];
            let result = self
                .spi
                .transfer(&mut status)
                .map_err(|_| Pn532Error::Spi);
            self.cs_high()?;
            result?;

            if status[1] == SPI_STATUS_READY {
                return Ok(());
            }

            // Busy-wait approximation
            // TODO: Use embedded_hal::blocking::delay::DelayUs for proper delay
            busy_wait_approx(POLL_DELAY_US);
        }

        Err(Pn532Error::Timeout)
    }

    /// Read and parse a response frame from the PN532.
    ///
    /// `expected_cmd` is the command byte that was sent; the response
    /// command byte should be `expected_cmd + 1`.
    ///
    /// Returns the number of data bytes written to `response` (payload
    /// after the response command byte).
    fn read_response_frame(
        &mut self,
        expected_cmd: u8,
        response: &mut [u8],
    ) -> Result<usize, Pn532Error> {
        // Read a generous buffer: SPI_DATA_READ + preamble(1) + start(2) +
        // len(1) + lcs(1) + tfi(1) + data(up to 255) + dcs(1) + postamble(1)
        let mut frame = [0u8; 264];
        frame[0] = SPI_DATA_READ;

        self.cs_low()?;
        let result = self
            .spi
            .transfer(&mut frame)
            .map_err(|_| Pn532Error::Spi);
        self.cs_high()?;
        result?;

        // Find start code (0x00 0xFF) — skip preamble bytes
        let mut offset = 1; // skip SPI_DATA_READ byte
        while offset < frame.len() - 1 {
            if frame[offset] == START_CODE_1 && frame[offset + 1] == START_CODE_2 {
                break;
            }
            offset += 1;
        }
        if offset >= frame.len() - 1 {
            return Err(Pn532Error::BadFrame);
        }
        offset += 2; // skip start code

        // Read length and LCS
        if offset + 2 > frame.len() {
            return Err(Pn532Error::BadFrame);
        }
        let len = frame[offset] as usize;
        let lcs = frame[offset + 1];
        offset += 2;

        // Verify LCS
        if (len as u8).wrapping_add(lcs) != 0 {
            return Err(Pn532Error::BadFrame);
        }

        // Check we have enough data
        if offset + len + 2 > frame.len() {
            return Err(Pn532Error::BadFrame);
        }

        // Verify TFI
        if frame[offset] != TFI_PN532_TO_HOST {
            return Err(Pn532Error::BadFrame);
        }

        // Verify response command byte
        let resp_cmd = frame[offset + 1];
        if resp_cmd != expected_cmd + 1 {
            return Err(Pn532Error::BadFrame);
        }

        // Verify DCS
        let mut dcs_check: u8 = 0;
        for i in 0..len {
            dcs_check = dcs_check.wrapping_add(frame[offset + i]);
        }
        let dcs = frame[offset + len];
        if dcs_check.wrapping_add(dcs) != 0 {
            return Err(Pn532Error::BadFrame);
        }

        // Extract payload (skip TFI and response command byte)
        let payload_len = if len >= 2 { len - 2 } else { 0 };
        if payload_len > response.len() {
            return Err(Pn532Error::BufferOverflow);
        }
        response[..payload_len].copy_from_slice(&frame[offset + 2..offset + 2 + payload_len]);

        Ok(payload_len)
    }

    // ========================================================================
    // CS pin helpers
    // ========================================================================

    fn cs_low(&mut self) -> Result<(), Pn532Error> {
        self.cs.set_low().map_err(|_| Pn532Error::CsPin)
    }

    fn cs_high(&mut self) -> Result<(), Pn532Error> {
        self.cs.set_high().map_err(|_| Pn532Error::CsPin)
    }
}

// ============================================================================
// Utility
// ============================================================================

/// Approximate busy-wait delay.
///
/// This is a crude delay implementation for environments without a
/// proper timer. Each iteration performs a volatile read to prevent
/// the compiler from optimizing the loop away.
///
/// TODO: Replace with `embedded_hal::blocking::delay::DelayUs` when
/// a delay provider is available from the HAL.
fn busy_wait_approx(us: u32) {
    // Approximate: each volatile read ≈ a few CPU cycles.
    // At 240 MHz (ESP32-S3), ~240 cycles ≈ 1 µs, so ~30 iterations
    // of volatile read (≈8 cycles each) per µs.
    let iterations = us * 30;
    for i in 0..iterations {
        unsafe {
            core::ptr::read_volatile(&i);
        }
    }
}
