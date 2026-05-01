//! Built-in ex-commands registered as peers of motions / operators / text
//! objects in the unified `CommandRegistry` (DESIGN.md §5.2.1).
//!
//! Each spec's `apply` callback is intentionally thin: it only packages
//! the parsed args into the matching [`Effect`] variant. The host (`App`)
//! owns the side-effect implementation (file I/O, view options, echo
//! area, document swap, ...) -- this keeps the closures static-state-free
//! so they can later be loaded from a WASM plugin without redesign.
//!
//! Coverage:
//! - Keyword form: `:w[rite]`, `:q[uit]`, `:wq`/`:x`, `:noh[lsearch]`,
//!   `:reg[isters]`, `:marks`, `:d[elete]`, `:set`, `:e[dit]`.
//! - Delimiter-syntax form (Appendix B.2): `:s/.../.../[g]`,
//!   `:%s/.../.../[g]`, `:g/.../.../`, `:v/.../.../`. These use
//!   `Args::List` to carry pattern / replacement / flags / body /
//!   inverted as positional `ArgValue`s; the parser front-end strips
//!   the delimiter prefix and dispatches through the same
//!   `grammar::execute()` as everything else.
//!
//! Aliases (`:w` for `:write`, `:q` for `:quit`, `:e` for `:edit`, ...)
//! are NOT separate registry entries -- they would inflate the
//! `CommandId` namespace and complicate `:describe-command`. Alias
//! resolution is the parser front-end's job (`expand_alias` in
//! `lattice-ui-tui::excommand`).

use crate::args::{ArgDefault, ArgKind, ArgSpec, ArgValue, Args};
use crate::effect::{Effect, SubstituteScope};
use crate::error::{CommandError, GrammarResult};
use crate::range::Range;
use crate::registry::{CommandRegistry, ExCommandContext, ExCommandId, ExCommandSpec};

/// Set of registered ex-command ids; mirrors the `Builtins` shape for
/// motions / operators / text objects.
#[derive(Debug, Clone, Copy)]
pub struct ExBuiltins {
    pub write: ExCommandId,
    pub quit: ExCommandId,
    pub write_quit: ExCommandId,
    pub no_hlsearch: ExCommandId,
    pub list_registers: ExCommandId,
    pub list_marks: ExCommandId,
    pub delete_line: ExCommandId,
    pub set_option: ExCommandId,
    pub edit: ExCommandId,
    pub substitute: ExCommandId,
    pub global: ExCommandId,
    pub describe_command: ExCommandId,
    pub describe_buffer: ExCommandId,
    pub apropos: ExCommandId,
}

