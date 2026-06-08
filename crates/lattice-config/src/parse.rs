//! `:set` command-line syntax parser. Lifted from
//! `lattice-ui-tui::options` and decoupled from any specific
//! option-value type — the syntax is the same regardless of the
//! underlying typed system.
//!
//! Forms accepted:
//! - `name` -- query / boolean-on.
//! - `name?` -- always print current value.
//! - `name=value` -- set typed value (registry parses against the
//!   option's [`crate::OptionType`]).
//! - `noname` -- clear boolean.
//!
//! Multiple-option forms (`:set ic hls scs`) are deferred; one
//! `parse_set` call handles one option per dispatch today.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedSet {
    /// `:set name` -- query (non-bool) / boolean-on (bool).
    NameOnly(String),
    /// `:set name?` -- always print current value.
    Query(String),
    /// `:set name=value` -- set typed value.
    Assign { name: String, value: String },
    /// `:set noname` -- clear boolean.
    Negate(String),
    /// `:set name&` / `:setlocal name&` -- reset to registered default
    /// / clear local override. Empty name means "clear all" (`:setlocal &`).
    Reset(String),
}

pub fn parse_set(input: &str) -> Result<ParsedSet, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty :set".into());
    }
    if let Some(eq) = trimmed.find('=') {
        let name = trimmed[..eq].trim().to_string();
        let value = trimmed[eq + 1..].trim().to_string();
        if name.is_empty() {
            return Err("empty option name".into());
        }
        return Ok(ParsedSet::Assign { name, value });
    }
    if let Some(name) = trimmed.strip_suffix('?') {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("empty option name".into());
        }
        return Ok(ParsedSet::Query(name));
    }
    if let Some(name) = trimmed.strip_suffix('&') {
        // `:set name&` resets to default; `:setlocal &` (bare `&`) clears all.
        return Ok(ParsedSet::Reset(name.trim().to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("no") {
        // Every `no...` form is a candidate negation; the registry
        // bounces it through `ErasedOption::negate()` which rejects
        // non-bool options.
        return Ok(ParsedSet::Negate(rest.to_string()));
    }
    Ok(ParsedSet::NameOnly(trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn parse_set_name_only() {
        assert_eq!(
            parse_set("number").unwrap(),
            ParsedSet::NameOnly("number".into())
        );
    }

    #[test]
    fn parse_set_assign() {
        assert_eq!(
            parse_set("tabstop=4").unwrap(),
            ParsedSet::Assign {
                name: "tabstop".into(),
                value: "4".into()
            }
        );
    }

    #[test]
    fn parse_set_negate() {
        assert_eq!(
            parse_set("nonumber").unwrap(),
            ParsedSet::Negate("number".into())
        );
    }

    #[test]
    fn parse_set_query() {
        assert_eq!(
            parse_set("number?").unwrap(),
            ParsedSet::Query("number".into())
        );
    }

    #[test]
    fn parse_set_assign_trims_whitespace_around_eq() {
        assert_eq!(
            parse_set("tabstop = 4").unwrap(),
            ParsedSet::Assign {
                name: "tabstop".into(),
                value: "4".into()
            }
        );
    }

    #[test]
    fn parse_set_assign_value_can_contain_chars_after_eq() {
        // `:set foldmethod=indent`
        assert_eq!(
            parse_set("foldmethod=indent").unwrap(),
            ParsedSet::Assign {
                name: "foldmethod".into(),
                value: "indent".into()
            }
        );
    }

    #[test]
    fn parse_set_empty_errors() {
        assert!(parse_set("   ").is_err());
        assert!(parse_set("").is_err());
    }

    #[test]
    fn parse_set_empty_name_errors() {
        assert!(parse_set("=4").is_err());
        assert!(parse_set("?").is_err());
    }
}
