//! Phase 2->3 ex-command parser.
//!
//! Every `:`-line input becomes a [`CommandInvocation`] dispatched
//! through `lattice_grammar::execute()` (DESIGN.md §5.2.1). Two parse
//! shapes feed the same dispatcher:
//!
//! - **Keyword form** (`:w foo.txt`, `:q!`, `:set number`): split off
//!   the command word + optional bang, look up by alias in the
//!   registry, call the spec's `parse_args(rest, bang)` to get typed
//!   `Args`, build a `CommandInvocation`.
//! - **Delimiter form** (`:s/.../.../`, `:%s/.../.../`, `:g/.../.../`,
//!   `:v/.../.../`): the delimiter syntax doesn't fit the keyword
//!   parse, so the front-end parses the body itself and produces an
//!   `Args::List([pattern, replacement, flags])` for `:substitute` or
//!   `Args::List([pattern, inverted, body])` for `:global` (DESIGN.md
//!   §B.1, §B.2). The same dispatcher then resolves the registered
//!   command id and runs the matching apply closure.

use std::collections::HashMap;

use thiserror::Error;

use lattice_grammar::registry::CommandRegistry;
use lattice_grammar::{ArgValue, Args, CommandInvocation, Range};

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
    /// User typed the keyword form (`:ex:global`) of a command whose
    /// surface form is `Delimiter`. `name` is the canonical command
    /// name, `hint` is the syntax to use instead.
    #[error("`{name}` uses delimiter syntax; type `{hint}`")]
    WrongSurfaceForm { name: String, hint: &'static str },
}

/// Parse a `:` line into a [`CommandInvocation`] dispatchable through
/// the unified `grammar::execute()`.
pub fn parse(line: &str, registry: &CommandRegistry) -> Result<CommandInvocation, ExCommandError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(ExCommandError::Empty);
    }

    // Delimiter-syntax routes through the registry too -- the front-end
    // parses the body into Args::List.
    if let Some(inv) = try_parse_substitute(trimmed, registry)? {
        return Ok(inv);
    }
    if let Some(inv) = try_parse_global(trimmed, registry)? {
        return Ok(inv);
    }

    parse_invocation(trimmed, registry)
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

    // Resolution order: try the typed text directly as a registry
    // name first (so canonical names like `ex:describe-command`
    // resolve), then fall back to alias expansion (so user-friendly
    // shorthands like `describe-command` / `q` / `wq` resolve too).
    // Both forms reach the same `CommandSpec`; this is what lets
    // tab-completion's accepted candidates submit correctly
    // regardless of whether the candidate text was the canonical
    // form or the alias.
    let id = if let Some(id) = registry.id_by_name(cmd) {
        id
    } else if let Some(canonical) = expand_alias(cmd) {
        registry
            .id_by_name(canonical)
            .ok_or_else(|| ExCommandError::Unknown(raw_cmd.to_string()))?
    } else {
        return Err(ExCommandError::Unknown(raw_cmd.to_string()));
    };

    let entry = registry
        .lookup(id)
        .ok_or_else(|| ExCommandError::Unknown(raw_cmd.to_string()))?;
    if entry.kind != lattice_grammar::CommandKind::ExCommand {
        // Only ex-commands are reachable from `:`. Motions / operators /
        // text objects are addressed through chord syntax, not `:cmd`.
        return Err(ExCommandError::Unknown(raw_cmd.to_string()));
    }

    let spec =
        ex_spec_for(registry, id).ok_or_else(|| ExCommandError::Unknown(raw_cmd.to_string()))?;
    // Surface-form check before parse_args. Commands flagged
    // `Delimiter` (`:ex:substitute`, `:ex:global`) are not
    // keyword-invocable; the front-end's delimiter-detection path
    // (`try_parse_substitute` / `try_parse_global`) is the only
    // valid entry. Reaching here means the user typed the keyword
    // form by hand. Surface a precise error with the right syntax
    // hint instead of letting the call fall through to a generic
    // `parse_args` failure (which would say "invalid args").
    if let lattice_grammar::SurfaceForm::Delimiter { hint } = spec.surface_form {
        return Err(ExCommandError::WrongSurfaceForm {
            name: entry.name.clone(),
            hint,
        });
    }
    if bang && !spec.accepts_bang {
        return Err(ExCommandError::BangNotAllowed(raw_cmd.to_string()));
    }
    let args = (spec.parse_args)(rest, bang).map_err(|e| match e {
        lattice_grammar::CommandError::BadArgs(msg) => ExCommandError::BadArgs(msg),
        other => ExCommandError::BadArgs(other.to_string()),
    })?;

    Ok(CommandInvocation::of(id).with_args(args).with_bang(bang))
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
    ALIAS_TABLE
        .iter()
        .find_map(|(short, canon)| (*short == cmd).then_some(*canon))
}

