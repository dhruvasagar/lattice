//! Canonical synthetic buffer name for the per-process AI log
//! family (AI-1b).
//!
//! One flavour: `*ai:<provider>:<index>*` -- one buffer per AI
//! agent session, keyed by [`SessionKey`]. Mirrors
//! `lattice-lsp`'s per-instance `*lsp:<server>:<workspace>*`
//! naming, one level simpler (no trace-variant sibling).
//!
//! The inverse helper ([`parse_ai_log_name`]) extracts the
//! originating [`SessionKey`] from a synthetic name so
//! `AiLogMode::on_activate` can derive its identity from its
//! buffer's name alone -- no buffer-local seeding required.
//!
//! ## Invariant
//!
//! Provider names never contain `:` (`opencode`, `claude`,
//! `gemini`). The parser splits on the first colon after `*ai:`;
//! everything after that up to the trailing `*` is the index.

use std::sync::Arc;

use super::ai_log::SessionKey;

/// Build the synthetic name for the per-session AI log buffer
/// owned by [`super::modes::AiLogMode`].
///
/// Format: `*ai:<provider>:<index>*`.
pub fn ai_log_name(session: &SessionKey) -> String {
    format!("*ai:{}:{}*", session.provider, session.index)
}

/// Recover the [`SessionKey`] from a per-session log buffer
/// name. Returns `None` when `name` does not match the
/// canonical `*ai:<provider>:<index>*` shape.
pub fn parse_ai_log_name(name: &str) -> Option<SessionKey> {
    let body = name.strip_prefix("*ai:")?.strip_suffix('*')?;
    if body.is_empty() {
        return None;
    }
    let (provider, index_str) = body.split_once(':')?;
    if provider.is_empty() {
        return None;
    }
    let index = index_str.parse::<u32>().ok()?;
    Some(SessionKey::new(Arc::<str>::from(provider), index))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(provider: &str, index: u32) -> SessionKey {
        SessionKey::new(Arc::<str>::from(provider), index)
    }

    #[test]
    fn name_round_trips() {
        let k = key("opencode", 1);
        let name = ai_log_name(&k);
        assert_eq!(name, "*ai:opencode:1*");
        let parsed = parse_ai_log_name(&name).expect("parse");
        assert_eq!(parsed, k);
    }

    #[test]
    fn name_round_trips_second_index() {
        let k = key("opencode", 2);
        let name = ai_log_name(&k);
        assert_eq!(name, "*ai:opencode:2*");
        let parsed = parse_ai_log_name(&name).expect("parse");
        assert_eq!(parsed, k);
    }

    #[test]
    fn parser_rejects_garbage_names() {
        assert!(parse_ai_log_name("*foo*").is_none());
        assert!(parse_ai_log_name("*ai:*").is_none());
        assert!(parse_ai_log_name("*ai::1*").is_none());
        assert!(parse_ai_log_name("*ai:opencode:*").is_none());
        assert!(parse_ai_log_name("*ai:opencode:x*").is_none());
        assert!(parse_ai_log_name("not-ai").is_none());
    }
}