pub fn populate(registry: &mut CommandRegistry) -> ExBuiltins {
    let write = registry.register_ex_command(
        "ex:write",
        "Write the current buffer to disk (`:w [path]`).",
        ExCommandSpec {
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(apply_write),
            args_schema: vec![],
        },
    );
    let quit = registry.register_ex_command(
        "ex:quit",
        "Quit the editor (`:q[!]`).",
        ExCommandSpec {
            accepts_bang: true,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(apply_quit),
            args_schema: vec![],
        },
    );
    let write_quit = registry.register_ex_command(
        "ex:write-quit",
        "Write the current buffer and quit (`:wq[!]` / `:x[!]`).",
        ExCommandSpec {
            accepts_bang: true,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(apply_write_quit),
            args_schema: vec![],
        },
    );
    let no_hlsearch = registry.register_ex_command(
        "ex:nohlsearch",
        "Clear the search-highlight overlay (`:noh[lsearch]`).",
        ExCommandSpec {
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::ClearSearchHighlight)),
            args_schema: vec![],
        },
    );
    let list_registers = registry.register_ex_command(
        "ex:registers",
        "Show every register's contents (`:reg[isters]`).",
        ExCommandSpec {
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::EchoRegisters)),
            args_schema: vec![],
        },
    );
    let list_marks = registry.register_ex_command(
        "ex:marks",
        "Show every set mark's name + position (`:marks`).",
        ExCommandSpec {
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::EchoMarks)),
            args_schema: vec![],
        },
    );
    let delete_line = registry.register_ex_command(
        "ex:delete",
        "Delete the current line including its newline (`:d[elete]`).",
        ExCommandSpec {
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::DeleteCurrentLine)),
            args_schema: vec![],
        },
    );
    let set_option = registry.register_ex_command(
        "ex:set",
        "Set a view option (`:set <option>`).",
        ExCommandSpec {
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(apply_set),
            args_schema: vec![],
        },
    );
    let edit = registry.register_ex_command(
        "ex:edit",
        "Load a file into the current document (`:e[!] [path]`).",
        ExCommandSpec {
            accepts_bang: true,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(apply_edit),
            args_schema: vec![],
        },
    );
    let substitute = registry.register_ex_command(
        "ex:substitute",
        "Replace pattern with replacement on the current line or `%` whole buffer (`:s/pat/rep/[g]`).",
        ExCommandSpec {
            accepts_bang: false,
            // `accepts_range: true` even though v1 only honours
            // CurrentLine and Whole; the parser front-end provides the
            // range from the `s/` vs `%s/` prefix.
            accepts_range: true,
            // The substitute call enters via the parser front-end's
            // delimiter detection, not the keyword form -- parse_args
            // is unreachable for normal `:`-line input. We keep a
            // stub that errors on direct use to prevent surprise from
            // a script invocation.
            parse_args: Box::new(parse_substitute_args_unreachable),
            apply: Box::new(apply_substitute),
            args_schema: vec![
                ArgSpec::required(
                    "pattern",
                    ArgKind::Pattern,
                    "Search pattern (literal in v1; regex post-1.0)",
                ),
                ArgSpec::required(
                    "replacement",
                    ArgKind::String,
                    "Replacement text (empty deletes matches)",
                ),
                ArgSpec {
                    name: "flags",
                    kind: ArgKind::String,
                    doc: "Flags string (currently `g` honoured; others ignored)",
                    prompt: "",
                    default: ArgDefault::Literal(ArgValue::String(String::new())),
                },
            ],
        },
    );
    let global = registry.register_ex_command(
        "ex:global",
        "Run a command on every line matching (`:g`) or NOT matching (`:v`) a pattern.",
        ExCommandSpec {
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_global_args_unreachable),
            apply: Box::new(apply_global),
            args_schema: vec![
                ArgSpec::required(
                    "pattern",
                    ArgKind::Pattern,
                    "Match pattern (literal in v1)",
                ),
                ArgSpec {
                    name: "inverted",
                    kind: ArgKind::Bool,
                    doc: "True for `:v` form -- match lines NOT matching the pattern.",
                    prompt: "",
                    default: ArgDefault::Literal(ArgValue::Bool(false)),
                },
                ArgSpec::required(
                    "body",
                    ArgKind::Raw,
                    "Ex-command to run on each matching line (re-parsed per match)",
                ),
            ],
        },
    );
    let describe_command = registry.register_ex_command(
        "ex:describe-command",
        "Open the help view for a named command (DESIGN.md §5.11).",
        ExCommandSpec {
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(apply_describe_command),
            args_schema: vec![ArgSpec {
                name: "name",
                kind: ArgKind::String,
                doc: "Registered command name (`ex:write`, `motion:word-forward`, ...)",
                prompt: "command:",
                default: ArgDefault::Required,
            }],
        },
    );
    let describe_buffer = registry.register_ex_command(
        "ex:describe-buffer",
        "Open the help view for the current buffer's state (DESIGN.md §5.11).",
        ExCommandSpec {
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::DescribeBuffer)),
            args_schema: vec![],
        },
    );
    let apropos = registry.register_ex_command(
        "ex:apropos",
        "Search every registered command's name + doc for a substring (DESIGN.md §5.11).",
        ExCommandSpec {
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(apply_apropos),
            args_schema: vec![ArgSpec {
                name: "pattern",
                kind: ArgKind::String,
                doc: "Case-insensitive substring matched against name and doc",
                prompt: "apropos:",
                default: ArgDefault::Required,
            }],
        },
    );
    ExBuiltins {
        write,
        quit,
        write_quit,
        no_hlsearch,
        list_registers,
        list_marks,
        delete_line,
        set_option,
        edit,
        substitute,
        global,
        describe_command,
        describe_buffer,
        apropos,
    }
}

