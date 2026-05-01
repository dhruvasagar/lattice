//! Phase 2->3 ex-command parser.
//!
//! Parses `:` lines into one of two shapes (DESIGN.md §5.2.1):
//!
//! - [`Parsed::Invocation`]: a unified [`CommandInvocation`] that flows
//!   through `lattice_grammar::execute()` like every other vim chord.
//!   Used for every ex-command registered as an `ExCommandSpec` in the
//!   `CommandRegistry` (`:w`, `:q`, `:wq`, `:noh`, `:marks`, `:reg`,
//!   `:d`, `:set`, `:e`, ...).
//! - [`Parsed::Legacy`]: the v1 typed enum, retained only for
//!   `:s/.../.../` and `:g/.../...` (and `:v/.../...`) until those land
//!   on a structured args encoding. The host runs them through the
//!   pre-unification path (`App::execute_ex`).
//!
//! The split is the migration knob: as commands move into the registry
//! their `Parsed::Legacy` variants disappear. The legacy path is *only*
//! for delimiter-syntax commands today; every keyword command goes
//! through the registry.

use std::collections::HashMap;

use thiserror::Error;

use lattice_grammar::registry::CommandRegistry;
use lattice_grammar::CommandInvocation;

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Invocation(CommandInvocation),
    Legacy(ExCommand),
}

/// Legacy enum retained only for `:s/.../.../`, `:%s/.../.../`, and
/// `:g/.../.../`/`:v/.../.../` -- the delimiter-syntax commands that
/// haven't yet been moved to structured args in the registry. Every
/// other ex-command is a registered `ExCommandSpec` and never produces
/// this enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExCommand {
    Substitute {
        scope: SubstituteScope,
        pattern: String,
        replacement: String,
        global: bool,
    },
    /// `:g/pat/cmd` (and `:v/pat/cmd` with `inverted = true`).
    Global {
        pattern: String,
        inverted: bool,
        body: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstituteScope {
    CurrentLine,
    Whole,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExCommandError {
    #[error("empty command")]
    Empty,
    #[error("unknown command: {0}")]
    Unknown(String),
    #[error("trailing characters after command")]
    TrailingArgs,
    #[error("malformed substitute: {0}")]
    BadSubstitute(&'static str),
    #[error("invalid args: {0}")]
    BadArgs(String),
    #[error("`!` is not allowed for `{0}`")]
    BangNotAllowed(String),
}

/// Parse a `:` line into either a [`CommandInvocation`] (registry path)
/// or a legacy [`ExCommand`] (delimiter-syntax path). The caller picks
/// the dispatcher based on the variant.
pub fn parse(line: &str, registry: &CommandRegistry) -> Result<Parsed, ExCommandError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(ExCommandError::Empty);
    }

    // Delimiter-syntax commands (`:s/...`, `:%s/...`, `:g/...`,
    // `:v/...`) -- these don't fit the keyword-then-args shape, so they
    // bypass the registry path until structured args land.
    if let Some(sub) = parse_substitute(trimmed)? {
        return Ok(Parsed::Legacy(sub));
    }
    if let Some(g) = parse_global(trimmed)? {
        return Ok(Parsed::Legacy(g));
    }

    parse_invocation(trimmed, registry).map(Parsed::Invocation)
}

/// Parse the keyword form (`:cmd[!] [args]`) into a registry-bound
/// `CommandInvocation`. The caller has already filtered out the
/// delimiter-syntax cases.
fn parse_invocation(
    trimmed: &str,
    registry: &CommandRegistry,
) -> Result<CommandInvocation, ExCommandError> {
    // Split into command word and rest. The command word may end in `!`;
    // we strip it here and surface it as the bang bit.
    let (raw_cmd, rest) = match trimmed.find(char::is_whitespace) {
        Some(i) => (&trimmed[..i], trimmed[i..].trim()),
        None => (trimmed, ""),
    };
    let (cmd, bang) = if let Some(stripped) = raw_cmd.strip_suffix('!') {
        (stripped, true)
    } else {
        (raw_cmd, false)
    };

    let canonical = expand_alias(cmd).ok_or_else(|| ExCommandError::Unknown(raw_cmd.to_string()))?;
    let id = registry
        .id_by_name(canonical)
        .ok_or_else(|| ExCommandError::Unknown(raw_cmd.to_string()))?;

    let entry = registry
        .lookup(id)
        .ok_or_else(|| ExCommandError::Unknown(raw_cmd.to_string()))?;
    if entry.kind != lattice_grammar::CommandKind::ExCommand {
        // Only ex-commands are reachable from `:`. Motions / operators /
        // text objects are addressed through chord syntax, not `:cmd`.
        return Err(ExCommandError::Unknown(raw_cmd.to_string()));
    }

    let spec = ex_spec_for(registry, id).ok_or_else(|| ExCommandError::Unknown(raw_cmd.to_string()))?;
    if bang && !spec.accepts_bang {
        return Err(ExCommandError::BangNotAllowed(raw_cmd.to_string()));
    }
    let args = (spec.parse_args)(rest, bang).map_err(|e| match e {
        lattice_grammar::CommandError::BadArgs(msg) => ExCommandError::BadArgs(msg),
        other => ExCommandError::BadArgs(other.to_string()),
    })?;

    Ok(CommandInvocation::of(id)
        .with_args(args)
        .with_bang(bang))
}

/// Borrow the registered [`ExCommandSpec`] body by id. The registry's
/// `entry()` accessor is `pub(crate)`, so we pull the spec via a public
/// helper -- swapping for a real registry method is a one-liner once
/// the registry crate exposes one.
fn ex_spec_for(
    registry: &CommandRegistry,
    id: lattice_grammar::CommandId,
) -> Option<&lattice_grammar::ExCommandSpec> {
    registry.ex_command_spec(id)
}

/// Map a user-typed command word to the canonical name registered in
/// the `CommandRegistry`. Aliases live here -- not as duplicate registry
/// entries -- so that `:describe-command` and the command palette show
/// one row per command, not five.
fn expand_alias(cmd: &str) -> Option<&'static str> {
    static ALIASES: &[(&str, &str)] = &[
        ("w", "ex:write"),
        ("write", "ex:write"),
        ("q", "ex:quit"),
        ("quit", "ex:quit"),
        ("wq", "ex:write-quit"),
        ("x", "ex:write-quit"),
        ("noh", "ex:nohlsearch"),
        ("nohl", "ex:nohlsearch"),
        ("nohlsearch", "ex:nohlsearch"),
        ("reg", "ex:registers"),
        ("registers", "ex:registers"),
        ("marks", "ex:marks"),
        ("d", "ex:delete"),
        ("delete", "ex:delete"),
        ("set", "ex:set"),
        ("e", "ex:edit"),
        ("edit", "ex:edit"),
    ];
    // O(N) over a tiny static table -- cheaper than a lazy hashmap for
    // ~17 entries on a non-keystroke path.
    ALIASES.iter().find_map(|(short, canon)| (*short == cmd).then_some(*canon))
}