/// Single source of truth for the alias table. `aliases()` exposes it
/// as a HashMap for tests and any future `:describe-aliases` view; the
/// hot path uses the slice directly via `expand_alias`.
static ALIAS_TABLE: &[(&str, &str)] = &[
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
    ("describe-command", "ex:describe-command"),
    ("describe-buffer", "ex:describe-buffer"),
    ("apropos", "ex:apropos"),
    ("describe-key", "ex:describe-key"),
    ("keymap", "ex:keymap"),
];

/// Built-in aliases as a `(short, canonical)` map. Exposed for tests
/// and any future `:describe-aliases` view.
pub fn aliases() -> HashMap<&'static str, &'static str> {
    ALIAS_TABLE.iter().copied().collect()
}

/// Reverse map of the alias table: for each canonical name, the
/// preferred user-facing alias (the longest one). Used by completion
/// to rewrite raw `gen:commands` output (which produces canonical
/// names like `ex:describe-command`) into the form a user actually
/// types (`describe-command`).
///
/// Picking the longest alias produces the most descriptive form
/// (`write` over `w`, `nohlsearch` over `noh`). Commands without
/// any alias map to themselves -- the canonical IS the user-facing
/// form.
pub fn preferred_alias_for(canonical: &str) -> Option<&'static str> {
    ALIAS_TABLE
        .iter()
        .filter(|(_, c)| *c == canonical)
        .map(|(short, _)| *short)
        .max_by_key(|s| s.len())
}

/// `:g/pattern/body` and `:v/pattern/body`. Produces a registered-
/// invocation pointing at `ex:global` with `Args::List([pattern,
/// inverted, body])`.
fn try_parse_global(
    input: &str,
    registry: &CommandRegistry,
) -> Result<Option<CommandInvocation>, ExCommandError> {
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
    if body.is_empty() {
        return Err(ExCommandError::BadSubstitute("empty body"));
    }
    // Parse the body as a CommandInvocation up front so the host
    // dispatches it per matching line without re-parsing, and body
    // syntax errors surface at `:g` parse time rather than mid-iteration.
    let body_inv = parse(&body, registry)?;
    let id = registry
        .id_by_name("ex:global")
        .ok_or_else(|| ExCommandError::Unknown("ex:global".into()))?;
    Ok(Some(CommandInvocation::of(id).with_args(Args::List(vec![
        ArgValue::Pattern(pattern),
        ArgValue::Bool(inverted),
        ArgValue::Invocation(Box::new(body_inv)),
    ]))))
}

/// Vim's `[%]s/pattern/replacement/[flags]`. Produces a
/// registered-invocation pointing at `ex:substitute` with the scope
/// expressed via `Range::CurrentLine` / `Range::Whole` and
/// `Args::List([pattern, replacement, flags])`.
/// Scope (current line vs. whole buffer) detected on the partial
/// or full `:s` / `:%s` form. Used by both the substitute parser
/// and the live-preview parser below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstitutePartialScope {
    CurrentLine,
    Whole,
}

/// Result of a best-effort parse of an in-progress substitute
/// command line, used by the live-preview path
/// (`refresh_substitute_preview` in App). Unlike the full
/// `try_parse_substitute` path, this never errors on incomplete
/// input -- a half-typed pattern or a missing second `/` is fine.
/// Returns `None` only when the input doesn't look like a
/// substitute at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstitutePartial {
    pub scope: SubstitutePartialScope,
    pub pattern: String,
    /// None when the user hasn't typed the second `/` yet (still
    /// inside the pattern field). `Some("")` is a typed second `/`
    /// with an empty replacement so far.
    pub replacement: Option<String>,
    /// None when the user hasn't typed the third `/` yet. `Some("")`
    /// is a typed third `/` with no flags so far. The flags string
    /// is opaque -- the live preview only uses pattern + replacement.
    pub flags: Option<String>,
}