// ---- parse_args helpers (raw string -> typed Args) ----

fn parse_no_args(rest: &str, _bang: bool) -> GrammarResult<Args> {
    if rest.trim().is_empty() {
        Ok(Args::None)
    } else {
        Err(CommandError::BadArgs(
            "trailing characters after command".into(),
        ))
    }
}

fn parse_optional_path(rest: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        Ok(Args::None)
    } else {
        Ok(Args::String(trimmed.to_string()))
    }
}

fn parse_required_string(rest: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        Err(CommandError::BadArgs("argument required".into()))
    } else {
        Ok(Args::String(trimmed.to_string()))
    }
}

/// `:substitute` and `:global` enter through the `:`-line parser's
/// delimiter detection, not through the generic keyword path -- their
/// args come pre-parsed as `Args::List`. These stubs guard against a
/// caller that registers a keyword alias `:substitute foo`: the parse
/// path errors instead of producing malformed Args::List.
fn parse_substitute_args_unreachable(_rest: &str, _bang: bool) -> GrammarResult<Args> {
    Err(CommandError::BadArgs(
        "use the delimiter form: `:s/pattern/replacement/[flags]`".into(),
    ))
}

fn parse_global_args_unreachable(_rest: &str, _bang: bool) -> GrammarResult<Args> {
    Err(CommandError::BadArgs(
        "use the delimiter form: `:g/pattern/body` (or `:v/...` for inverted)".into(),
    ))
}

// ---- apply closures ----

fn apply_write(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    let path = match &ctx.args {
        Args::None => None,
        Args::String(s) => Some(std::path::PathBuf::from(s)),
        _ => {
            return Err(CommandError::BadArgs(
                "expected optional path string".into(),
            ));
        }
    };
    Ok(Effect::SaveBuffer { path })
}

fn apply_quit(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    Ok(Effect::QuitEditor { force: ctx.bang })
}

fn apply_write_quit(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    // The bang on `:wq!` / `:x!` propagates to the quit step (vim's
    // semantics: force the quit even if the save fails). Save itself is
    // never forced -- writing a path you don't have permission for fails
    // visibly.
    Ok(Effect::Many(vec![
        Effect::SaveBuffer { path: None },
        Effect::QuitEditor { force: ctx.bang },
    ]))
}

fn apply_set(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    match &ctx.args {
        Args::String(s) => Ok(Effect::SetOption { spec: s.clone() }),
        _ => Err(CommandError::BadArgs("expected option string".into())),
    }
}

fn apply_edit(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    let path = match &ctx.args {
        Args::None => None,
        Args::String(s) => Some(std::path::PathBuf::from(s)),
        _ => {
            return Err(CommandError::BadArgs(
                "expected optional path string".into(),
            ));
        }
    };
    Ok(Effect::OpenBuffer {
        path,
        force: ctx.bang,
    })
}

