//! Ex-command parsing helpers shared by every agent adapter.

use lattice_grammar::args::Args;
use lattice_grammar::error::{CommandError, GrammarResult};

/// Reject any trailing characters; these commands take no arguments.
pub fn parse_no_args(rest: &str, _bang: bool) -> GrammarResult<Args> {
    if rest.trim().is_empty() {
        Ok(Args::None)
    } else {
        Err(CommandError::BadArgs(
            "trailing characters after command".into(),
        ))
    }
}

/// Take the rest of the line verbatim (trimmed) as a single string arg.
pub fn parse_rest_as_text(rest: &str, _bang: bool) -> GrammarResult<Args> {
    Ok(Args::String(rest.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_accepts_empty_and_whitespace() {
        assert_eq!(parse_no_args("", false).expect("empty is ok"), Args::None);
        assert_eq!(
            parse_no_args("   ", false).expect("whitespace is ok"),
            Args::None
        );
    }

    #[test]
    fn no_args_rejects_trailing_characters() {
        assert!(parse_no_args("junk", false).is_err());
    }

    #[test]
    fn rest_as_text_trims_and_keeps_inner_spaces() {
        assert_eq!(
            parse_rest_as_text("  hello  world  ", false).expect("always ok"),
            Args::String("hello  world".to_string())
        );
        assert_eq!(
            parse_rest_as_text("", false).expect("always ok"),
            Args::String(String::new())
        );
    }
}
