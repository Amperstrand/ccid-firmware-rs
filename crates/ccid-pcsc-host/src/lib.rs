//! Host-side PC/SC client for Amperstrand CCID readers.
//!
//! Canonical home for the host↔card APDU knowledge proven on the bench
//! (Cherry USB CCID, GemPC Twin serial, nucula wifi-vpcd): reader
//! discovery by name, the verified APDU case forms, and exchange with
//! status-word splitting. Consumers across Amperstrand repos inherit
//! these rules instead of re-deriving them (modularity rule).
//!
//! ## APDU case forms (bench-verified 2026-10-14, nucula wifi-vpcd chain)
//!
//! - command data present → `[CLA INS P1 P2 Lc data]` (case-3, NO Le —
//!   this applet family counts Le as incoming data: import_mint with a
//!   trailing Le answers 6700)
//! - command data empty   → `[CLA INS P1 P2 Le]` (case-2, Le = 0 → 256)
//! - NEVER a bare `Lc=0` byte before Le (the 6-byte zero-data form):
//!   the card answers `6700` and the nucula board card-session wedges
//!   (phantom-ATR garbage until OTA reboot + pcscd restart).
//!   Trailing-Le forms survive only on JCRE-routed commands (SELECT).

#![forbid(unsafe_code)]

use core::fmt;

pub const SW_OK: u16 = 0x9000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Transport(&'static str),
    Sw(u16),
    /// `apdu()` refuses data longer than a short-APDU Lc can encode —
    /// a silent `as u8` wrap would emit Lc=0 followed by the data,
    /// which cards parse as an extended-length header (Codex #169).
    DataTooLong(usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Transport(m) => write!(f, "pcsc transport: {m}"),
            Error::Sw(sw) => write!(f, "card status word {sw:04X}"),
            Error::DataTooLong(n) => {
                write!(f, "apdu data {n} bytes exceeds short-APDU Lc (255)")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Build a short APDU in the bench-verified case form.
///
/// `data.is_empty()` → case-2 `[h h h h Le]`; otherwise case-3
/// `[h h h h Lc data]` (no Le). The 6-byte zero-data form (bare Lc=0
/// before Le) is never produced — see the module docs.
pub fn apdu(cla: u8, ins: u8, p1: u8, p2: u8, data: &[u8]) -> Result<Vec<u8>, Error> {
    if data.len() > u8::MAX as usize {
        return Err(Error::DataTooLong(data.len()));
    }
    let mut cmd = Vec::with_capacity(5 + data.len());
    cmd.extend_from_slice(&[cla, ins, p1, p2]);
    if data.is_empty() {
        cmd.push(0); // Le
    } else {
        cmd.push(data.len() as u8);
        cmd.extend_from_slice(data);
    }
    Ok(cmd)
}

/// A connected card: exchange APDUs, get back (response, SW).
pub struct PcscCard {
    card: pcsc::Card,
}

impl PcscCard {
    /// Connect to the first pcscd reader whose name contains
    /// `name_substr`, requiring a card present. Reader-name matching is
    /// the bench convention (gp and the HIL suites select by name).
    pub fn connect_by_name(name_substr: &str) -> Result<Self, Error> {
        let ctx = pcsc::Context::establish(pcsc::Scope::User)
            .map_err(|_| Error::Transport("pcsc context"))?;
        let mut buf = [0; 2048];
        let readers = ctx
            .list_readers(&mut buf)
            .map_err(|_| Error::Transport("list readers"))?;
        let mut found = None;
        for r in readers {
            let s = r.to_string_lossy();
            if s.contains(name_substr) {
                found = Some(s.into_owned());
                break;
            }
        }
        let name = found.ok_or(Error::Transport("no matching reader"))?;
        let cstr = std::ffi::CString::new(name).map_err(|_| Error::Transport("reader name"))?;
        let card = ctx
            .connect(&cstr, pcsc::ShareMode::Shared, pcsc::Protocols::ANY)
            .map_err(|_| Error::Transport("connect card"))?;
        Ok(Self { card })
    }

    pub fn atr(&self) -> Result<Vec<u8>, Error> {
        let st = self
            .card
            .status2_owned()
            .map_err(|_| Error::Transport("card status"))?;
        Ok(st.atr().to_vec())
    }

    /// Transmit a pre-built APDU (`apdu()` output or a raw form),
    /// returning (response bytes, SW).
    pub fn transmit(&self, cmd: &[u8]) -> Result<(Vec<u8>, u16), Error> {
        let mut resp = [0u8; 1024];
        let r = self
            .card
            .transmit(cmd, &mut resp)
            .map_err(|_| Error::Transport("transmit"))?;
        let n = r.len();
        if n < 2 {
            return Err(Error::Transport("short response"));
        }
        let sw = u16::from_be_bytes([r[n - 2], r[n - 1]]);
        Ok((r[..n - 2].to_vec(), sw))
    }

    /// Convenience: build the case-verified APDU and transmit it;
    /// SW != 9000 becomes `Error::Sw`.
    pub fn exchange(
        &self,
        cla: u8,
        ins: u8,
        p1: u8,
        p2: u8,
        data: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let (resp, sw) = self.transmit(&apdu(cla, ins, p1, p2, data)?)?;
        if sw != SW_OK {
            return Err(Error::Sw(sw));
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_data_is_case2_header_plus_le() {
        assert_eq!(
            apdu(0xB0, 0xB0, 0, 0, &[]).unwrap(),
            vec![0xB0, 0xB0, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            apdu(0x00, 0xA4, 0x04, 0x00, &[]).unwrap(),
            vec![0x00, 0xA4, 0x04, 0x00, 0x00]
        );
    }

    #[test]
    fn data_is_case3_lc_without_trailing_le() {
        assert_eq!(
            apdu(0xB0, 0xB1, 0x00, 0x00, &[0x14, 0x68, 0x74]).unwrap(),
            vec![0xB0, 0xB1, 0x00, 0x00, 0x03, 0x14, 0x68, 0x74]
        );
    }

    /// Regression: the 6-byte zero-data form (bare Lc=0 before Le) is
    /// 6700 on hardware and wedges the nucula card-session — the builder
    /// must never produce it.
    /// Regression (Codex #169): 256 data bytes used to wrap Lc to 0 and
    /// stream the data after it — a card parses that as an extended-length
    /// header, not the promised short case-3. The 255 boundary still builds.
    #[test]
    fn oversize_data_is_rejected_not_wrapped() {
        let max = vec![0xAB; 255];
        let cmd = apdu(0x00, 0xA4, 0x00, 0x00, &max).unwrap();
        assert_eq!(cmd.len(), 5 + 255);
        assert_eq!(cmd[4], 255);
        for n in [256usize, 300, 4096] {
            assert_eq!(
                apdu(0x00, 0xA4, 0x00, 0x00, &vec![0x00; n]),
                Err(Error::DataTooLong(n))
            );
        }
    }

    #[test]
    fn builder_never_emits_the_wedging_six_byte_form() {
        for cla in [0x00u8, 0x80, 0xB0] {
            let cmd = apdu(cla, 0x3C, 0x00, 0x00, &[]).unwrap();
            assert_eq!(cmd.len(), 5, "zero-data APDU must stay 5 bytes");
        }
    }
}