fn apply_substitute(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    let list = ctx
        .args
        .as_list()
        .ok_or_else(|| CommandError::BadArgs("expected Args::List for :substitute".into()))?;
    if list.len() != 3 {
        return Err(CommandError::BadArgs(
            "expected 3 args: pattern, replacement, flags".into(),
        ));
    }
    let pattern = list[0]
        .as_str()
        .ok_or_else(|| CommandError::BadArgs("arg 0 (pattern) must be string-shaped".into()))?
        .to_string();
    let replacement = list[1]
        .as_str()
        .ok_or_else(|| CommandError::BadArgs("arg 1 (replacement) must be string-shaped".into()))?
        .to_string();
    let flags = list[2]
        .as_str()
        .ok_or_else(|| CommandError::BadArgs("arg 2 (flags) must be string-shaped".into()))?;
    let global = flags.contains('g');
    // Scope falls out of the invocation's range: `s/...` -> CurrentLine,
    // `%s/...` -> Whole. The parser front-end set this from the
    // delimiter prefix.
    let scope = match ctx.range {
        Some(Range::Whole) => SubstituteScope::Whole,
        _ => SubstituteScope::CurrentLine,
    };
    Ok(Effect::Substitute {
        scope,
        pattern,
        replacement,
        global,
    })
}

fn apply_describe_command(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    match &ctx.args {
        Args::String(s) => Ok(Effect::DescribeCommand { name: s.clone() }),
        _ => Err(CommandError::BadArgs(
            "expected command name string".into(),
        )),
    }
}

fn apply_apropos(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    match &ctx.args {
        Args::String(s) => Ok(Effect::Apropos { pattern: s.clone() }),
        _ => Err(CommandError::BadArgs("expected pattern string".into())),
    }
}