/// Built-in aliases as a `(short, canonical)` map. Exposed for tests
/// and any future `:describe-aliases` view.
pub fn aliases() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    for (s, c) in [
        ("w", "ex:write"),
        ("write", "ex:write"),
        ("q", "ex:quit"),
        ("quit", "ex:quit"),
        ("wq", "ex:write-quit"),
        ("x", "ex:write-quit"),
        ("noh", "ex:nohlsearch"),
        ("nohl", "ex:nohlsearch"),
        ("nohlsearch", "ex:nohlsearch"),
        ("reg", "ex:registers"),
        ("registers", "ex:registers"),
        ("marks", "ex:marks"),
        ("d", "ex:delete"),
        ("delete", "ex:delete"),
        ("set", "ex:set"),
        ("e", "ex:edit"),
        ("edit", "ex:edit"),
    ] {
        m.insert(s, c);
    }
    m
}

/// Parse vim's :g and :v: `g/pattern/body` and `v/pattern/body`.
fn parse_global(input: &str) -> Result<Option<ExCommand>, ExCommandError> {
    let (inverted, rest) = if let Some(rest) = input.strip_prefix("g/") {
        (false, rest)
    } else if let Some(rest) = input.strip_prefix("v/") {
        (true, rest)
    } else {
        return Ok(None);
    };
    // Walk forward to the next unescaped `/`.
    let mut pattern = String::new();
    let mut chars = rest.chars().peekable();
    let mut found_delim = false;
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                pattern.push(next);
            }
            continue;
        }
        if c == '/' {
            found_delim = true;
            break;
        }
        pattern.push(c);
    }
    if !found_delim {
        return Err(ExCommandError::BadSubstitute(
            "missing closing `/` after pattern",
        ));
    }
    if pattern.is_empty() {
        return Err(ExCommandError::BadSubstitute("empty pattern"));
    }
    let body: String = chars.collect();
    Ok(Some(ExCommand::Global {
        pattern,
        inverted,
        body,
    }))
}

