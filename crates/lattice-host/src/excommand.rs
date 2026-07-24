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

use lattice_grammar::registry::{CommandRegistry, MotionId, TextObjectId};
use lattice_grammar::{ArgValue, Args, CommandInvocation, CommandKind, Range, Target};

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
    WrongSurfaceForm {
        name: String,
        // PL8.F: `SurfaceForm::Delimiter.hint` became `Cow<'static, str>` (a
        // plugin delimiter command's hint is owned + frees on unregister), so
        // this carried field can't stay `&'static str`.
        hint: std::borrow::Cow<'static, str>,
    },
}

/// Parse a `:` line into a [`CommandInvocation`] dispatchable through
/// the unified `grammar::execute()`.
pub fn parse(line: &str, registry: &CommandRegistry) -> Result<CommandInvocation, ExCommandError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(ExCommandError::Empty);
    }

    // Vim's Visual `:` prefills the cmdline with `'<,'>` (the visual
    // range). Strip that prefix and mark the resulting invocation
    // `Range::Selection`, which resolves from `last_visual` (captured when
    // `:` left Visual — `resolve_grammar_range` + the narrow handler read
    // it). Lets `:'<,'>narrow` and other range-honoring commands act on
    // the selection. (General `%` / `1,5` line-range prefixes are a
    // separate enhancement; substitute keeps its own scope model.)
    if let Some(rest) = trimmed.strip_prefix("'<,'>") {
        let inner = parse(rest, registry)?;
        return Ok(inner.with_range(Range::Selection));
    }

    // Bare line number: `:42` → go to line 42 (same as `42G`).
    // Checked before keyword parse; pure digits are not valid command
    // names so there's no ambiguity. `:0` is treated as `:1`.
    if let Ok(n) = trimmed.parse::<u32>() {
        let id = registry
            .id_by_name("motion:goto-last-line")
            .ok_or_else(|| ExCommandError::Unknown(trimmed.to_string()))?;
        return Ok(lattice_grammar::CommandInvocation::of(id)
            .with_count(lattice_grammar::command::Count(n.max(1))));
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

    // DESIGN.md §5.2.1 kind-prefix form: `:motion <name>`,
    // `:operator <name> [target]`, `:text-object <name>`. The
    // bare kind word reserves the namespace; the next token is
    // looked up as `kind:<name>` in the registry. This is the
    // canonical surface for invoking non-ex-command primitives
    // from `:`. Bang on the kind word itself is rejected (it'd be
    // ambiguous which side it applied to).
    if let Some(kind) = parse_kind_word(cmd) {
        if bang {
            return Err(ExCommandError::BangNotAllowed(raw_cmd.to_string()));
        }
        return parse_kind_prefixed(registry, kind, rest);
    }

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

    // The bare-word path handles ex-commands only (`:write`, `:wq`,
    // `:set ...`). Motions / operators / text-objects are reached
    // from `:` via the explicit kind-prefix form
    // (`:motion goto-first-line`, `:operator delete word-forward`,
    // `:text-object inner-word`) -- handled in `parse_invocation`'s
    // kind-prefix branch above. If we land here with a non-ex
    // command, the user typed the registered canonical name (e.g.
    // `:motion:goto-first-line`) -- supported as a fallback for
    // tooling / scripts but not the canonical user surface; we
    // dispatch but don't accept args because the kind-prefix form
    // is the right place for that.
    match entry.kind {
        CommandKind::ExCommand => parse_ex_command(registry, id, entry, raw_cmd, rest, bang),
        CommandKind::Motion => parse_naked_motion(id, raw_cmd, rest, bang),
        CommandKind::Operator => parse_operator_with_target(registry, id, raw_cmd, rest, bang),
        CommandKind::TextObject => Err(ExCommandError::BadArgs(format!(
            "`{cmd}` is a text-object; pair it with an operator \
             (`:operator delete {cmd}`) or use chord grammar"
        ))),
        CommandKind::Action => Err(ExCommandError::Unknown(raw_cmd.to_string())),
    }
}

/// Match a bare command word against the reserved kind-prefix set
/// (`motion`, `operator`, `text-object`). Returns the matching
/// [`CommandKind`] or `None` for any other word -- which falls
/// through to ex-command resolution.
fn parse_kind_word(s: &str) -> Option<CommandKind> {
    match s {
        "motion" => Some(CommandKind::Motion),
        "operator" => Some(CommandKind::Operator),
        "text-object" => Some(CommandKind::TextObject),
        _ => None,
    }
}

