//! OTA wire-protocol header + token authorization (pure logic, host-testable;
//! the TCP/flash server lives in `ota.rs`, esp targets only).
//!
//! Two header shapes share the 4-byte magic slot:
//! - `OTA1` + u32-LE size (8 bytes) — the original, tokenless form
//! - `OTA2` + u32-LE size + 32-byte token (40 bytes) — authenticated form
//!
//! Compile-time token: `OTA_TOKEN=<exactly 32 chars>` at build time arms the
//! server (same env-var discipline as the wifi credentials). Arming rules:
//! - server armed + `OTA2` + matching token → accepted
//! - server armed + `OTA2` + wrong token   → refused (loud, pre-flash)
//! - server armed + `OTA1`                 → refused ("this build requires
//!   a token") — a tokenless client must never flash an armed build
//! - server unarmed + `OTA1`               → accepted (today's behavior)
//! - server unarmed + `OTA2`               → accepted, token ignored
//!   (forward compatibility: old firmware, newer client)
//!
//! Mismatches fail at the MAGIC/token stage before any image byte is read —
//! no corruption window, both directions loud.

pub const MAGIC_V1: [u8; 4] = *b"OTA1";
pub const MAGIC_V2: [u8; 4] = *b"OTA2";
pub const TOKEN_LEN: usize = 32;
pub const HEADER_V1_LEN: usize = 8;
pub const HEADER_V2_LEN: usize = 40;

pub type Token = [u8; TOKEN_LEN];

pub fn compiled_token() -> Option<Token> {
    let tok = option_env!("OTA_TOKEN")?;
    let bytes = tok.as_bytes();
    if bytes.len() != TOKEN_LEN {
        // A wrong-length build-time token cannot be matched on the wire;
        // treat as unarmed would SILENTLY disable auth — fail loudly at
        // runtime instead (build.rs-style validation is overkill here).
        panic!(
            "OTA_TOKEN must be exactly {TOKEN_LEN} bytes, got {}",
            bytes.len()
        );
    }
    Some(
        *bytes
            .first_chunk::<TOKEN_LEN>()
            .expect("length checked above"),
    )
}

pub fn authorize(
    magic: &[u8; 4],
    token: Option<&Token>,
    expected: Option<&Token>,
) -> Result<(), &'static str> {
    match (*magic == MAGIC_V2, expected) {
        (true, Some(want)) => {
            let got = token.ok_or("OTA2 header missing token payload")?;
            if got == want {
                Ok(())
            } else {
                Err("OTA token mismatch")
            }
        }
        (true, None) => Ok(()),
        (false, Some(_)) => Err("OTA1 refused: this build requires a token (use OTA2)"),
        (false, None) => {
            if magic == &MAGIC_V1 {
                Ok(())
            } else {
                Err("bad magic")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOK: Token = [0xA5; TOKEN_LEN];

    #[test]
    fn v1_unarmed_accepts_the_original_protocol() {
        assert!(authorize(&MAGIC_V1, None, None).is_ok());
    }

    #[test]
    fn v2_matching_token_accepts() {
        assert!(authorize(&MAGIC_V2, Some(&TOK), Some(&TOK)).is_ok());
    }

    #[test]
    fn v2_wrong_token_is_refused() {
        let bad = [!0xA5; TOKEN_LEN];
        assert_eq!(
            authorize(&MAGIC_V2, Some(&bad), Some(&TOK)),
            Err("OTA token mismatch")
        );
    }

    #[test]
    fn v2_missing_token_is_refused_when_armed() {
        assert_eq!(
            authorize(&MAGIC_V2, None, Some(&TOK)),
            Err("OTA2 header missing token payload")
        );
    }

    #[test]
    fn v1_is_refused_when_armed() {
        assert_eq!(
            authorize(&MAGIC_V1, None, Some(&TOK)),
            Err("OTA1 refused: this build requires a token (use OTA2)")
        );
    }

    #[test]
    fn v2_token_ignored_when_unarmed() {
        assert!(authorize(&MAGIC_V2, Some(&TOK), None).is_ok());
        assert!(authorize(&MAGIC_V2, None, None).is_ok());
    }

    #[test]
    fn unknown_magic_refused() {
        assert_eq!(authorize(b"XXXX", None, None), Err("bad magic"));
    }
}