fn apply_global(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    let list = ctx
        .args
        .as_list()
        .ok_or_else(|| CommandError::BadArgs("expected Args::List for :global".into()))?;
    if list.len() != 3 {
        return Err(CommandError::BadArgs(
            "expected 3 args: pattern, inverted, body".into(),
        ));
    }
    let pattern = list[0]
        .as_str()
        .ok_or_else(|| CommandError::BadArgs("arg 0 (pattern) must be string-shaped".into()))?
        .to_string();
    let inverted = list[1]
        .as_bool()
        .ok_or_else(|| CommandError::BadArgs("arg 1 (inverted) must be bool".into()))?;
    let body = list[2]
        .as_str()
        .ok_or_else(|| CommandError::BadArgs("arg 2 (body) must be string-shaped".into()))?
        .to_string();
    Ok(Effect::Global {
        pattern,
        inverted,
        body,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::dispatcher::execute;
    use crate::command::CommandInvocation;
    use lattice_core::Document;
    use lattice_protocol::position::Position;

    fn fixture() -> (CommandRegistry, ExBuiltins, Document) {
        let mut registry = CommandRegistry::new();
        let _ = crate::builtins::populate(&mut registry);
        let ex = populate(&mut registry);
        (registry, ex, Document::empty())
    }

    #[test]
    fn write_with_no_path_emits_save_buffer_none() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.write.0).with_args(Args::None);
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match eff {
            Effect::SaveBuffer { path } => assert!(path.is_none()),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn write_with_path_carries_path() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.write.0).with_args(Args::String("foo.txt".into()));
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match eff {
            Effect::SaveBuffer { path: Some(p) } => assert_eq!(p, std::path::PathBuf::from("foo.txt")),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn quit_bang_propagates_to_force() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.quit.0).with_bang(true);
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match eff {
            Effect::QuitEditor { force } => assert!(force),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn quit_no_bang_is_not_forced() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.quit.0);
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match eff {
            Effect::QuitEditor { force } => assert!(!force),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn write_quit_emits_many_save_then_quit() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.write_quit.0).with_bang(true);
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match eff {
            Effect::Many(parts) => {
                assert!(matches!(parts[0], Effect::SaveBuffer { .. }));
                assert!(matches!(parts[1], Effect::QuitEditor { force: true }));
            }
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn nohlsearch_emits_clear_search_highlight() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.no_hlsearch.0);
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        assert!(matches!(eff, Effect::ClearSearchHighlight));
    }

    #[test]
    fn registers_emits_echo_registers() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.list_registers.0);
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        assert!(matches!(eff, Effect::EchoRegisters));
    }

    #[test]
    fn marks_emits_echo_marks() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.list_marks.0);
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        assert!(matches!(eff, Effect::EchoMarks));
    }

    #[test]
    fn delete_emits_delete_current_line() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.delete_line.0);
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        assert!(matches!(eff, Effect::DeleteCurrentLine));
    }

    #[test]
    fn describe_command_emits_describe_command_effect() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.describe_command.0)
            .with_args(Args::String("ex:write".into()));
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match eff {
            Effect::DescribeCommand { name } => assert_eq!(name, "ex:write"),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn describe_buffer_emits_describe_buffer_effect() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.describe_buffer.0);
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        assert!(matches!(eff, Effect::DescribeBuffer));
    }

    #[test]
    fn apropos_emits_apropos_effect() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.apropos.0).with_args(Args::String("write".into()));
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match eff {
            Effect::Apropos { pattern } => assert_eq!(pattern, "write"),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn describe_command_advertises_args_schema() {
        // §B.1 metadata is what makes :describe-command interesting.
        let (registry, ex, _doc) = fixture();
        let spec = registry.lookup(ex.describe_command.0).unwrap();
        assert_eq!(spec.args_schema.len(), 1);
        assert_eq!(spec.args_schema[0].name, "name");
    }

    #[test]
    fn apropos_advertises_args_schema() {
        let (registry, ex, _doc) = fixture();
        let spec = registry.lookup(ex.apropos.0).unwrap();
        assert_eq!(spec.args_schema.len(), 1);
        assert_eq!(spec.args_schema[0].name, "pattern");
    }

    #[test]
    fn set_with_string_arg_carries_spec() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.set_option.0).with_args(Args::String("number".into()));
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match eff {
            Effect::SetOption { spec } => assert_eq!(spec, "number"),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn set_with_no_args_errors() {
        let (registry, ex, mut doc) = fixture();
        // The dispatcher itself doesn't error here -- parse_args is called
        // by the parser front-end, not the dispatcher. apply with the
        // wrong Args variant errors instead.
        let inv = CommandInvocation::of(ex.set_option.0).with_args(Args::None);
        let err = execute(&registry, &mut doc, Position::ZERO, inv).unwrap_err();
        assert!(matches!(err, CommandError::BadArgs(_)));
    }

    #[test]
    fn edit_with_path_and_bang_carries_force() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.edit.0)
            .with_args(Args::String("/tmp/x".into()))
            .with_bang(true);
        let eff = execute(&registry, &mut doc, Position::ZERO, inv).unwrap();
        match eff {
            Effect::OpenBuffer { path, force } => {
                assert_eq!(path, Some(std::path::PathBuf::from("/tmp/x")));
                assert!(force);
            }
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn parse_no_args_rejects_trailing() {
        assert!(matches!(
            parse_no_args("oops", false),
            Err(CommandError::BadArgs(_))
        ));
        assert!(matches!(parse_no_args("", false), Ok(Args::None)));
        assert!(matches!(parse_no_args("   ", false), Ok(Args::None)));
    }

    #[test]
    fn parse_optional_path_returns_some_or_none() {
        assert!(matches!(parse_optional_path("", false), Ok(Args::None)));
        match parse_optional_path("foo.rs", false).unwrap() {
            Args::String(s) => assert_eq!(s, "foo.rs"),
            other => panic!("unexpected args: {other:?}"),
        }
    }

    #[test]
    fn parse_required_string_demands_arg() {
        assert!(matches!(
            parse_required_string("", false),
            Err(CommandError::BadArgs(_))
        ));
        match parse_required_string("number", false).unwrap() {
            Args::String(s) => assert_eq!(s, "number"),
            other => panic!("unexpected args: {other:?}"),
        }
    }
}