/// Resolve the kind-prefix form: the leading kind word fixes the
/// namespace; the next whitespace-delimited token is the command
/// tail (the part after `motion:` / `operator:` / `text-object:`
/// in the canonical name). Subsequent tokens, if any, feed each
/// kind's specific parser (operators take a target).
fn parse_kind_prefixed(
    registry: &CommandRegistry,
    kind: CommandKind,
    rest: &str,
) -> Result<CommandInvocation, ExCommandError> {
    let trimmed = rest.trim();
    let (tail, more) = match trimmed.find(char::is_whitespace) {
        Some(i) => (&trimmed[..i], trimmed[i..].trim()),
        None => (trimmed, ""),
    };
    if tail.is_empty() {
        return Err(ExCommandError::BadArgs(format!(
            "`{}` requires a name (e.g. `:{} <name>`)",
            kind.label(),
            kind.label()
        )));
    }
    let canonical = format!("{}:{}", kind.label(), tail);
    let id = registry
        .id_by_name(&canonical)
        .ok_or_else(|| ExCommandError::Unknown(canonical.clone()))?;
    // Defensive: the lookup should match the kind we asserted via
    // the prefix. Mismatch means a registry inconsistency, not
    // user error.
    let entry = registry
        .lookup(id)
        .ok_or_else(|| ExCommandError::Unknown(canonical.clone()))?;
    debug_assert_eq!(entry.kind, kind, "kind-prefix lookup inconsistency");
    let _ = entry;
    match kind {
        CommandKind::Motion => parse_naked_motion(id, &canonical, more, false),
        CommandKind::Operator => parse_operator_with_target(registry, id, &canonical, more, false),
        CommandKind::TextObject => Err(ExCommandError::BadArgs(format!(
            "`{tail}` is a text-object; pair it with an operator \
             (`:operator delete {tail}`) or use chord grammar"
        ))),
        CommandKind::ExCommand | CommandKind::Action => unreachable!(),
    }
}

