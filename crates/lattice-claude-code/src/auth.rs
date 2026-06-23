//! Connection authorization: the per-session token + a constant-time
//! header check.
//!
//! The security boundary is the loopback bind (`127.0.0.1`) plus this
//! token: the server writes a fresh token into the discovery lockfile,
//! and an attaching agent must echo it in the
//! `x-claude-code-ide-authorization` handshake header. The token is
//! compared in constant time so a local attacker can't time-side-channel
//! it byte by byte.

use crate::error::{ClaudeCodeError, Result};

/// The handshake header the agent must present, carrying the token read
/// from the discovery lockfile. Matches the VS Code IDE-integration
/// contract so the stock `claude` CLI authorizes unchanged.
pub const AUTH_HEADER: &str = "x-claude-code-ide-authorization";

/// Mint a fresh random auth token: 16 CSPRNG bytes rendered as 32 hex
/// characters. Loopback bind + this token are the only security boundary,
/// so the token must be unpredictable.
pub fn generate_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| ClaudeCodeError::Random(e.to_string()))?;
    let mut hex = String::with_capacity(32);
    for b in bytes {
        use std::fmt::Write as _;
        // Infallible write into a String.
        let _ = write!(hex, "{b:02x}");
    }
    Ok(hex)
}

/// Whether the provided header value matches the expected token, compared
/// in constant time. Returns `false` immediately on length mismatch —
/// tokens are fixed-length, so length is not secret.
pub fn header_matches(expected: &str, provided: &str) -> bool {
    constant_time_eq(expected.as_bytes(), provided.as_bytes())
}

/// Constant-time byte-slice equality. Leaks only the (non-secret) length;
/// the content comparison takes the same time regardless of where the
/// first differing byte is.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_token_is_32_hex_chars() {
        let t = generate_token().expect("token");
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn two_tokens_differ() {
        // Vanishingly unlikely to collide; guards against a constant token.
        let a = generate_token().expect("a");
        let b = generate_token().expect("b");
        assert_ne!(a, b);
    }

    #[test]
    fn matching_token_accepts() {
        let token = "deadbeefcafef00ddeadbeefcafef00d";
        assert!(header_matches(token, token));
    }

    #[test]
    fn wrong_token_rejects() {
        let token = "deadbeefcafef00ddeadbeefcafef00d";
        assert!(!header_matches(token, "deadbeefcafef00ddeadbeefcafef00e"));
    }

    #[test]
    fn length_mismatch_rejects() {
        assert!(!header_matches("short", "shorter-value"));
        assert!(!header_matches("", "x"));
    }

    #[test]
    fn empty_provided_rejects_nonempty_expected() {
        assert!(!header_matches("token", ""));
    }
}