/// Best-effort parse of a partial `:s` / `:%s` command line. Used by
/// the live-preview path: as the user types, we want to highlight
/// matches of the in-progress pattern even before the second `/` or
/// closing `/` is typed. Backslash-escapes are honored the same way
/// `try_parse_substitute` honors them, so a partial `\/` doesn't
/// flip the field state mid-stream.
pub fn try_parse_substitute_partial(input: &str) -> Option<SubstitutePartial> {
    let (scope, body) = if let Some(rest) = input.strip_prefix("%s/") {
        (SubstitutePartialScope::Whole, rest)
    } else if let Some(rest) = input.strip_prefix("s/") {
        (SubstitutePartialScope::CurrentLine, rest)
    } else {
        return None;
    };

    let mut pattern = String::new();
    let mut replacement: Option<String> = None;
    let mut flags: Option<String> = None;
    let mut state = 0u8; // 0 = pattern, 1 = replacement, 2 = flags
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                match state {
                    0 => pattern.push(next),
                    1 => replacement.get_or_insert_with(String::new).push(next),
                    _ => flags.get_or_insert_with(String::new).push(next),
                }
            } else {
                // Trailing `\` with nothing after -- mid-input.
                // Treat as a literal in the current field so the
                // user sees their typing reflected.
                match state {
                    0 => pattern.push('\\'),
                    1 => replacement.get_or_insert_with(String::new).push('\\'),
                    _ => flags.get_or_insert_with(String::new).push('\\'),
                }
            }
            continue;
        }
        if c == '/' {
            state += 1;
            if state == 1 {
                replacement = Some(String::new());
            } else if state == 2 {
                flags = Some(String::new());
            }
            // Extra `/` past the flags field is just absorbed into
            // the flags string -- the full parser would reject it,
            // but for a live preview we don't care.
            if state > 2 {
                flags.get_or_insert_with(String::new).push('/');
                state = 2;
            }
            continue;
        }
        match state {
            0 => pattern.push(c),
            1 => replacement.get_or_insert_with(String::new).push(c),
            _ => flags.get_or_insert_with(String::new).push(c),
        }
    }

    Some(SubstitutePartial {
        scope,
        pattern,
        replacement,
        flags,
    })
}