/// Parse vim's substitute syntax: `[%]s/pattern/replacement/[flags]`.
/// Returns `Ok(Some)` on a successful s-form match, `Ok(None)` if the
/// input doesn't look like substitute at all, `Err(BadSubstitute)` on a
/// malformed s-form.
fn parse_substitute(input: &str) -> Result<Option<ExCommand>, ExCommandError> {
    // Detect `%s/.../.../...` and `s/.../.../...`.
    let (scope, body) = if let Some(rest) = input.strip_prefix("%s/") {
        (SubstituteScope::Whole, rest)
    } else if let Some(rest) = input.strip_prefix("s/") {
        (SubstituteScope::CurrentLine, rest)
    } else {
        return Ok(None);
    };

    // Walk `body` character-by-character respecting backslash-escapes.
    // Vim's substitute uses `\/` as an escape-for-delimiter; we accept
    // `\/` as a literal `/` in either pattern or replacement.
    let mut pattern = String::new();
    let mut replacement = String::new();
    let mut flags = String::new();
    let mut state = 0u8; // 0 = pattern, 1 = replacement, 2 = flags
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Take the next char literally (escape).
            if let Some(next) = chars.next() {
                let target = match state {
                    0 => &mut pattern,
                    1 => &mut replacement,
                    _ => &mut flags,
                };
                target.push(next);
            }
            continue;
        }
        if c == '/' {
            state += 1;
            if state > 2 {
                return Err(ExCommandError::BadSubstitute("too many `/` separators"));
            }
            continue;
        }
        let target = match state {
            0 => &mut pattern,
            1 => &mut replacement,
            _ => &mut flags,
        };
        target.push(c);
    }

    if pattern.is_empty() {
        return Err(ExCommandError::BadSubstitute("empty pattern"));
    }
    // 'g' is the only flag honored in v1; 'i', 'c', etc. are accepted
    // but ignored.
    let global = flags.contains('g');
    Ok(Some(ExCommand::Substitute {
        scope,
        pattern,
        replacement,
        global,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_grammar::{Args, CommandKind};

    fn fixture() -> CommandRegistry {
        let mut registry = CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut registry);
        let _ = lattice_grammar::ex_commands::populate(&mut registry);
        registry
    }

    fn invocation_name<'a>(
        inv: &'a CommandInvocation,
        registry: &'a CommandRegistry,
    ) -> &'a str {
        registry.lookup(inv.command).map(|s| s.name.as_str()).unwrap_or("?")
    }

    #[test]
    fn empty_input_is_empty_error() {
        let r = fixture();
        assert_eq!(parse("", &r), Err(ExCommandError::Empty));
        assert_eq!(parse("   ", &r), Err(ExCommandError::Empty));
    }

    #[test]
    fn write_short_form_routes_to_registry() {
        let r = fixture();
        let p = parse("w", &r).unwrap();
        match p {
            Parsed::Invocation(inv) => {
                assert_eq!(invocation_name(&inv, &r), "ex:write");
                assert!(!inv.bang);
                assert_eq!(inv.args, Args::None);
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn write_with_path_carries_string_arg() {
        let r = fixture();
        let p = parse("w foo.txt", &r).unwrap();
        match p {
            Parsed::Invocation(inv) => {
                assert_eq!(invocation_name(&inv, &r), "ex:write");
                assert_eq!(inv.args, Args::String("foo.txt".into()));
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn quit_bang_sets_bang_field() {
        let r = fixture();
        let p = parse("q!", &r).unwrap();
        match p {
            Parsed::Invocation(inv) => {
                assert_eq!(invocation_name(&inv, &r), "ex:quit");
                assert!(inv.bang);
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn quit_long_form_alias_resolves() {
        let r = fixture();
        let p = parse("quit", &r).unwrap();
        match p {
            Parsed::Invocation(inv) => assert_eq!(invocation_name(&inv, &r), "ex:quit"),
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn writequit_aliases_collapse_to_one_command() {
        let r = fixture();
        let wq = parse("wq", &r).unwrap();
        let x = parse("x", &r).unwrap();
        match (wq, x) {
            (Parsed::Invocation(a), Parsed::Invocation(b)) => {
                assert_eq!(a.command, b.command);
                assert_eq!(invocation_name(&a, &r), "ex:write-quit");
            }
            other => panic!("expected matching invocations, got {other:?}"),
        }
    }

    #[test]
    fn writequit_bang_propagates() {
        let r = fixture();
        let p = parse("wq!", &r).unwrap();
        match p {
            Parsed::Invocation(inv) => {
                assert!(inv.bang);
                assert_eq!(invocation_name(&inv, &r), "ex:write-quit");
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn unknown_command_reports_name() {
        let r = fixture();
        assert_eq!(
            parse("frobnicate", &r),
            Err(ExCommandError::Unknown("frobnicate".into()))
        );
    }

    #[test]
    fn bang_on_command_that_does_not_accept_bang_errors() {
        let r = fixture();
        // `:set!` is not valid -- accepts_bang = false.
        assert_eq!(
            parse("set!", &r),
            Err(ExCommandError::BangNotAllowed("set!".into()))
        );
    }

    #[test]
    fn parse_args_propagates_bad_args_error() {
        let r = fixture();
        // `:set` with no option string.
        let err = parse("set", &r).unwrap_err();
        assert!(matches!(err, ExCommandError::BadArgs(_)));
    }

    #[test]
    fn trailing_args_on_no_arg_command_surfaces_bad_args() {
        let r = fixture();
        // `:q please` -- parse_no_args rejects via BadArgs.
        let err = parse("q please", &r).unwrap_err();
        assert!(matches!(err, ExCommandError::BadArgs(_)));
    }

    #[test]
    fn substitute_remains_legacy_path() {
        let r = fixture();
        match parse("s/foo/bar/", &r).unwrap() {
            Parsed::Legacy(ExCommand::Substitute {
                scope,
                pattern,
                replacement,
                global,
            }) => {
                assert!(matches!(scope, SubstituteScope::CurrentLine));
                assert_eq!(pattern, "foo");
                assert_eq!(replacement, "bar");
                assert!(!global);
            }
            other => panic!("expected legacy Substitute, got {other:?}"),
        }
    }

    #[test]
    fn substitute_whole_buffer_remains_legacy_path() {
        let r = fixture();
        match parse("%s/foo/bar/g", &r).unwrap() {
            Parsed::Legacy(ExCommand::Substitute {
                scope,
                pattern,
                replacement,
                global,
            }) => {
                assert!(matches!(scope, SubstituteScope::Whole));
                assert_eq!(pattern, "foo");
                assert_eq!(replacement, "bar");
                assert!(global);
            }
            other => panic!("expected legacy Substitute, got {other:?}"),
        }
    }

    #[test]
    fn substitute_with_escaped_slash_in_pattern() {
        let r = fixture();
        match parse("s/a\\/b/c/", &r).unwrap() {
            Parsed::Legacy(ExCommand::Substitute { pattern, .. }) => assert_eq!(pattern, "a/b"),
            other => panic!("expected legacy Substitute, got {other:?}"),
        }
    }

    #[test]
    fn substitute_empty_pattern_is_error() {
        let r = fixture();
        assert!(matches!(
            parse("s//bar/", &r),
            Err(ExCommandError::BadSubstitute(_))
        ));
    }

    #[test]
    fn substitute_unknown_flag_is_ignored() {
        let r = fixture();
        match parse("s/foo/bar/gi", &r).unwrap() {
            Parsed::Legacy(ExCommand::Substitute { global, .. }) => assert!(global),
            other => panic!("expected legacy Substitute, got {other:?}"),
        }
    }

    #[test]
    fn global_basic_match_with_delete_body() {
        let r = fixture();
        match parse("g/foo/d", &r).unwrap() {
            Parsed::Legacy(ExCommand::Global {
                pattern,
                inverted,
                body,
            }) => {
                assert_eq!(pattern, "foo");
                assert!(!inverted);
                assert_eq!(body, "d");
            }
            other => panic!("expected legacy Global, got {other:?}"),
        }
    }

    #[test]
    fn vglobal_inverts_match() {
        let r = fixture();
        match parse("v/foo/d", &r).unwrap() {
            Parsed::Legacy(ExCommand::Global { inverted, .. }) => assert!(inverted),
            other => panic!("expected legacy Global, got {other:?}"),
        }
    }

    #[test]
    fn nohlsearch_aliases_route_to_one_command() {
        let r = fixture();
        for s in ["noh", "nohl", "nohlsearch"] {
            match parse(s, &r).unwrap() {
                Parsed::Invocation(inv) => {
                    assert_eq!(invocation_name(&inv, &r), "ex:nohlsearch");
                }
                other => panic!("expected Invocation for {s}, got {other:?}"),
            }
        }
    }

    #[test]
    fn registers_aliases_route_to_one_command() {
        let r = fixture();
        for s in ["reg", "registers"] {
            match parse(s, &r).unwrap() {
                Parsed::Invocation(inv) => {
                    assert_eq!(invocation_name(&inv, &r), "ex:registers");
                }
                other => panic!("expected Invocation for {s}, got {other:?}"),
            }
        }
    }

    #[test]
    fn edit_with_path() {
        let r = fixture();
        match parse("e foo.txt", &r).unwrap() {
            Parsed::Invocation(inv) => {
                assert_eq!(invocation_name(&inv, &r), "ex:edit");
                assert_eq!(inv.args, Args::String("foo.txt".into()));
                assert!(!inv.bang);
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn edit_force_with_bang() {
        let r = fixture();
        match parse("e! /tmp/x", &r).unwrap() {
            Parsed::Invocation(inv) => {
                assert_eq!(invocation_name(&inv, &r), "ex:edit");
                assert!(inv.bang);
                assert_eq!(inv.args, Args::String("/tmp/x".into()));
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn edit_without_path_is_reload() {
        let r = fixture();
        match parse("e", &r).unwrap() {
            Parsed::Invocation(inv) => {
                assert_eq!(invocation_name(&inv, &r), "ex:edit");
                assert_eq!(inv.args, Args::None);
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn set_with_option_parses() {
        let r = fixture();
        match parse("set number", &r).unwrap() {
            Parsed::Invocation(inv) => {
                assert_eq!(invocation_name(&inv, &r), "ex:set");
                assert_eq!(inv.args, Args::String("number".into()));
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn set_short_alias_with_value() {
        let r = fixture();
        match parse("set nu", &r).unwrap() {
            Parsed::Invocation(inv) => {
                assert_eq!(inv.args, Args::String("nu".into()));
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn marks_routes_to_registry() {
        let r = fixture();
        match parse("marks", &r).unwrap() {
            Parsed::Invocation(inv) => {
                assert_eq!(invocation_name(&inv, &r), "ex:marks");
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn delete_short_form_routes_to_registry() {
        let r = fixture();
        for s in ["d", "delete"] {
            match parse(s, &r).unwrap() {
                Parsed::Invocation(inv) => {
                    assert_eq!(invocation_name(&inv, &r), "ex:delete");
                }
                other => panic!("expected Invocation for {s}, got {other:?}"),
            }
        }
    }

    #[test]
    fn whitespace_around_command_is_tolerated() {
        let r = fixture();
        match parse("  w  ", &r).unwrap() {
            Parsed::Invocation(inv) => {
                assert_eq!(invocation_name(&inv, &r), "ex:write");
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
        match parse("\t w foo.rs \t", &r).unwrap() {
            Parsed::Invocation(inv) => {
                assert_eq!(invocation_name(&inv, &r), "ex:write");
                assert_eq!(inv.args, Args::String("foo.rs".into()));
            }
            other => panic!("expected Invocation, got {other:?}"),
        }
    }

    #[test]
    fn motion_name_in_registry_is_not_an_ex_command() {
        // `motion:word-forward` is registered but is not an ex-command;
        // it must surface as Unknown when typed at the `:` line. (No
        // alias maps to it -- this guards against a future regression
        // where someone adds `motion:word-forward` as an alias target.)
        let r = fixture();
        let id = r.id_by_name("motion:word-forward").unwrap();
        let entry = r.lookup(id).unwrap();
        assert_eq!(entry.kind, CommandKind::Motion);
    }

    #[test]
    fn aliases_table_is_self_consistent() {
        // Every alias points at a name registered in the registry.
        let r = fixture();
        for (short, canonical) in aliases() {
            assert!(
                r.id_by_name(canonical).is_some(),
                "alias `{short}` -> `{canonical}` not registered"
            );
        }
    }
}