/// Run the ex-command-specific parsing path: surface-form check,
/// bang validation, then the spec's `parse_args` callback.
fn parse_ex_command(
    registry: &CommandRegistry,
    id: lattice_grammar::CommandId,
    entry: &lattice_grammar::CommandSpec,
    raw_cmd: &str,
    rest: &str,
    bang: bool,
) -> Result<CommandInvocation, ExCommandError> {
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
    // PL8.F: `SurfaceForm` is no longer `Copy` — bind the hint by reference and
    // clone it into the owned error field.
    if let lattice_grammar::SurfaceForm::Delimiter { hint } = &spec.surface_form {
        return Err(ExCommandError::WrongSurfaceForm {
            name: entry.name.clone(),
            hint: hint.clone(),
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

/// `:motion:NAME` -- run the motion against the active cursor. v1
/// accepts the naked form only (Args::None); motions that need a
/// char arg (`f`, `t`, etc.) error from inside the motion's
/// evaluator with `CommandError::InvalidArgs`. Bang is rejected --
/// motions don't carry a force flag.
fn parse_naked_motion(
    id: lattice_grammar::CommandId,
    raw_cmd: &str,
    rest: &str,
    bang: bool,
) -> Result<CommandInvocation, ExCommandError> {
    if bang {
        return Err(ExCommandError::BangNotAllowed(raw_cmd.to_string()));
    }
    if !rest.is_empty() {
        return Err(ExCommandError::TrailingArgs);
    }
    Ok(CommandInvocation::of(id))
}

/// `:operator <name> [target]` -- run the operator against a motion
/// or text-object whose tail name follows. The target lives in
/// `CommandInvocation::target`; the dispatcher's existing
/// resolve-target path handles the rest.
///
/// Target resolution uses the kind-prefix form's implicit-namespace
/// rule: a bare tail like `word-forward` is tried as `motion:word-
/// forward` first, then `text-object:word-forward`. The user can
/// also type the full canonical name (`motion:word-forward`) for
/// disambiguation; that path resolves directly. v1 accepts only
/// the trailing-name form -- explicit ranges
/// (`:operator delete .`, `:operator delete %`) are queued because
/// they overlap vim's existing range-prefix syntax (`:1,5d`) and we
/// want a single canonical surface.
fn parse_operator_with_target(
    registry: &CommandRegistry,
    id: lattice_grammar::CommandId,
    raw_cmd: &str,
    rest: &str,
    bang: bool,
) -> Result<CommandInvocation, ExCommandError> {
    if bang {
        return Err(ExCommandError::BangNotAllowed(raw_cmd.to_string()));
    }
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Err(ExCommandError::BadArgs(format!(
            "`{raw_cmd}` requires a target; use chord grammar (e.g. `dw`) \
             or pass a motion / text-object name \
             (`:operator delete word-forward`)"
        )));
    }
    // Resolution attempts (later wins out only if earlier fails):
    //   1. Direct canonical: user typed `motion:word-forward`.
    //   2. Implicit motion: prefix with `motion:`.
    //   3. Implicit text-object: prefix with `text-object:`.
    let target_id = registry
        .id_by_name(trimmed)
        .or_else(|| registry.id_by_name(&format!("motion:{trimmed}")))
        .or_else(|| registry.id_by_name(&format!("text-object:{trimmed}")))
        .ok_or_else(|| ExCommandError::Unknown(trimmed.to_string()))?;
    let target_entry = registry
        .lookup(target_id)
        .ok_or_else(|| ExCommandError::Unknown(trimmed.to_string()))?;
    let target = match target_entry.kind {
        CommandKind::Motion => Target::Motion(MotionId(target_id), Args::None),
        CommandKind::TextObject => Target::TextObject(TextObjectId(target_id), Args::None),
        _ => {
            return Err(ExCommandError::BadArgs(format!(
                "`{trimmed}` is not a motion or text-object \
                 (operators take motion/text-object targets only)"
            )));
        }
    };
    Ok(CommandInvocation::of(id).with_target(target))
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
    // `:qa` quits the whole editor (every pane + tab), distinct from
    // `:q` which closes a single pane unless it's the last one.
    ("qa", "ex:quit-all"),
    ("qall", "ex:quit-all"),
    ("quitall", "ex:quit-all"),
    // `:only` / `:on` -- close every pane except the active one
    // (vim's `<C-w>o`). Pane axis of `:tabonly`.
    ("only", "ex:only"),
    ("on", "ex:only"),
    // Pane splits + close (vim `:sp` / `:vs` / `:clo`).
    ("split", "ex:split"),
    ("sp", "ex:split"),
    ("vsplit", "ex:vsplit"),
    ("vsp", "ex:vsplit"),
    ("vs", "ex:vsplit"),
    ("close", "ex:close"),
    ("clo", "ex:close"),
    ("noh", "ex:nohlsearch"),
    ("nohl", "ex:nohlsearch"),
    ("nohlsearch", "ex:nohlsearch"),
    ("reg", "ex:registers"),
    ("registers", "ex:registers"),
    ("marks", "ex:marks"),
    ("d", "ex:delete"),
    ("delete", "ex:delete"),
    ("set", "ex:set"),
    // T.9.b (2026-06-18): `:colorscheme` + the vim short `:colo`.
    ("colorscheme", "ex:colorscheme"),
    ("colo", "ex:colorscheme"),
    ("setlocal", "ex:setlocal"),
    ("sl", "ex:setlocal"),
    ("setglobal", "ex:setglobal"),
    ("sg", "ex:setglobal"),
    ("e", "ex:edit"),
    ("edit", "ex:edit"),
    ("describe-command", "ex:describe-command"),
    ("describe-buffer", "ex:describe-buffer"),
    ("apropos", "ex:apropos"),
    ("describe-key", "ex:describe-key"),
    ("keymap", "ex:keymap"),
    ("bn", "ex:bnext"),
    ("bnext", "ex:bnext"),
    ("bp", "ex:bprev"),
    ("bprev", "ex:bprev"),
    // Issue #29 (2026-05-22): tab management.
    ("tabn", "ex:tabnext"),
    ("tabnext", "ex:tabnext"),
    ("tabp", "ex:tabprev"),
    ("tabprev", "ex:tabprev"),
    ("tabprevious", "ex:tabprev"),
    ("tabN", "ex:tabprev"),
    ("tabnew", "ex:tabnew"),
    ("tabe", "ex:tabnew"),
    ("tabedit", "ex:tabnew"),
    ("tabc", "ex:tabclose"),
    ("tabclose", "ex:tabclose"),
    // Issue #40 / Terminal-mode T1 (2026-05-22).
    ("term", "ex:terminal"),
    ("terminal", "ex:terminal"),
    ("tnew", "ex:terminal"),
    // T4 (2026-05-25): `:tabterminal [cmd]` opens a fresh tab
    // and lands a terminal in it. Sugar for `:tabnew | :terminal`.
    ("tabterminal", "ex:tabterminal"),
    ("tabterm", "ex:tabterminal"),
    ("tabo", "ex:tabonly"),
    ("tabonly", "ex:tabonly"),
    ("tabm", "ex:tabmove"),
    ("tabmove", "ex:tabmove"),
    // `:ls` keeps vim/emacs's static text listing (`list-buffers`,
    // `<C-x><C-b>`). `:buffers` and `:b` open the fuzzy buffer picker —
    // consistent with `:files` opening the file picker; the picker is the
    // interesting, actionable surface.
    ("ls", "ex:buffers"),
    ("buffers", "ex:buffer-picker"),
    ("bd", "ex:bdelete"),
    ("bdelete", "ex:bdelete"),
    ("b", "ex:buffer-picker"),
    // Picker entry points (Phase 4.x picker work). `:picker
    // <source>` is the canonical surface; the per-source
    // aliases (`:files`, `:recent`) ship for vim muscle
    // memory and route through their dedicated effect.
    ("picker", "ex:picker"),
    ("files", "ex:files"),
    ("recent", "ex:recent"),
    ("history", "ex:history"),
    ("Tree", "ex:filetree"),
    ("tree", "ex:filetree"),
    ("Filetree", "ex:filetree"),
    ("filetree", "ex:filetree"),
    ("TreeClose", "ex:filetree-close"),
    ("FiletreeClose", "ex:filetree-close"),
    ("filetree-close", "ex:filetree-close"),
    ("describe-option", "ex:describe-option"),
    // T.9.d (2026-06-18): theme-element / face introspection. Both the
    // element-name and the emacs `face` framing route to the one
    // command.
    ("describe-element", "ex:describe-element"),
    ("describe-face", "ex:describe-element"),
    ("options", "ex:options"),
    // PI.2: plugin-API introspection.
    ("describe-plugin-api", "ex:describe-plugin-api"),
    ("list-plugin-apis", "ex:list-plugin-apis"),
    ("export-plugin-api", "ex:export-plugin-api"),
    ("list-commands", "ex:list-commands"),
    ("describe-plugin", "ex:describe-plugin"),
    ("list-plugins", "ex:list-plugins"),
    ("describe-events", "ex:describe-events"),
    ("describe-event", "ex:describe-event"),
    // CR.6 (2026-06-24): the diff/hunk commands no longer need aliases —
    // `lattice_diff::install()` registers them under their plain canonical
    // names (`diff`, `diffsplit`, `hunk-next`, …), so `:diff` resolves
    // directly (the multibuffer pattern). No host `ex:diff*` shim remains.
    ("list-modes", "ex:list-modes"),
    ("describe-mode", "ex:describe-mode"),
    (
        "describe-option-resolution",
        "ex:describe-option-resolution",
    ),
    ("customize", "ex:customize"),
    ("tutor", "ex:tutor"),
    ("Tutor", "ex:tutor"),
    ("tutor-next", "ex:tutor-next"),
    ("tutor-prev", "ex:tutor-prev"),
    ("hover", "ex:hover"),
    ("HoverClose", "ex:hover-close"),
    ("h", "ex:help"),
    ("help", "ex:help"),
    // K.3.1 (2026-06-02): emacs-style canonical name for the
    // help-for-help entry point. Same effect as `:help` — the
    // <C-h>-prefix bindings (`<C-h><C-h>` / `<C-h>?`) wire here
    // per the K.3 help-prefix slice plan.
    ("help-for-help", "ex:help"),
    ("diagnostics", "ex:diagnostics"),
    ("diag", "ex:diagnostics"),
    ("diag-next", "ex:diag-next"),
    ("dnext", "ex:diag-next"),
    ("diag-prev", "ex:diag-prev"),
    ("dprev", "ex:diag-prev"),
    // CM.2 / CM.7 (2026-07-22): `:cnext`/`:cn`/`:cprev`/`:cp` repointed
    // from `ex:diag-*` to the error-list family. Error-list commands touch
    // ONLY the error list (no diagnostic fallback — dedicated
    // `]d`/`[d` + `:diag-*` own diagnostics); an empty list echoes
    // `no error list`, matching vim's `E42: No Errors`.
    // naming-2026-07-22: readable canonical names (`next-error`, emacs
    // vocabulary) lead; the vim `:c*` spellings are aliases onto them.
    ("cnext", "next-error"),
    ("cn", "next-error"),
    ("cprev", "previous-error"),
    ("cp", "previous-error"),
    ("cc", "error"),
    // The error picker (`:error`; parallel to `:diagnostics`).
    ("clist", "error-list"),
    ("cl", "error-list"),
    ("cfirst", "first-error"),
    ("crewind", "first-error"),
    ("cr", "first-error"),
    ("clast", "last-error"),
    // File-level traversal.
    ("cnextfile", "next-error-file"),
    ("cnf", "next-error-file"),
    ("cprevfile", "previous-error-file"),
    ("cpf", "previous-error-file"),
    // The grouped *problems* multibuffer (`:problems`; vim `:copen`/`:cclose`).
    ("copen", "problems"),
    ("cclose", "problems-close"),
    // LSP commands. Naming convention:
    //
    // 1. Dashed canonical only. Every LSP-coupled command has
    //    exactly one user-facing form -- the explicit `lsp-*`
    //    dashed name that tracks the canonical `ex:lsp-*` id.
    //    Generic names (`format`, `rename`, `complete`,
    //    `signature-help`, `code-actions`, `format-range`) are
    //    NOT registered as LSP aliases. They imply genericness
    //    -- a non-LSP `:format` could come from rustfmt-direct,
    //    a project formatter, treesitter, etc. -- and silently
    //    no-op when no LSP server (or no server with the
    //    relevant capability) is attached. The explicit
    //    `lsp-` prefix makes the dependency visible at the
    //    cmdline and reserves the generic names for future
    //    non-LSP implementations.
    //
    // 2. No collapsed forms (`lspformat`, `lspcodeaction`,
    //    `signaturehelp`, ...). They duplicate the dashed
    //    canonical with no visual benefit.
    //
    // 3. No vim-style 1-2 letter shortcuts for LSP commands.
    //    Vim shorts (`cn`, `cp`, `bn`, `bp`, etc.) come from
    //    decades of vim tradition tied to specific commands
    //    (`:cnext` etc.). LSP didn't exist when those were
    //    canonised, so any 1-2 letter LSP shortcut would be
    //    novel -- and `fmt` / `rn` / `ca` are too generic to
    //    earn that scarcity. If the user-config alias
    //    mechanism eventually lands (slice 8.h's WIT-shaped
    //    plugin / init.rs API), users / plugins can add their
    //    own personal shortcuts.
    ("messages", "ex:messages"),
    ("msg", "ex:messages"),
    ("lsp-log", "ex:lsp-log"),
    ("lsp-trace", "ex:lsp-trace"),
    ("lsp-trace-log", "ex:lsp-trace-log"),
    ("lsp-status", "ex:lsp-status"),
    ("lsp-server-log", "ex:lsp-server-log"),
    ("lsp-restart", "ex:lsp-restart"),
    ("lsp-progress-cancel", "ex:lsp-progress-cancel"),
    ("lsp-expand-region", "ex:lsp-expand-region"),
    ("lsp-shrink-region", "ex:lsp-shrink-region"),
    ("lsp-log-level", "ex:lsp-log-level"),
    ("lsp-log-clear", "ex:lsp-log-clear"),
    // Navigation pickers (Phase 4.2.e / 4.2.f).
    ("lsp-symbols", "ex:lsp-symbols"),
    ("lsp-workspace-symbol", "ex:lsp-workspace-symbol"),
    // 4.5.a -- call hierarchy.
    ("lsp-incoming-calls", "ex:lsp-incoming-calls"),
    ("lsp-outgoing-calls", "ex:lsp-outgoing-calls"),
    // 4.5.b -- type hierarchy.
    ("lsp-supertypes", "ex:lsp-supertypes"),
    ("lsp-subtypes", "ex:lsp-subtypes"),
    // 4.5.g -- moniker (cross-project symbol id).
    ("lsp-moniker", "ex:lsp-moniker"),
    // 4.5.d -- code lens picker.
    ("lsp-code-lens", "ex:lsp-code-lens"),
    // 4.5.e -- color presentation picker.
    ("lsp-color-presentation", "ex:lsp-color-presentation"),
    // Phase 4.3 edits.
    ("lsp-format", "ex:lsp-format"),
    ("lsp-format-range", "ex:lsp-format-range"),
    ("lsp-signature-help", "ex:lsp-signature-help"),
    ("lsp-complete", "ex:lsp-complete"),
    ("lsp-rename", "ex:lsp-rename"),
    ("lsp-code-action", "ex:lsp-code-action"),
    ("cd", "ex:cd"),
    ("chdir", "ex:cd"),
    ("pwd", "ex:pwd"),
];

/// Built-in aliases as a `(short, canonical)` map. Exposed for tests
/// and any future `:describe-aliases` view.
pub fn aliases() -> HashMap<&'static str, &'static str> {
    ALIAS_TABLE.iter().copied().collect()
}

/// Resolve a user-typed command spelling against the registry,
/// trying the canonical form first then falling back to the alias
/// table. Mirrors [`parse`]'s two-stage resolution so introspection
/// (`:describe-command`, `:apropos`) accepts both forms a user can
/// type at `:` (canonical `ex:write` or alias `w` / `write`).
///
/// Relocated to `lattice-host` in 5.5.F.2 alongside the
/// `:describe-command` builder that depends on it.
pub fn resolve_command_name_or_alias(
    registry: &lattice_grammar::CommandRegistry,
    name: &str,
) -> Option<lattice_grammar::CommandId> {
    if let Some(id) = registry.id_by_name(name) {
        return Some(id);
    }
    let canonical = aliases().get(name).copied()?;
    registry.id_by_name(canonical)
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

// ─────────────────────────────────────────────────────────────────
// MB.4 (rich minibuffer): live `:` line decorations.
//
// A lightweight tokenizer + validator that turns the in-progress
// command line into (1) syntax-highlight spans, (2) a live error
// indicator, and (3) a parameter hint — all produced off the render
// thread (computed on the actor thread, like
// `refresh_substitute_preview`, then published into the modeline
// render state). The ex-parser IS the grammar front-end (paramount
// #3), so this reuses its resolution + validation rather than
// re-implementing it.
// ─────────────────────────────────────────────────────────────────

use lattice_cells::style::Style;

/// One highlighted span of the `:` command line. `range` is a byte
/// range into the line text (the `:` prompt is NOT included — spans
/// are relative to `command_line()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLineSpan {
    pub range: std::ops::Range<usize>,
    pub style: Style,
}

/// Live decorations for the `:` line (MB.4): syntax spans, an
/// optional validation error, and an optional parameter hint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLineDecorations {
    /// Per-token highlight spans over the line text.
    pub spans: Vec<CommandLineSpan>,
    /// A live error message (unknown command / bad args), surfaced
    /// only once the command word is "committed" (a trailing space or
    /// `!`) so a valid-command *prefix* mid-typing doesn't flash red.
    pub error: Option<String>,
    /// A parameter hint (`<pat>/<rep>/[flags]`, `<file>`, …) derived
    /// from the resolved command's `ArgSpec`s. Shown dim after the line.
    pub param_hint: Option<String>,
}

impl CommandLineDecorations {
    /// True when there is nothing to draw (empty / all-default line).
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty() && self.error.is_none() && self.param_hint.is_none()
    }
}

/// Byte length of a leading range prefix (`%` whole-file, `'<,'>`
/// visual range) at the start of `line`, or `None` when the line has
/// no range prefix. Only the prefixes the ex-parser actually honors
/// are recognised; anything else falls through to the command word.
fn leading_range_len(line: &str) -> Option<usize> {
    if line.starts_with("'<,'>") {
        Some("'<,'>".len())
    } else if line.starts_with('%') {
        Some(1)
    } else {
        None
    }
}

/// Format a command's `ArgSpec`s as a parameter hint — `<name>` for a
/// required arg, `[<name>]` for an optional one (emacs / `marginalia`
/// convention, matching the command-palette args column).
fn format_arg_hint(schema: &[lattice_grammar::args::ArgSpec]) -> String {
    use lattice_grammar::args::ArgDefault;
    schema
        .iter()
        .map(|a| match a.default {
            ArgDefault::Required => format!("<{}>", a.name),
            _ => format!("[<{}>]", a.name),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Tokenize a substitute body (`s/pat/rep/flags`) starting at byte
/// `start` (the `s`), pushing spans for the `s`, each `/` delimiter,
/// the pattern, replacement, and flags. Honors `\`-escapes so an
/// escaped `\/` doesn't split a field. Byte offsets stay absolute.
fn tokenize_substitute(line: &str, start: usize, spans: &mut Vec<CommandLineSpan>) {
    // The command letter `s`.
    spans.push(CommandLineSpan {
        range: start..start + 1,
        style: Style::Keyword,
    });
    // 0 = pattern, 1 = replacement, 2 = flags.
    let mut state = 0u8;
    let mut field_start: Option<usize> = None;
    let mut chars = line[start + 1..].char_indices().peekable();
    while let Some((rel, c)) = chars.next() {
        let at = start + 1 + rel;
        if c == '\\' {
            // Escape: absorb the next char into the current field.
            field_start.get_or_insert(at);
            let _ = chars.next();
            continue;
        }
        if c == '/' {
            // Close the field before this delimiter.
            if let Some(fs) = field_start.take() {
                spans.push(CommandLineSpan {
                    range: fs..at,
                    style: field_style(state),
                });
            }
            spans.push(CommandLineSpan {
                range: at..at + c.len_utf8(),
                style: Style::Punctuation,
            });
            state = state.saturating_add(1).min(2);
            continue;
        }
        field_start.get_or_insert(at);
    }
    if let Some(fs) = field_start {
        spans.push(CommandLineSpan {
            range: fs..line.len(),
            style: field_style(state),
        });
    }
}

fn field_style(state: u8) -> Style {
    match state {
        0 | 1 => Style::String, // pattern / replacement
        _ => Style::Attribute,  // flags
    }
}

/// Compute the live decorations for a `:` command line (MB.4). `line`
/// is the text without the leading `:` (i.e. `command_line()`). Pure +
/// cheap (one line, no I/O) so it runs on the actor thread each edit.
pub fn command_line_decorations(
    line: &str,
    registry: &CommandRegistry,
) -> CommandLineDecorations {
    let mut deco = CommandLineDecorations::default();
    if line.trim().is_empty() {
        return deco;
    }
    let end = line.trim_end().len();

    // Bare line number (`:42`) — a goto-line range.
    if line.trim().chars().all(|c| c.is_ascii_digit()) {
        deco.spans.push(CommandLineSpan {
            range: 0..end,
            style: Style::Number,
        });
        return deco;
    }

    // Optional leading range prefix (`%`, `'<,'>`).
    let mut cursor = 0usize;
    if let Some(rng) = leading_range_len(line) {
        deco.spans.push(CommandLineSpan {
            range: 0..rng,
            style: Style::Number,
        });
        cursor = rng;
    }
    let rest = &line[cursor..];

    // Substitute delimiter form (`s/…/…/…`).
    if rest.starts_with("s/") {
        tokenize_substitute(line, cursor, &mut deco.spans);
        deco.param_hint = Some("<pattern>/<replacement>/[flags]".to_string());
        return deco;
    }
    // Global form (`g/pattern/cmd`, `v/pattern/cmd`).
    if rest.starts_with("g/") || rest.starts_with("v/") {
        deco.spans.push(CommandLineSpan {
            range: cursor..cursor + 1,
            style: Style::Keyword,
        });
        if end > cursor + 1 {
            deco.spans.push(CommandLineSpan {
                range: cursor + 1..end,
                style: Style::Default,
            });
        }
        deco.param_hint = Some("/<pattern>/<command>".to_string());
        return deco;
    }

    // Keyword form: command word (+ optional `!`) then args.
    let word_rel = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let raw_word = &rest[..word_rel];
    let (word, has_bang) = raw_word
        .strip_suffix('!')
        .map(|w| (w, true))
        .unwrap_or((raw_word, false));
    let word_end = cursor + word.len();

    let resolved = if word.is_empty() {
        None
    } else {
        resolve_command_name_or_alias(registry, word)
    };
    let known = resolved.is_some();
    // A word is "committed" once it is followed by whitespace or a
    // bang — before that, a valid-command prefix shouldn't flash red.
    let committed = word_rel < rest.len() || has_bang;
    let flag_error = !word.is_empty() && !known && committed;

    deco.spans.push(CommandLineSpan {
        range: cursor..word_end,
        style: if flag_error {
            Style::DiagnosticError
        } else {
            Style::Keyword
        },
    });
    if has_bang {
        deco.spans.push(CommandLineSpan {
            range: word_end..word_end + 1,
            style: Style::Operator,
        });
    }
    let args_start = cursor + raw_word.len();
    if args_start < end {
        deco.spans.push(CommandLineSpan {
            range: args_start..end,
            style: Style::Default,
        });
    }

    if flag_error {
        deco.error = Some(format!("unknown command: {raw_word}"));
    }

    // Parameter hint from the resolved command's ArgSpec (only when the
    // command is known and takes args).
    if let Some(id) = resolved
        && let Some(spec) = registry.lookup(id)
        && !spec.args_schema.is_empty()
    {
        deco.param_hint = Some(format_arg_hint(&spec.args_schema));
    }

    deco
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
        // `:problems` / `:problems-close` (aliased from vim `:copen` /
        // `:cclose` in the table below) are registered by the multibuffer
        // install path at boot, not by the grammar populate. Mirror that
        // here so `aliases_table_is_self_consistent` sees their targets.
        lattice_multibuffer::providers::problems::register_problems_ex_commands(
            &mut registry,
        );
        registry
    }

    fn invocation_name<'a>(inv: &'a CommandInvocation, registry: &'a CommandRegistry) -> &'a str {
        registry
            .lookup(inv.command)
            .map(|s| s.name.as_str())
            .unwrap_or("?")
    }

    // ---- MB.4: live command-line decorations ----

    /// The command word of a known command highlights as a keyword;
    /// no error, and a known command with args offers a param hint.
    #[test]
    fn mb4_known_command_highlights_keyword_with_hint() {
        let reg = fixture();
        let d = command_line_decorations("write foo.txt", &reg);
        // First span covers the command word `write` as a keyword.
        assert_eq!(d.spans[0].range, 0..5);
        assert_eq!(d.spans[0].style, Style::Keyword);
        assert!(d.error.is_none(), "known command has no error");
        assert!(
            d.param_hint.is_some(),
            "`:write` takes a path arg → a hint"
        );
    }

    /// An unknown command, once committed (a trailing space), flags an
    /// error and paints the word with the diagnostic-error style.
    #[test]
    fn mb4_unknown_committed_command_flags_error() {
        let reg = fixture();
        let d = command_line_decorations("frobnicate x", &reg);
        assert_eq!(d.spans[0].style, Style::DiagnosticError);
        assert_eq!(d.error.as_deref(), Some("unknown command: frobnicate"));
    }

    /// A *prefix* of a valid command, still being typed (no trailing
    /// space), must NOT flash red — reduces mid-typing flicker.
    #[test]
    fn mb4_uncommitted_prefix_does_not_error() {
        let reg = fixture();
        let d = command_line_decorations("writ", &reg);
        assert_eq!(d.spans[0].style, Style::Keyword);
        assert!(d.error.is_none());
    }

    /// A substitute line tokenizes into `s`, `/` delimiters, the
    /// pattern, and the replacement, and offers the s/// hint.
    #[test]
    fn mb4_substitute_tokenizes_fields() {
        let reg = fixture();
        let d = command_line_decorations("s/foo/bar/g", &reg);
        // `s` keyword.
        assert_eq!(d.spans[0].range, 0..1);
        assert_eq!(d.spans[0].style, Style::Keyword);
        // Somewhere a `/` delimiter (punctuation) and a String field.
        assert!(d.spans.iter().any(|s| s.style == Style::Punctuation));
        assert!(d.spans.iter().any(|s| s.style == Style::String));
        // Flags field styled as an attribute.
        assert!(d.spans.iter().any(|s| s.style == Style::Attribute));
        assert!(d.param_hint.is_some());
    }

    /// A `%`-scoped substitute paints the leading `%` as a range.
    #[test]
    fn mb4_range_prefix_is_highlighted() {
        let reg = fixture();
        let d = command_line_decorations("%s/a/b/", &reg);
        assert_eq!(d.spans[0].range, 0..1);
        assert_eq!(d.spans[0].style, Style::Number);
    }

    /// An empty line produces no decorations.
    #[test]
    fn mb4_empty_line_is_bare() {
        let reg = fixture();
        assert!(command_line_decorations("   ", &reg).is_empty());
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
    fn colorscheme_full_and_short_alias_route_to_registry() {
        // T.9.b: both `:colorscheme <name>` and the vim short `:colo`
        // resolve to `ex:colorscheme` carrying the name as a String arg.
        let r = fixture();
        let inv = parse("colorscheme catppuccin-macchiato", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:colorscheme");
        assert_eq!(inv.args, Args::String("catppuccin-macchiato".to_string()));
        let inv_short = parse("colo catppuccin-mocha", &r).unwrap();
        assert_eq!(invocation_name(&inv_short, &r), "ex:colorscheme");
        assert_eq!(inv_short.args, Args::String("catppuccin-mocha".to_string()));
    }

    #[test]
    fn colorscheme_without_name_opens_picker() {
        // T.12a: the no-arg form no longer errors — it parses to
        // `ex:colorscheme` with `Args::None`. The host's
        // `Effect::SetColorscheme("")` arm opens the live-preview
        // theme picker (see dispatch.rs + the `theme_picker_*` tests).
        let r = fixture();
        let inv = parse("colorscheme", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:colorscheme");
        assert_eq!(inv.args, Args::None);
        let inv_short = parse("colo", &r).unwrap();
        assert_eq!(invocation_name(&inv_short, &r), "ex:colorscheme");
        assert_eq!(inv_short.args, Args::None);
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
    fn buffers_and_b_open_the_picker_ls_keeps_the_text_list() {
        // `:buffers` and `:b` both open the fuzzy buffer picker (consistent
        // with `:files`); `:ls` keeps the static text listing.
        let r = fixture();
        let buffers = parse("buffers", &r).unwrap();
        let b = parse("b", &r).unwrap();
        assert_eq!(invocation_name(&buffers, &r), "ex:buffer-picker");
        assert_eq!(
            buffers.command, b.command,
            ":buffers and :b are the same command"
        );
        assert_eq!(invocation_name(&parse("ls", &r).unwrap(), &r), "ex:buffers");
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

    // ---- §5.2.1 kind-prefix form: every command reachable from `:` ----

    #[test]
    fn motion_kind_prefix_dispatches_naked() {
        let r = fixture();
        let inv = parse("motion goto-first-line", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "motion:goto-first-line");
        assert_eq!(inv.target, None);
        assert!(matches!(inv.args, Args::None));
        assert!(!inv.bang);
    }

    #[test]
    fn motion_kind_prefix_with_trailing_text_errors() {
        let r = fixture();
        let err = parse("motion goto-first-line nonsense", &r).unwrap_err();
        assert!(matches!(err, ExCommandError::TrailingArgs));
    }

    #[test]
    fn motion_kind_prefix_without_name_errors_helpfully() {
        let r = fixture();
        let err = parse("motion", &r).unwrap_err();
        let msg = match err {
            ExCommandError::BadArgs(m) => m,
            other => panic!("expected BadArgs, got {other:?}"),
        };
        assert!(msg.contains("requires a name"), "got: {msg}");
    }

    #[test]
    fn kind_prefix_with_unknown_tail_errors_unknown() {
        let r = fixture();
        let err = parse("motion no-such-thing", &r).unwrap_err();
        match err {
            ExCommandError::Unknown(name) => {
                assert_eq!(name, "motion:no-such-thing");
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn kind_prefix_rejects_bang_on_kind_word() {
        let r = fixture();
        let err = parse("motion! goto-first-line", &r).unwrap_err();
        assert!(matches!(err, ExCommandError::BangNotAllowed(_)));
    }

    #[test]
    fn operator_with_bare_motion_target_resolves_via_implicit_namespace() {
        let r = fixture();
        // `:operator delete word-forward` -- target tail looked up
        // as `motion:word-forward` implicitly.
        let inv = parse("operator delete word-forward", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "operator:delete");
        match inv.target {
            Some(Target::Motion(_, _)) => {}
            other => panic!("expected motion target, got {other:?}"),
        }
    }

    #[test]
    fn operator_with_full_canonical_target_also_resolves() {
        let r = fixture();
        // `:operator delete motion:word-forward` -- canonical form
        // also accepted for disambiguation.
        let inv = parse("operator delete motion:word-forward", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "operator:delete");
        match inv.target {
            Some(Target::Motion(_, _)) => {}
            other => panic!("expected motion target, got {other:?}"),
        }
    }

    #[test]
    fn operator_with_text_object_target_via_implicit_namespace() {
        let r = fixture();
        let inv = parse("operator delete inner-word", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "operator:delete");
        match inv.target {
            Some(Target::TextObject(_, _)) => {}
            other => panic!("expected text-object target, got {other:?}"),
        }
    }

    #[test]
    fn operator_without_target_errors_helpfully() {
        let r = fixture();
        let err = parse("operator delete", &r).unwrap_err();
        let msg = match err {
            ExCommandError::BadArgs(m) => m,
            other => panic!("expected BadArgs, got {other:?}"),
        };
        assert!(msg.contains("requires a target"), "got: {msg}");
        assert!(msg.contains("chord grammar"), "got: {msg}");
    }

    #[test]
    fn operator_with_non_motion_target_errors() {
        let r = fixture();
        // Pass an ex-command name as target -- not a motion or
        // text-object. Implicit namespace tries `motion:ex:write`
        // and `text-object:ex:write` (both miss); the canonical
        // `ex:write` resolves but fails the kind check.
        let err = parse("operator delete ex:write", &r).unwrap_err();
        let msg = match err {
            ExCommandError::BadArgs(m) => m,
            other => panic!("expected BadArgs, got {other:?}"),
        };
        assert!(msg.contains("not a motion or text-object"), "got: {msg}");
    }

    #[test]
    fn naked_text_object_errors_helpfully() {
        let r = fixture();
        let err = parse("text-object inner-word", &r).unwrap_err();
        let msg = match err {
            ExCommandError::BadArgs(m) => m,
            other => panic!("expected BadArgs, got {other:?}"),
        };
        assert!(msg.contains("text-object"), "got: {msg}");
        assert!(msg.contains("operator"), "got: {msg}");
    }

    #[test]
    fn ex_command_path_unchanged() {
        // Sanity check: the kind-prefix work doesn't disturb the
        // ex-command happy path. `:write foo.txt` still resolves.
        let r = fixture();
        let inv = parse("write foo.txt", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "ex:write");
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

    /// Regression: every picker entry-point ex-command is
    /// invocable through its user-facing name. The grammar
    /// registers the canonical `ex:*` form; without an alias
    /// row the parser rejects `:picker` / `:files` /
    /// `:recent` as "unknown command". This walks the
    /// expected user-facing surface end-to-end through `parse`
    /// so a missing alias row surfaces in CI rather than at
    /// runtime.
    #[test]
    fn picker_commands_resolve_through_user_facing_names() {
        let r = fixture();
        for line in ["picker files", "files", "recent"] {
            let result = parse(line, &r);
            assert!(
                result.is_ok(),
                "expected `:{line}` to parse, got {:?}",
                result.err()
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

    #[test]
    fn bare_integer_routes_to_goto_last_line_with_count() {
        let r = fixture();
        let inv = parse("42", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "motion:goto-last-line");
        let count = inv.count.expect("bare integer must carry explicit count");
        assert_eq!(count.get(), 42);
    }

    #[test]
    fn bare_integer_one_routes_to_goto_last_line_with_count_one() {
        let r = fixture();
        let inv = parse("1", &r).unwrap();
        assert_eq!(invocation_name(&inv, &r), "motion:goto-last-line");
        assert_eq!(inv.count.unwrap().get(), 1);
    }
}