fn try_parse_substitute(
    input: &str,
    registry: &CommandRegistry,
) -> Result<Option<CommandInvocation>, ExCommandError> {
    let (range, body) = if let Some(rest) = input.strip_prefix("%s/") {
        (Range::Whole, rest)
    } else if let Some(rest) = input.strip_prefix("s/") {
        (Range::CurrentLine, rest)
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
    let id = registry
        .id_by_name("ex:substitute")
        .ok_or_else(|| ExCommandError::Unknown("ex:substitute".into()))?;
    Ok(Some(CommandInvocation::of(id).with_range(range).with_args(
        Args::List(vec![
            ArgValue::Pattern(pattern),
            ArgValue::String(replacement),
            ArgValue::String(flags),
        ]),
    )))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_grammar::CommandKind;

    fn fixture() -> CommandRegistry {
        let mut registry = CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut registry);
        let _ = lattice_grammar::ex_commands::populate(&mut registry);
        registry
    }

    fn invocation_name<'a>(inv: &'a CommandInvocation, registry: &'a CommandRegistry) -> &'a str {
        registry
            .lookup(inv.command)
            .map(|s| s.name.as_str())
            .unwrap_or("?")
    }

    // ---- Substitute live-preview parser ----

    #[test]
    fn partial_substitute_with_pattern_only() {
        let p = try_parse_substitute_partial("s/foo").unwrap();
        assert_eq!(p.scope, SubstitutePartialScope::CurrentLine);
        assert_eq!(p.pattern, "foo");
        assert_eq!(p.replacement, None);
        assert_eq!(p.flags, None);
    }

    #[test]
    fn partial_substitute_with_pattern_and_typed_delimiter() {
        // Typed second `/` -- replacement is Some("") even before
        // the user types any replacement chars.
        let p = try_parse_substitute_partial("s/foo/").unwrap();
        assert_eq!(p.pattern, "foo");
        assert_eq!(p.replacement.as_deref(), Some(""));
        assert_eq!(p.flags, None);
    }

    #[test]
    fn partial_substitute_with_replacement_in_progress() {
        let p = try_parse_substitute_partial("s/foo/bar").unwrap();
        assert_eq!(p.pattern, "foo");
        assert_eq!(p.replacement.as_deref(), Some("bar"));
    }

    #[test]
    fn partial_substitute_with_flags() {
        let p = try_parse_substitute_partial("%s/foo/bar/g").unwrap();
        assert_eq!(p.scope, SubstitutePartialScope::Whole);
        assert_eq!(p.pattern, "foo");
        assert_eq!(p.replacement.as_deref(), Some("bar"));
        assert_eq!(p.flags.as_deref(), Some("g"));
    }

    #[test]
    fn partial_substitute_rejects_non_substitute_input() {
        assert!(try_parse_substitute_partial("write").is_none());
        assert!(try_parse_substitute_partial("/foo").is_none());
        assert!(try_parse_substitute_partial("g/foo/d").is_none());
    }

    #[test]
    fn partial_substitute_honors_backslash_escape_in_pattern() {
        // `\/` is an escaped delimiter -- still part of the pattern.
        let p = try_parse_substitute_partial(r"s/foo\/bar").unwrap();
        assert_eq!(p.pattern, "foo/bar");
        assert_eq!(p.replacement, None);
    }

    #[test]
    fn empty_input_is_empty_error() {
        let r = fixture();
        assert_eq!(parse("", &r).unwrap_err(), ExCommandError::Empty);
        assert_eq!(parse("   ", &r).unwrap_err(), ExCommandError::Empty);
    }

    #[test]
    fn write_short_form_routes_to_registry() {
        let r = fixture();
        let inv = parse("w", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:write");
        assert!(!inv.bang);
        assert_eq!(inv.args, Args::None);
    }

    #[test]
    fn write_with_path_carries_string_arg() {
        let r = fixture();
        let inv = parse("w foo.txt", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:write");
        assert_eq!(inv.args, Args::String("foo.txt".into()));
    }

    #[test]
    fn quit_bang_sets_bang_field() {
        let r = fixture();
        let inv = parse("q!", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:quit");
        assert!(inv.bang);
    }

    #[test]
    fn quit_long_form_alias_resolves() {
        let r = fixture();
        let inv = parse("quit", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:quit");
    }

    #[test]
    fn writequit_aliases_collapse_to_one_command() {
        let r = fixture();
        let wq = parse("wq", &r).unwrap();
        let x = parse("x", &r).unwrap();
        assert_eq!(wq.command, x.command);
        assert_eq!(invocation_name(&wq, &r), "ex:write-quit");
    }

    #[test]
    fn writequit_bang_propagates() {
        let r = fixture();
        let inv = parse("wq!", &r).unwrap();
        assert!(inv.bang);
        assert_eq!(invocation_name(&inv, &r), "ex:write-quit");
    }

    #[test]
    fn unknown_command_reports_name() {
        let r = fixture();
        assert_eq!(
            parse("frobnicate", &r).unwrap_err(),
            ExCommandError::Unknown("frobnicate".into())
        );
    }

    #[test]
    fn bang_on_command_that_does_not_accept_bang_errors() {
        let r = fixture();
        // `:set!` is not valid -- accepts_bang = false.
        assert_eq!(
            parse("set!", &r).unwrap_err(),
            ExCommandError::BangNotAllowed("set!".into())
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

    // ---- Substitute / global: registry-routed delimiter form ----

    #[test]
    fn substitute_current_line_produces_invocation_with_args_list() {
        let r = fixture();
        let inv = parse("s/foo/bar/", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:substitute");
        assert_eq!(inv.range, Some(Range::CurrentLine));
        let list = inv.args.as_list().expect("expected Args::List");
        assert_eq!(list.len(), 3);
        assert_eq!(list[0], ArgValue::Pattern("foo".into()));
        assert_eq!(list[1], ArgValue::String("bar".into()));
        assert_eq!(list[2], ArgValue::String(String::new()));
    }

    #[test]
    fn substitute_whole_buffer_sets_range_whole() {
        let r = fixture();
        let inv = parse("%s/foo/bar/g", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:substitute");
        assert_eq!(inv.range, Some(Range::Whole));
        let list = inv.args.as_list().unwrap();
        assert_eq!(list[2], ArgValue::String("g".into()));
    }

    #[test]
    fn substitute_with_escaped_slash_in_pattern() {
        let r = fixture();
        let inv = parse("s/a\\/b/c/", &r).unwrap();
        let list = inv.args.as_list().unwrap();
        assert_eq!(list[0], ArgValue::Pattern("a/b".into()));
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
    fn substitute_flags_string_carries_through() {
        let r = fixture();
        let inv = parse("s/foo/bar/gi", &r).unwrap();
        let list = inv.args.as_list().unwrap();
        // Apply closure interprets the flag string; the parser just
        // hands it through verbatim.
        assert_eq!(list[2], ArgValue::String("gi".into()));
    }

    #[test]
    fn global_basic_match_with_delete_body() {
        let r = fixture();
        let inv = parse("g/foo/d", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:global");
        let list = inv.args.as_list().unwrap();
        assert_eq!(list[0], ArgValue::Pattern("foo".into()));
        assert_eq!(list[1], ArgValue::Bool(false));
        // Body is parsed up front -- arg 2 is an Invocation pointing
        // at the resolved `:d` command, not a Raw string.
        let body = list[2].as_invocation().expect("body should be parsed");
        assert_eq!(invocation_name(body, &r), "ex:delete");
    }

    #[test]
    fn vglobal_inverts_match() {
        let r = fixture();
        let inv = parse("v/foo/d", &r).unwrap();
        let list = inv.args.as_list().unwrap();
        assert_eq!(list[1], ArgValue::Bool(true));
    }

    #[test]
    fn global_body_can_be_substitute() {
        // Nested delimiter command in the body is parsed up front;
        // the resulting Invocation points at `:s` with its own args.
        let r = fixture();
        let inv = parse("g/foo/s/a/b/g", &r).unwrap();
        let list = inv.args.as_list().unwrap();
        let body = list[2].as_invocation().expect("body should be parsed");
        assert_eq!(invocation_name(body, &r), "ex:substitute");
        let body_list = body.args.as_list().unwrap();
        assert_eq!(body_list[0], ArgValue::Pattern("a".into()));
        assert_eq!(body_list[1], ArgValue::String("b".into()));
        assert_eq!(body_list[2], ArgValue::String("g".into()));
    }

    #[test]
    fn global_with_unparseable_body_errors_at_parse_time() {
        // The body must parse against the registry; an unknown
        // command surfaces immediately, not after `:g` matches.
        let r = fixture();
        let result = parse("g/foo/this-is-not-a-real-command", &r);
        assert!(
            matches!(result, Err(ExCommandError::Unknown(_))),
            "expected Unknown error, got {result:?}"
        );
    }

    #[test]
    fn global_with_empty_body_errors_at_parse_time() {
        let r = fixture();
        let result = parse("g/foo/", &r);
        assert!(matches!(result, Err(ExCommandError::BadSubstitute(_))));
    }

    #[test]
    fn nohlsearch_aliases_route_to_one_command() {
        let r = fixture();
        for s in ["noh", "nohl", "nohlsearch"] {
            let inv = parse(s, &r).unwrap();
            assert_eq!(invocation_name(&inv, &r), "ex:nohlsearch", "alias `{s}`");
        }
    }

    #[test]
    fn registers_aliases_route_to_one_command() {
        let r = fixture();
        for s in ["reg", "registers"] {
            let inv = parse(s, &r).unwrap();
            assert_eq!(invocation_name(&inv, &r), "ex:registers");
        }
    }

    #[test]
    fn edit_with_path() {
        let r = fixture();
        let inv = parse("e foo.txt", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:edit");
        assert_eq!(inv.args, Args::String("foo.txt".into()));
        assert!(!inv.bang);
    }

    #[test]
    fn edit_force_with_bang() {
        let r = fixture();
        let inv = parse("e! /tmp/x", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:edit");
        assert!(inv.bang);
        assert_eq!(inv.args, Args::String("/tmp/x".into()));
    }

    #[test]
    fn edit_without_path_is_reload() {
        let r = fixture();
        let inv = parse("e", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:edit");
        assert_eq!(inv.args, Args::None);
    }

    #[test]
    fn set_with_option_parses() {
        let r = fixture();
        let inv = parse("set number", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:set");
        assert_eq!(inv.args, Args::String("number".into()));
    }

    #[test]
    fn set_short_alias_with_value() {
        let r = fixture();
        let inv = parse("set nu", &r).unwrap();
        assert_eq!(inv.args, Args::String("nu".into()));
    }

    #[test]
    fn marks_routes_to_registry() {
        let r = fixture();
        let inv = parse("marks", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:marks");
    }

    #[test]
    fn delete_short_form_routes_to_registry() {
        let r = fixture();
        for s in ["d", "delete"] {
            let inv = parse(s, &r).unwrap();
            assert_eq!(invocation_name(&inv, &r), "ex:delete");
        }
    }

    #[test]
    fn whitespace_around_command_is_tolerated() {
        let r = fixture();
        let inv = parse("  w  ", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:write");
        let inv = parse("\t w foo.rs \t", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:write");
        assert_eq!(inv.args, Args::String("foo.rs".into()));
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

    #[test]
    fn substitute_and_global_register_with_args_schema() {
        // §B.1: ex:substitute and ex:global advertise their structured
        // shape via args_schema. A future :describe-command consumes
        // this; in v1 it's used by tests as a smoke check.
        let r = fixture();
        let sub_id = r.id_by_name("ex:substitute").unwrap();
        let sub = r.lookup(sub_id).unwrap();
        assert_eq!(sub.args_schema.len(), 3);
        assert_eq!(sub.args_schema[0].name, "pattern");
        assert_eq!(sub.args_schema[1].name, "replacement");
        assert_eq!(sub.args_schema[2].name, "flags");

        let global_id = r.id_by_name("ex:global").unwrap();
        let global = r.lookup(global_id).unwrap();
        assert_eq!(global.args_schema.len(), 3);
        assert_eq!(global.args_schema[0].name, "pattern");
        assert_eq!(global.args_schema[1].name, "inverted");
        assert_eq!(global.args_schema[2].name, "body");
    }

    #[test]
    fn keyword_form_of_substitute_returns_wrong_surface_form_error() {
        // The user types `:ex:substitute foo`. Surface-form check
        // fires before parse_args; the error names the canonical
        // command and the syntax to use.
        let r = fixture();
        let err = parse("ex:substitute foo bar", &r).unwrap_err();
        match err {
            ExCommandError::WrongSurfaceForm { name, hint } => {
                assert_eq!(name, "ex:substitute");
                assert!(hint.contains(":s/"));
            }
            other => panic!("expected WrongSurfaceForm, got {other:?}"),
        }
    }

    #[test]
    fn keyword_form_of_global_returns_wrong_surface_form_error() {
        let r = fixture();
        let err = parse("ex:global //d", &r).unwrap_err();
        match err {
            ExCommandError::WrongSurfaceForm { name, hint } => {
                assert_eq!(name, "ex:global");
                assert!(hint.contains(":g/"));
                assert!(hint.contains(":v/"));
            }
            other => panic!("expected WrongSurfaceForm, got {other:?}"),
        }
    }

    #[test]
    fn delimiter_form_of_substitute_still_parses() {
        // Surface-form gating must NOT break the front-end delimiter
        // path -- the gate fires only for the keyword route.
        let r = fixture();
        let inv = parse("s/foo/bar/", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:substitute");
    }

    #[test]
    fn delimiter_form_of_global_still_parses() {
        let r = fixture();
        let inv = parse("g/foo/d", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:global");
    }
}
