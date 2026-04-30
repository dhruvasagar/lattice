//! Per-command argument values.
//!
//! Each registered command declares an `args_schema`; the dispatcher carries
//! the matching values here. `Args::None` is the universal "no args" form.
//!
//! This is a small typed enum, not a dynamic value bag, so callers get static
//! type checks at the boundary. Plugin-supplied commands that need richer
//! arguments encode them via `Args::Bytes` (msgpack on the wire) for now;
//! when WASM lands, the WIT-typed args replace this byte form.

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub enum Args {
    #[default]
    None,
    Char(char),
    String(String),
    Bytes(Vec<u8>),
}

impl Args {
    pub fn is_none(&self) -> bool {
        matches!(self, Args::None)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn default_is_none() {
        assert_eq!(Args::default(), Args::None);
        assert!(Args::None.is_none());
    }

    #[test]
    fn char_carries_character() {
        let a = Args::Char('x');
        assert!(!a.is_none());
        match a {
            Args::Char(c) => assert_eq!(c, 'x'),
            _ => panic!("expected Char"),
        }
    }

    #[test]
    fn string_args_round_trip() {
        let a = Args::String("hello".into());
        let json = serde_json::to_string(&a).unwrap();
        let back: Args = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }
}
