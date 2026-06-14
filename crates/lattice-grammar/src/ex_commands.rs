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
use crate::command::LatencyClass;
use crate::AppEffect;
use crate::effect::{Effect, SubstituteScope};
use crate::error::{CommandError, GrammarResult};
use crate::range::Range;
use crate::registry::{CommandRegistry, ExCommandContext, ExCommandId, ExCommandSpec, SurfaceForm};

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
    pub set_local_option: ExCommandId,
    pub set_global_option: ExCommandId,
    pub edit: ExCommandId,
    pub substitute: ExCommandId,
    pub global: ExCommandId,
    pub describe_command: ExCommandId,
    pub describe_buffer: ExCommandId,
    pub apropos: ExCommandId,
    pub describe_key: ExCommandId,
    pub list_keymap: ExCommandId,
    pub buffer_next: ExCommandId,
    pub buffer_prev: ExCommandId,
    pub list_buffers: ExCommandId,
    pub buffer_delete: ExCommandId,
    pub file_tree: ExCommandId,
    pub file_tree_close: ExCommandId,
    pub oil: ExCommandId,
    pub describe_option: ExCommandId,
    pub list_options: ExCommandId,
    pub describe_events: ExCommandId,
    pub describe_event: ExCommandId,
    /// D.2.d (2026-05-29): `:describe-diff` introspection
    /// (`lattice-host` `do_describe_diff`).
    pub describe_diff: ExCommandId,
    /// D.3.a.1 (2026-05-29): `:diff` (no args) — open an
    /// inline diff session against on-disk baseline.
    pub diff_open: ExCommandId,
    /// D.3.a.1 (2026-05-29): `:diffoff[!]` — close the active
    /// pane's diff session. D.4.d.3.a (2026-05-30) added the
    /// bang form (forward-compat for D.6 three-way merge; v1
    /// behaviour is identical to the no-bang form).
    pub diff_off: ExCommandId,
    /// D.4.d.3.a (2026-05-30): `:diffthis` — stage the active
    /// pane for a two-pane diff; the second invocation in a
    /// different pane completes the session.
    pub diff_this: ExCommandId,
    /// D.4.d.3.b (2026-05-30): `:diffsplit <file>` — open
    /// `<file>` in a new vsplit and register a two-pane diff
    /// between the current pane and the new pane.
    pub diff_split: ExCommandId,
    /// D.6.d (2026-05-31): `:diffget [<bufnr>]` — pull the
    /// hunk under the cursor from another buffer's side.
    pub diff_get_cmd: ExCommandId,
    /// D.6.d (2026-05-31): `:diffput [<bufnr>]` — push the
    /// hunk under the cursor into another buffer's side.
    pub diff_put_cmd: ExCommandId,
    /// D.6.e (2026-05-31): `:diff-accept` — resolve the
    /// active session with `DiffOutcome::Accept`.
    pub diff_accept: ExCommandId,
    /// D.6.e (2026-05-31): `:diff-reject` — resolve the
    /// active session with `DiffOutcome::Reject`.
    pub diff_reject: ExCommandId,
    /// D.3.c (2026-05-29): `:hunk-next` / `]c` — jump to next
    /// diff hunk on the current side.
    pub hunk_next: ExCommandId,
    /// D.3.c (2026-05-29): `:hunk-prev` / `[c` — jump to
    /// previous diff hunk on the current side.
    pub hunk_prev: ExCommandId,
    pub list_modes: ExCommandId,
    pub describe_mode: ExCommandId,
    pub describe_option_resolution: ExCommandId,
    pub customize: ExCommandId,
    pub tutor: ExCommandId,
    pub tutor_next: ExCommandId,
    pub tutor_prev: ExCommandId,
    pub hover: ExCommandId,
    pub hover_close: ExCommandId,
    pub help: ExCommandId,
    pub list_diagnostics: ExCommandId,
    pub next_diagnostic: ExCommandId,
    pub prev_diagnostic: ExCommandId,
    pub lsp_log: ExCommandId,
    pub messages: ExCommandId,
    pub lsp_trace: ExCommandId,
    pub lsp_status: ExCommandId,
    pub lsp_server_log: ExCommandId,
    pub lsp_restart: ExCommandId,
    pub lsp_progress_cancel: ExCommandId,
    pub lsp_expand_region: ExCommandId,
    pub lsp_shrink_region: ExCommandId,
    pub lsp_log_level: ExCommandId,
    pub lsp_log_clear: ExCommandId,
    pub lsp_symbols: ExCommandId,
    pub lsp_workspace_symbol: ExCommandId,
    pub lsp_incoming_calls: ExCommandId,
    pub lsp_outgoing_calls: ExCommandId,
    pub lsp_supertypes: ExCommandId,
    pub lsp_subtypes: ExCommandId,
    pub lsp_moniker: ExCommandId,
    pub lsp_code_lens: ExCommandId,
    pub lsp_color_presentation: ExCommandId,
    pub lsp_format: ExCommandId,
    pub lsp_format_range: ExCommandId,
    pub lsp_signature_help: ExCommandId,
    pub lsp_complete: ExCommandId,
    pub lsp_rename: ExCommandId,
    pub lsp_code_action: ExCommandId,
    // SN.3c.1 (2026-06-14): `:snippet-expand` removed (UX-useless;
    // `<C-x><C-s>` is the live trigger, now mode-owned). The expand
    // path no longer has an ex-command surface form.
    pub reload_snippets: ExCommandId,
}

pub fn populate(registry: &mut CommandRegistry) -> ExBuiltins {
    let write = registry.register_ex_command(
        "ex:write",
        "Write the current buffer to disk (`:w [path]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(apply_write),
            args_schema: vec![ArgSpec {
                name: "path",
                kind: ArgKind::String,
                doc: "Destination path. Absent = overwrite current file.",
                prompt: "path:",
                default: ArgDefault::None,
                completion: Some("gen:files"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let quit = registry.register_ex_command(
        "ex:quit",
        "Quit the editor (`:q[!]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: true,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(apply_quit),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let write_quit = registry.register_ex_command(
        "ex:write-quit",
        "Write the current buffer and quit (`:wq[!]` / `:x[!]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: true,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(apply_write_quit),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let no_hlsearch = registry.register_ex_command(
        "ex:nohlsearch",
        "Clear the search-highlight overlay (`:noh[lsearch]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::ClearSearchHighlight)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let list_registers = registry.register_ex_command(
        "ex:registers",
        "Show every register's contents (`:reg[isters]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::EchoRegisters)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let list_marks = registry.register_ex_command(
        "ex:marks",
        "Show every set mark's name + position (`:marks`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::EchoMarks)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let delete_line = registry.register_ex_command(
        "ex:delete",
        "Delete the current line including its newline (`:d[elete]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::DeleteCurrentLine)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let set_option = registry.register_ex_command(
        "ex:set",
        "Set a view option (`:set <option>`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(apply_set),
            // Single arg slot tied to the `gen:options` completion
            // generator so `:set <Tab>` enumerates option names and
            // `:set foldmethod=<Tab>` enumerates valid values.
            args_schema: vec![ArgSpec {
                name: "option",
                kind: ArgKind::String,
                doc: "Option name, `name=value`, `name?`, or `noname`.",
                prompt: "option:",
                default: ArgDefault::Required,
                completion: Some("gen:options"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let set_local_option = registry.register_ex_command(
        "ex:setlocal",
        "Set a buffer-local option override (`:setlocal <option>`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(apply_set_local),
            args_schema: vec![ArgSpec {
                name: "option",
                kind: ArgKind::String,
                doc: "Option name, `name=value`, `noname`, or `name&` to clear.",
                prompt: "option:",
                default: ArgDefault::Required,
                completion: Some("gen:options"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let set_global_option = registry.register_ex_command(
        "ex:setglobal",
        "Set a global option (`:setglobal <option>`). Does not update buffer-local overrides.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(apply_set_global),
            args_schema: vec![ArgSpec {
                name: "option",
                kind: ArgKind::String,
                doc: "Option name, `name=value`, `noname`, or `name?` to query global value.",
                prompt: "option:",
                default: ArgDefault::Required,
                completion: Some("gen:options"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let edit = registry.register_ex_command(
        "ex:edit",
        "Load a file into the current document (`:e[!] [path]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: true,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(apply_edit),
            args_schema: vec![ArgSpec {
                name: "path",
                kind: ArgKind::String,
                doc: "File path to open. Absent = reload current file.",
                prompt: "path:",
                default: ArgDefault::None,
                completion: Some("gen:files"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let substitute = registry.register_ex_command(
        "ex:substitute",
        "Replace pattern with replacement on the current line or `%` whole buffer (`:s/pat/rep/[g]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
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
                    completion: None,
                },
            ],
            surface_form: SurfaceForm::Delimiter {
                hint: ":s/pattern/replacement/[flags]  (or :%s/.../.../  for whole buffer)",
            },
        },
    );
    let global = registry.register_ex_command(
        "ex:global",
        "Run a command on every line matching (`:g`) or NOT matching (`:v`) a pattern.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_global_args_unreachable),
            apply: Box::new(apply_global),
            args_schema: vec![
                ArgSpec::required("pattern", ArgKind::Pattern, "Match pattern (literal in v1)"),
                ArgSpec {
                    name: "inverted",
                    kind: ArgKind::Bool,
                    doc: "True for `:v` form -- match lines NOT matching the pattern.",
                    prompt: "",
                    default: ArgDefault::Literal(ArgValue::Bool(false)),
                    completion: None,
                },
                ArgSpec::required(
                    "body",
                    ArgKind::Raw,
                    "Ex-command to run on each matching line (re-parsed per match)",
                ),
            ],
            surface_form: SurfaceForm::Delimiter {
                hint: ":g/pattern/body  (or :v/pattern/body  for inverted)",
            },
        },
    );
    let describe_command = registry.register_ex_command(
        "ex:describe-command",
        "Open the help view for a named command (DESIGN.md §5.11).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
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
                completion: Some("gen:commands"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let describe_buffer = registry.register_ex_command(
        "ex:describe-buffer",
        "Open the help view for the current buffer's state (DESIGN.md §5.11).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::DescribeBuffer)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let apropos = registry.register_ex_command(
        "ex:apropos",
        "Search every registered command's name + doc for a substring (DESIGN.md §5.11).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
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
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let describe_key = registry.register_ex_command(
        "ex:describe-key",
        "Open the help view for a key chord (DESIGN.md §5.11).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(apply_describe_key),
            args_schema: vec![ArgSpec {
                name: "chord",
                kind: ArgKind::Chord,
                doc: "Chord notation (`j`, `dw`, `<C-d>`, `gg`, `<Esc>`, ...)",
                prompt: "key:",
                default: ArgDefault::Required,
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let list_keymap = registry.register_ex_command(
        "ex:keymap",
        "Open the help view listing every default keymap binding by mode (DESIGN.md §5.11).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::ListKeymap)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let buffer_next = registry.register_ex_command(
        "ex:bnext",
        "Cycle to the next open document buffer (`:bn[ext]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::BufferNext)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // Issue #29 (2026-05-22): tab management ex-commands.
    // Each returns Effect::AppAction wrapping the matching
    // AppEffect; the host's apply_app_effect pushes the
    // matching Action onto out.next_actions.
    let _tab_next = registry.register_ex_command(
        "ex:tabnext",
        "Switch to the next tab (`:tabn[ext]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::AppAction(AppEffect::NextTab))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _tab_prev = registry.register_ex_command(
        "ex:tabprev",
        "Switch to the previous tab (`:tabp[rev]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::AppAction(AppEffect::PrevTab))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _tab_new = registry.register_ex_command(
        "ex:tabnew",
        "Open a new tab (optionally with `<path>`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| match &ctx.args {
                Args::String(path) if !path.is_empty() => {
                    Ok(Effect::AppAction(AppEffect::NewTabAt(path.clone())))
                }
                _ => Ok(Effect::AppAction(AppEffect::NewTab)),
            }),
            args_schema: vec![ArgSpec {
                name: "path",
                kind: ArgKind::String,
                doc: "Optional file path to open in the new tab",
                prompt: "",
                default: ArgDefault::None,
                completion: Some("gen:files"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _tab_close = registry.register_ex_command(
        "ex:tabclose",
        "Close the active tab.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::AppAction(AppEffect::CloseTab))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // Issue #40 / Terminal-mode T1: `:terminal [cmd]` opens a
    // PTY-backed shell buffer (T2 wires keystroke input).
    let _terminal = registry.register_ex_command(
        "ex:terminal",
        "Open a PTY-backed shell buffer (optionally running `<cmd>`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| match &ctx.args {
                Args::String(s) if !s.is_empty() => {
                    Ok(Effect::AppAction(AppEffect::TerminalSpawn(Some(s.clone()))))
                }
                _ => Ok(Effect::AppAction(AppEffect::TerminalSpawn(None))),
            }),
            args_schema: vec![ArgSpec {
                name: "cmd",
                kind: ArgKind::String,
                doc: "Optional command line (default: $SHELL or /bin/sh)",
                prompt: "",
                default: ArgDefault::None,
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // T4 (2026-05-25): `:tabterminal [cmd]` — open a new tab
    // with a PTY-backed shell. Sugar for `:tabnew | :terminal`.
    let _tab_terminal = registry.register_ex_command(
        "ex:tabterminal",
        "Open a new tab containing a PTY-backed shell buffer.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| match &ctx.args {
                Args::String(s) if !s.is_empty() => Ok(Effect::AppAction(
                    AppEffect::TerminalSpawnInNewTab(Some(s.clone())),
                )),
                _ => Ok(Effect::AppAction(AppEffect::TerminalSpawnInNewTab(None))),
            }),
            args_schema: vec![ArgSpec {
                name: "cmd",
                kind: ArgKind::String,
                doc: "Optional command line (default: $SHELL or /bin/sh)",
                prompt: "",
                default: ArgDefault::None,
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _tab_only = registry.register_ex_command(
        "ex:tabonly",
        "Close every tab except the active one.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::AppAction(AppEffect::OnlyTab))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // `:tabmove [N]` — optional positional u32 arg. Missing
    // (or 0) means "move to last position" (mirrors vim).
    // Stored as Args::String for the parsed-int text since
    // Args has no Int variant; apply re-parses.
    let _tab_move = registry.register_ex_command(
        "ex:tabmove",
        "Move the active tab to position N (1-indexed; default last).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_tabmove_arg),
            apply: Box::new(|ctx| {
                let n: u32 = match &ctx.args {
                    Args::String(s) => s.parse::<u32>().unwrap_or(0),
                    _ => 0,
                };
                Ok(Effect::AppAction(AppEffect::MoveTab(n)))
            }),
            args_schema: vec![ArgSpec::required(
                "n",
                ArgKind::Int,
                "Target position (1-indexed; 0 = last)",
            )],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let buffer_prev = registry.register_ex_command(
        "ex:bprev",
        "Cycle to the previous open document buffer (`:bp[rev]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::BufferPrev)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let list_buffers = registry.register_ex_command(
        "ex:buffers",
        "List every open document buffer (`:ls` / `:buffers`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::ListBuffers)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _buffer_picker = registry.register_ex_command(
        "ex:buffer-picker",
        "Open the vertico-style buffer switcher (`:b`). Type to filter; `<CR>` to switch.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::OpenBufferPicker)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let buffer_delete = registry.register_ex_command(
        "ex:bdelete",
        "Close the active document buffer (`:bd[elete][!]`). `!` discards unsaved changes.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: true,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|ctx| Ok(Effect::BufferDelete { force: ctx.bang })),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // `:files [root]` and `:recent` are short aliases for
    // `:picker files [root]` / `:picker recent`. They emit
    // the canonical `Effect::OpenPicker` so they go through
    // the same trait-driven dispatch + MRU pipeline; the
    // separate ex-command surface exists only for vim
    // muscle memory.
    let _files_picker = registry.register_ex_command(
        "ex:files",
        "Open the workspace file picker (`:files [root]`). Alias for `:picker files [root]`.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| {
                let args: Vec<String> = match &ctx.args {
                    Args::String(p) if !p.is_empty() => vec![p.clone()],
                    _ => Vec::new(),
                };
                Ok(Effect::OpenPicker {
                    source: "files".into(),
                    args,
                })
            }),
            args_schema: vec![ArgSpec {
                name: "root",
                kind: ArgKind::String,
                doc: "Directory to walk. Absent = current document's parent / cwd.",
                prompt: "root:",
                default: ArgDefault::None,
                completion: Some("gen:files"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _recent_files_picker = registry.register_ex_command(
        "ex:recent",
        "Open the recent-files picker (`:recent`). Alias for `:picker recent`.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| {
                Ok(Effect::OpenPicker {
                    source: "recent".into(),
                    args: Vec::new(),
                })
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _picker = registry.register_ex_command(
        "ex:picker",
        "Open a picker over the named source (`:picker <source> [args...]`). \
         Source ids come from the host's `PickerRegistry` -- type `<Tab>` after \
         `:picker ` to list them. Short aliases like `:files`, `:recent`, `:b` \
         dispatch through the same machinery.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_picker_args),
            apply: Box::new(|ctx| {
                // `parse_picker_args` always produces an `Args::List`
                // with `source` at index 0 + raw arg tokens after.
                let list = ctx
                    .args
                    .as_list()
                    .ok_or_else(|| CommandError::BadArgs("picker: expected list args".into()))?;
                let source = list
                    .first()
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| CommandError::BadArgs("picker: source missing".into()))?
                    .to_string();
                let args: Vec<String> = list[1..]
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                Ok(Effect::OpenPicker { source, args })
            }),
            args_schema: vec![ArgSpec {
                name: "source",
                kind: ArgKind::String,
                doc: "Picker source id (`files`, `recent`, `buffers`, ...).",
                prompt: "source:",
                default: ArgDefault::Required,
                // `gen:picker-sources` walks the App's
                // `picker_registry` (Arc-shared, captured Weakly
                // by the generator) and emits one candidate per
                // registered source id. Adding a new picker source
                // surfaces in `:picker <Tab>` automatically.
                completion: Some("gen:picker-sources"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let file_tree = registry.register_ex_command(
        "ex:filetree",
        "Open a file-tree buffer (`:Filetree [path]`). Absent = current dir.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| {
                let root = match &ctx.args {
                    Args::String(p) if !p.is_empty() => Some(std::path::PathBuf::from(p.as_str())),
                    _ => None,
                };
                Ok(Effect::OpenFileTree { root })
            }),
            args_schema: vec![ArgSpec {
                name: "root",
                kind: ArgKind::String,
                doc: "Directory to open as the tree root. Absent = current dir.",
                prompt: "root:",
                default: ArgDefault::None,
                completion: Some("gen:files"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let file_tree_close = registry.register_ex_command(
        "ex:filetree-close",
        "Dismiss the file-tree buffer (`:FiletreeClose`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::CloseFileTree)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let oil = registry.register_ex_command(
        "ex:oil",
        "Open an oil buffer (`:Oil [path]`). Absent = current dir.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| {
                let dir = match &ctx.args {
                    Args::String(p) if !p.is_empty() => Some(std::path::PathBuf::from(p.as_str())),
                    _ => None,
                };
                Ok(Effect::OpenOil { dir })
            }),
            args_schema: vec![ArgSpec {
                name: "dir",
                kind: ArgKind::String,
                doc: "Directory to open. Absent = current document's parent.",
                prompt: "dir:",
                default: ArgDefault::None,
                completion: Some("gen:files"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let describe_option = registry.register_ex_command(
        "ex:describe-option",
        "Open the help view for a typed option (`:describe-option NAME`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(|ctx| match &ctx.args {
                Args::String(name) => Ok(Effect::DescribeOption {
                    name: name.to_string(),
                }),
                _ => Err(CommandError::BadArgs("expected option name".into())),
            }),
            args_schema: vec![ArgSpec {
                name: "name",
                kind: ArgKind::String,
                doc: "Registered option name (or alias).",
                prompt: "option:",
                default: ArgDefault::Required,
                completion: Some("gen:options"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let list_options = registry.register_ex_command(
        "ex:options",
        "List every registered option (`:options`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::ListOptions)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let describe_events = registry.register_ex_command(
        "ex:describe-events",
        "List every registered event (`:describe-events`). Walks the \
         distributed-slice event registry and renders one row per event \
         with name + source crate + doc.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::DescribeEvents)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let diff_open = registry.register_ex_command(
        "ex:diff",
        "Open an inline diff session for the active document \
         against its on-disk content (`:diff`). D.3.a.1.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::DiffOpen)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let diff_off = registry.register_ex_command(
        "ex:diffoff",
        "Close the active pane's diff session, if any \
         (`:diffoff[!]`). D.4.d.3.a extends the v1 inline \
         `:diffoff` (D.3.a.1) to also handle two-pane \
         `:diffthis` / `:diffsplit` sessions; the session's \
         watch list is the source of truth for participants \
         (the tab is not a grouping unit). `:diffoff!` is \
         forward-compat for D.6 three-way merge — in v1 both \
         forms behave identically because removing one side \
         of a two-way diff collapses the whole session.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: true,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|ctx| Ok(Effect::DiffOff { force: ctx.bang })),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let diff_this = registry.register_ex_command(
        "ex:diffthis",
        "Stage the active pane for a two-pane diff; the second \
         `:diffthis` invocation in a different pane completes \
         the session (`DiffSession` + `PaneGroup` with \
         `HunkRowMapper` + filler providers on each side). \
         Same pane twice unstages. Third pane errors out — v1 \
         is two-way only; multi-way arrives with D.6 three-way \
         merge. D.4.d.3.a.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::Diffthis)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let diff_split = registry.register_ex_command(
        "ex:diffsplit",
        "Open `<base>` (and optionally `<remote>`) in new \
         vertical splits and register a diff session. One arg \
         ⇒ two-way diff: current pane vs the new pane loading \
         `<base>` (`:diffsplit <base>`, D.4.d.3.b). Two args ⇒ \
         three-way merge: current pane (local) vs two new \
         panes loading `<base>` (common ancestor) and \
         `<remote>` (other side) (`:diffsplit <base> <remote>`, \
         D.6.c). Composes a vsplit + `:edit` in each new pane \
         plus the appropriate `register_two_pane_diff` / \
         `register_three_pane_diff` helper. First path is \
         required; cursor lands in the first new pane.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_path),
            apply: Box::new(apply_diffsplit),
            args_schema: vec![
                ArgSpec {
                    name: "base",
                    kind: ArgKind::String,
                    doc: "File path. In two-way (single arg) this \
                          is the baseline; in three-way this is \
                          the common ancestor (base).",
                    prompt: "base path:",
                    default: ArgDefault::Required,
                    completion: Some("gen:files"),
                },
                ArgSpec {
                    name: "remote",
                    kind: ArgKind::String,
                    doc: "Optional second file path. When present, \
                          opens a three-way merge with the current \
                          pane as local, `base` as the common \
                          ancestor, and this file as the other \
                          side. D.6.c.",
                    prompt: "remote path:",
                    default: ArgDefault::None,
                    completion: Some("gen:files"),
                },
            ],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let diff_get_cmd = registry.register_ex_command(
        "ex:diffget",
        "Pull the hunk under the cursor from another buffer's \
         side into the active buffer (`:diffget [<bufnr>]`). \
         No arg: in a two-pane diff, defaults to the peer side \
         (chord-equivalent); in three-way merge, errors with \
         \"target required\". `<bufnr>` selects which side to \
         pull from — required for three-way, optional for \
         two-way. Allows resolving three-way Conflict hunks by \
         explicit pick. D.6.d.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_bufnr),
            apply: Box::new(apply_diffget),
            args_schema: vec![ArgSpec {
                name: "bufnr",
                kind: ArgKind::String,
                doc: "Optional buffer number to pull from. \
                      Required in three-way merge to disambiguate \
                      which side wins.",
                prompt: "bufnr:",
                default: ArgDefault::None,
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let diff_put_cmd = registry.register_ex_command(
        "ex:diffput",
        "Push the hunk under the cursor from the active buffer \
         into another buffer's side (`:diffput [<bufnr>]`). No \
         arg: in a two-pane diff, defaults to the peer side; in \
         three-way merge, errors with \"target required\". \
         `<bufnr>` selects the destination side — required for \
         three-way, optional for two-way. Allows resolving \
         three-way Conflict hunks by explicit pick. D.6.d.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_bufnr),
            apply: Box::new(apply_diffput),
            args_schema: vec![ArgSpec {
                name: "bufnr",
                kind: ArgKind::String,
                doc: "Optional buffer number to push to. \
                      Required in three-way merge to disambiguate \
                      which side receives the edit.",
                prompt: "bufnr:",
                default: ArgDefault::None,
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let diff_accept = registry.register_ex_command(
        "ex:diff-accept",
        "Resolve the active pane's diff session with \
         `DiffOutcome::Accept`. Tears down the session \
         (`:diffoff`-equivalent) and fires the Accept \
         signal on any bound completion channel. The \
         buffer's current content is the accepted \
         resolution — plugins consuming the outcome commit \
         from there. D.6.e.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::DiffAccept)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let diff_reject = registry.register_ex_command(
        "ex:diff-reject",
        "Resolve the active pane's diff session with \
         `DiffOutcome::Reject`. Tears down the session \
         (`:diffoff!`-equivalent) and fires the Reject \
         signal on any bound completion channel. Plugins \
         consuming the outcome should revert any \
         pre-session state. D.6.e.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::DiffReject)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let hunk_next = registry.register_ex_command(
        "ex:hunk-next",
        "Jump cursor to the start of the next diff hunk on \
         the current side. Wraps to top. `]c` / `:hunk-next`. \
         D.3.c.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::NextHunk)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let hunk_prev = registry.register_ex_command(
        "ex:hunk-prev",
        "Jump cursor to the start of the previous diff hunk \
         on the current side. Wraps to bottom. `[c` / \
         `:hunk-prev`. D.3.c.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::PrevHunk)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let describe_diff = registry.register_ex_command(
        "ex:describe-diff",
        "List every active diff session (`:describe-diff`). D.2.d. \
         Walks `DiffSubsystem::describe_sessions` and renders one row \
         per session with BufferId, algorithm, current revision, hunk \
         count, and watched buffers.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::DescribeDiff)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let describe_event = registry.register_ex_command(
        "ex:describe-event",
        "Open the help view for a registered event (`:describe-event NAME`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(|ctx| match &ctx.args {
                Args::String(name) => Ok(Effect::DescribeEvent {
                    name: name.to_string(),
                }),
                _ => Err(CommandError::BadArgs("expected event name".into())),
            }),
            args_schema: vec![ArgSpec {
                name: "name",
                kind: ArgKind::String,
                doc: "Registered event name (e.g. `lsp.buffer-attached`).",
                prompt: "event:",
                default: ArgDefault::Required,
                completion: Some("gen:events"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let list_modes = registry.register_ex_command(
        "ex:list-modes",
        "List every registered mode (`:list-modes`). Groups by \
         kind (Major / Minor) and shows each mode's current \
         activation state on the active buffer.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::ListModes)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let describe_mode = registry.register_ex_command(
        "ex:describe-mode",
        "Open the help view for a registered mode \
         (`:describe-mode NAME`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(|ctx| match &ctx.args {
                Args::String(name) => Ok(Effect::DescribeMode {
                    name: name.to_string(),
                }),
                _ => Err(CommandError::BadArgs("expected mode name".into())),
            }),
            args_schema: vec![ArgSpec {
                name: "name",
                kind: ArgKind::String,
                doc: "Registered mode name (e.g. `lsp-mode`, `text-mode`).",
                prompt: "mode:",
                default: ArgDefault::Required,
                completion: Some("gen:modes"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let describe_option_resolution = registry.register_ex_command(
        "ex:describe-option-resolution",
        "Show which resolver layer provides the resolved value \
         for an option on the active buffer \
         (`:describe-option-resolution NAME`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(|ctx| match &ctx.args {
                Args::String(name) => Ok(Effect::DescribeOptionResolution {
                    name: name.to_string(),
                }),
                _ => Err(CommandError::BadArgs("expected option name".into())),
            }),
            args_schema: vec![ArgSpec {
                name: "name",
                kind: ArgKind::String,
                doc: "Registered option name (e.g. `number`, `tabstop`).",
                prompt: "option:",
                default: ArgDefault::Required,
                completion: Some("gen:options"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let tutor = registry.register_ex_command(
        "ex:tutor",
        "Open the interactive Lattice tutor lesson \
         (`:tutor [N]`). With no arg, opens lesson 1. The \
         lesson is embedded in the binary; each invocation \
         copies a fresh practice file to a temp path so you \
         can edit / practice without losing the canonical \
         lesson source. Run `:tutor` again to start over.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| match &ctx.args {
                Args::None => Ok(Effect::Tutor { lesson: None }),
                Args::String(s) => {
                    let n: u32 = s.parse().map_err(|_| {
                        CommandError::BadArgs(format!("expected lesson number, got `{s}`"))
                    })?;
                    Ok(Effect::Tutor { lesson: Some(n) })
                }
                _ => Err(CommandError::BadArgs(
                    "expected at most one numeric argument".into(),
                )),
            }),
            args_schema: vec![ArgSpec {
                name: "lesson",
                kind: ArgKind::String,
                doc: "Lesson number (default: 1).",
                prompt: "lesson:",
                default: ArgDefault::None,
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let tutor_next = registry.register_ex_command(
        "ex:tutor-next",
        "Advance to the next tutor exercise (or lesson). \
         Equivalent to pressing `<CR>` in tutor-mode.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::AppAction(AppEffect::TutorAdvance))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let tutor_prev = registry.register_ex_command(
        "ex:tutor-prev",
        "Retreat to the previous tutor exercise.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::AppAction(AppEffect::TutorRetreat))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let customize = registry.register_ex_command(
        "ex:customize",
        "Open the customize buffer (`:customize [name]`). With no \
         arg, opens the picker listing every registered group + \
         every mode with at least one customizable option. With \
         a `<name>-mode` arg, opens the focused view of that \
         mode's contributions. With a group name (no `-mode` \
         suffix), opens the cross-mode group view.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| match &ctx.args {
                Args::None => Ok(Effect::Customize { name: None }),
                Args::String(name) => Ok(Effect::Customize {
                    name: Some(name.to_string()),
                }),
                _ => Err(CommandError::BadArgs(
                    "expected at most one argument".into(),
                )),
            }),
            args_schema: vec![ArgSpec {
                name: "name",
                kind: ArgKind::String,
                doc: "Group name (e.g. `editor`, `lsp`) OR mode name \
                      ending in `-mode` (e.g. `lsp-completion-mode`).",
                prompt: "customize:",
                default: ArgDefault::None,
                completion: Some("gen:customize"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let hover = registry.register_ex_command(
        "ex:hover",
        "Open a hover popup at the cursor (`:hover [markdown]`). v1 path: feed text manually; \
         Phase 4 LSP will source from `textDocument/hover`.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| {
                let markdown = match &ctx.args {
                    Args::String(s) if !s.is_empty() => s.to_string(),
                    _ => "(empty hover)".to_string(),
                };
                Ok(Effect::OpenHover { markdown })
            }),
            args_schema: vec![ArgSpec {
                name: "markdown",
                kind: ArgKind::String,
                doc: "Markdown body of the hover popup.",
                prompt: "hover:",
                default: ArgDefault::None,
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let hover_close = registry.register_ex_command(
        "ex:hover-close",
        "Dismiss the active hover popup (`:HoverClose`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::CloseHover)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // ---- LSP introspection (Phase 4.1.g) -------------------
    let messages = registry.register_ex_command(
        "ex:messages",
        "Open the `*messages*` buffer -- the emacs `*Messages*` analogue carrying every minibuffer echo / notification with timestamps (`:messages`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_ctx| Ok(Effect::OpenMessages)),
            args_schema: Vec::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_log = registry.register_ex_command(
        "ex:lsp-log",
        "Open the LSP subsystem log (`*lsp*`) or a per-server log (`*lsp:<server>*`) (`:lsp-log [server]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| {
                let server_id = match &ctx.args {
                    Args::String(s) if !s.is_empty() => Some(s.to_string()),
                    _ => None,
                };
                Ok(Effect::OpenLspLog { server_id })
            }),
            args_schema: vec![ArgSpec {
                name: "server",
                kind: ArgKind::String,
                doc: "Server id (e.g. `rust`, `python`). Absent = subsystem-wide log.",
                prompt: "server:",
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_trace = registry.register_ex_command(
        "ex:lsp-trace",
        "Toggle JSON-RPC wire trace for a server (pure toggle; view records via `:lsp-trace-log <server>`) (`:lsp-trace <server>`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(|ctx| match &ctx.args {
                Args::String(s) if !s.is_empty() => Ok(Effect::ToggleLspTrace {
                    server_id: s.to_string(),
                }),
                _ => Err(CommandError::BadArgs(
                    ":lsp-trace requires a server id".into(),
                )),
            }),
            args_schema: vec![ArgSpec {
                name: "server",
                kind: ArgKind::String,
                doc: "Server id to toggle trace on.",
                prompt: "server:",
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _lsp_trace_log = registry.register_ex_command(
        "ex:lsp-trace-log",
        "Open the JSON-RPC trace ring for an LSP server (`:lsp-trace-log [server]`). No arg = picker over every running instance; arg = pre-filter (single match short-circuits). Independent of `:lsp-trace`.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| {
                let server_id = match &ctx.args {
                    Args::String(s) if !s.is_empty() => Some(s.to_string()),
                    _ => None,
                };
                Ok(Effect::OpenLspTraceLog { server_id })
            }),
            args_schema: vec![ArgSpec {
                name: "server",
                kind: ArgKind::String,
                doc: "Server id (e.g. `rust`). Absent = picker over every running instance.",
                prompt: "server:",
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_status = registry.register_ex_command(
        "ex:lsp-status",
        "Render every running LSP server (id, root, pid, uptime) in a help-style buffer (`:lsp-status`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::LspStatus)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_server_log = registry.register_ex_command(
        "ex:lsp-server-log",
        "Picker-style listing of every running LSP server actor with workspace root + buffer count + capability summary; each row links to its log + trace via `exec:` URLs (`:lsp-server-log`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::LspServerLogListing)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_restart = registry.register_ex_command(
        "ex:lsp-restart",
        "Force-restart a stuck LSP server. Wired but no-op until the supervisor restart path lands in 4.4 (`:lsp-restart <server>`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(|ctx| match &ctx.args {
                Args::String(s) if !s.is_empty() => Ok(Effect::LspRestart {
                    server_id: s.to_string(),
                }),
                _ => Err(CommandError::BadArgs(
                    ":lsp-restart requires a server id".into(),
                )),
            }),
            args_schema: vec![ArgSpec {
                name: "server",
                kind: ArgKind::String,
                doc: "Server id to restart.",
                prompt: "server:",
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_progress_cancel = registry.register_ex_command(
        "ex:lsp-progress-cancel",
        "Cancel cancellable LSP $/progress operations on the named server (or every server attached to the active buffer if omitted) (`:lsp-progress-cancel [server]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| match &ctx.args {
                Args::String(s) if !s.trim().is_empty() => Ok(Effect::LspProgressCancel {
                    server_id: Some(s.trim().to_string()),
                }),
                _ => Ok(Effect::LspProgressCancel { server_id: None }),
            }),
            args_schema: vec![ArgSpec {
                name: "server",
                kind: ArgKind::String,
                doc: "Optional server id; omit to cancel on every attached server.",
                prompt: "server:",
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_expand_region = registry.register_ex_command(
        "ex:lsp-expand-region",
        "Smart-expansion: walk one step outward in the LSP selectionRange chain (`:lsp-expand-region`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::LspExpandRegion)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_shrink_region = registry.register_ex_command(
        "ex:lsp-shrink-region",
        "Walk one step inward in the cached LSP selectionRange chain (`:lsp-shrink-region`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::LspShrinkRegion)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_log_level = registry.register_ex_command(
        "ex:lsp-log-level",
        "Set the subsystem-wide default min log level (or a per-server override) (`:lsp-log-level [server] <level>`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_required_string),
            apply: Box::new(|ctx| match &ctx.args {
                Args::String(s) if !s.is_empty() => {
                    let trimmed = s.trim();
                    let mut parts = trimmed.split_whitespace();
                    let first = parts.next().unwrap_or("");
                    let second = parts.next();
                    let (server_id, level) = match second {
                        Some(level) => (Some(first.to_string()), level.to_string()),
                        None => (None, first.to_string()),
                    };
                    Ok(Effect::SetLspLogLevel { server_id, level })
                }
                _ => Err(CommandError::BadArgs(
                    ":lsp-log-level requires `[server] <level>`".into(),
                )),
            }),
            args_schema: vec![ArgSpec {
                name: "spec",
                kind: ArgKind::String,
                // Single-token completion lands the level form
                // (`info`, `debug`, ...). The two-token
                // `<server> <level>` form parses correctly at
                // submit; v1 ships completion for the common
                // (subsystem-wide) shape.
                doc: "Either a level (`error`/`warn`/`info`/`debug`/`trace`) for the subsystem default, or `<server> <level>` for a per-server override.",
                prompt: "[server] level:",
                default: ArgDefault::None,
                completion: Some("gen:log-levels"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_log_clear = registry.register_ex_command(
        "ex:lsp-log-clear",
        "Drop the records in `*lsp*` (no arg) or `*lsp:<server>*` (with arg) (`:lsp-log-clear [server]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| {
                let server_id = match &ctx.args {
                    Args::String(s) if !s.is_empty() => Some(s.to_string()),
                    _ => None,
                };
                Ok(Effect::LspLogClear { server_id })
            }),
            args_schema: vec![ArgSpec {
                name: "server",
                kind: ArgKind::String,
                doc: "Server id whose ring to clear. Absent = subsystem-wide.",
                prompt: "server:",
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers"),
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let lsp_symbols = registry.register_ex_command(
        "ex:lsp-symbols",
        "Open a vertico picker over the active document's symbol outline (`:lsp-symbols`, Phase 4.2.e).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::LspDocumentSymbol)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_format = registry.register_ex_command(
        "ex:lsp-format",
        "Run `textDocument/formatting` on the active buffer's highest-priority LSP server; apply the returned edits as one undo unit (`:format`, Phase 4.3).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::LspFormat)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_format_range = registry.register_ex_command(
        "ex:lsp-format-range",
        "Run `textDocument/rangeFormatting` over the active Visual selection or the supplied line range (`:[range]format-range`, Phase 4.3).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: true,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::LspFormatRange)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let lsp_code_action = registry.register_ex_command(
        "ex:lsp-code-action",
        "Open a vertico picker over LSP code actions at the cursor / selection (`:code-actions`, Phase 4.3).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: true,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::LspCodeAction)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let lsp_rename = registry.register_ex_command(
        "ex:lsp-rename",
        "Rename the symbol under cursor across the workspace via textDocument/rename. Empty name uses prepareRename's placeholder when advertised (`:rename [new-name]`, Phase 4.3).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| {
                let new_name = match &ctx.args {
                    Args::String(s) => s.trim().to_string(),
                    _ => String::new(),
                };
                Ok(Effect::LspRename { new_name })
            }),
            args_schema: vec![ArgSpec {
                name: "new-name",
                kind: ArgKind::String,
                doc: "Replacement identifier. Empty -> use the server's prepareRename placeholder.",
                prompt: "new name:",
                default: ArgDefault::None,
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let lsp_complete = registry.register_ex_command(
        "ex:lsp-complete",
        "Open a vertico picker over LSP completion items at the cursor (`:complete`, Phase 4.2.g). Plain-text insert -- snippet expansion / lazy resolve land with buffer-level Insert-mode completion.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::LspComplete)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let lsp_signature_help = registry.register_ex_command(
        "ex:lsp-signature-help",
        "Open the LSP signature-help popup for the current cursor (`:signature-help`, Phase 4.3). Trigger-character driven in Insert mode -- typing `(` / `,` etc. fires the same request automatically.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::LspSignatureHelp)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let lsp_workspace_symbol = registry.register_ex_command(
        "ex:lsp-workspace-symbol",
        "Open a vertico picker over workspace symbols matching `query` (server-side substring filter; `:lsp-workspace-symbol [query]`, Phase 4.2.f).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| {
                let query = match &ctx.args {
                    Args::String(s) => s.to_string(),
                    _ => String::new(),
                };
                Ok(Effect::LspWorkspaceSymbol { query })
            }),
            args_schema: vec![ArgSpec {
                name: "query",
                kind: ArgKind::String,
                doc: "Server-side substring filter; empty returns the server's idea of \"every workspace symbol\".",
                prompt: "query:",
                default: ArgDefault::None,
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let lsp_incoming_calls = registry.register_ex_command(
        "ex:lsp-incoming-calls",
        "Open a vertico picker over the callers of the function at the cursor (`textDocument/prepareCallHierarchy` -> `callHierarchy/incomingCalls`, Phase 4.5.a).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_ctx| Ok(Effect::LspIncomingCalls)),
            args_schema: Vec::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_outgoing_calls = registry.register_ex_command(
        "ex:lsp-outgoing-calls",
        "Open a vertico picker over the callees of the function at the cursor (`textDocument/prepareCallHierarchy` -> `callHierarchy/outgoingCalls`, Phase 4.5.a).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_ctx| Ok(Effect::LspOutgoingCalls)),
            args_schema: Vec::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_supertypes = registry.register_ex_command(
        "ex:lsp-supertypes",
        "Open a vertico picker over the types the type at the cursor subtypes (`textDocument/prepareTypeHierarchy` -> `typeHierarchy/supertypes`, Phase 4.5.b).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_ctx| Ok(Effect::LspSupertypes)),
            args_schema: Vec::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_subtypes = registry.register_ex_command(
        "ex:lsp-subtypes",
        "Open a vertico picker over the subtypes of the type at the cursor (`textDocument/prepareTypeHierarchy` -> `typeHierarchy/subtypes`, Phase 4.5.b).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_ctx| Ok(Effect::LspSubtypes)),
            args_schema: Vec::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_moniker = registry.register_ex_command(
        "ex:lsp-moniker",
        "Fire `textDocument/moniker` at the cursor and echo the result (cross-project symbol identifier, Phase 4.5.g).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_ctx| Ok(Effect::LspMoniker)),
            args_schema: Vec::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_code_lens = registry.register_ex_command(
        "ex:lsp-code-lens",
        "Open a picker over the active buffer's cached code lenses; accept routes the chosen lens through `workspace/executeCommand` (Phase 4.5.d).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_ctx| Ok(Effect::LspCodeLens)),
            args_schema: Vec::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
    let lsp_color_presentation = registry.register_ex_command(
        "ex:lsp-color-presentation",
        "At the cursor, look up the color literal in the documentColor cache and open a picker of alternative formats (`textDocument/colorPresentation`, Phase 4.5.e).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_ctx| Ok(Effect::LspColorPresentation)),
            args_schema: Vec::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );

    let list_diagnostics = registry.register_ex_command(
        "ex:diagnostics",
        "Open a help-style buffer listing every workspace diagnostic with clickable per-entry source links (`:diagnostics`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::ListDiagnostics)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let next_diagnostic = registry.register_ex_command(
        "ex:diag-next",
        "Move the cursor to the next diagnostic in the active buffer (wraps; `]d` / `:diag-next` / `:cnext`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::NextDiagnostic)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let prev_diagnostic = registry.register_ex_command(
        "ex:diag-prev",
        "Move the cursor to the previous diagnostic in the active buffer (wraps; `[d` / `:diag-prev` / `:cprev`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::PrevDiagnostic)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // SN.3c.1 (2026-06-14): `:snippet-expand` ex-command removed.
    // The only live snippet-expand trigger is `<C-x><C-s>`, now
    // mode-owned by `snippet-mode` (`keymap()` + `action_handlers()`
    // → `Effect::ExpandSnippet`). An ex-command surface form was
    // UX-useless, so it's gone rather than re-routed.
    let reload_snippets = registry.register_ex_command(
        "ex:reload-snippets",
        "Re-read every snippet file from disk and rebuild the per-language snippet registry (`:reload-snippets`, Phase 4.2.g.4).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::ReloadSnippets)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let help = registry.register_ex_command(
        "ex:help",
        "Open the topic index or a named help topic (`:help [topic]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_path),
            apply: Box::new(|ctx| {
                let topic = match &ctx.args {
                    Args::String(s) if !s.is_empty() => Some(s.to_string()),
                    _ => None,
                };
                Ok(Effect::OpenHelpTopic { topic })
            }),
            args_schema: vec![ArgSpec {
                name: "topic",
                kind: ArgKind::String,
                doc: "Topic name (`folding`, `buffers`, ...). Absent = index.",
                prompt: "topic:",
                default: ArgDefault::None,
                completion: Some("gen:help-topics"),
            }],
            surface_form: SurfaceForm::Keyword,
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
        set_local_option,
        set_global_option,
        edit,
        substitute,
        global,
        describe_command,
        describe_buffer,
        apropos,
        describe_key,
        list_keymap,
        buffer_next,
        buffer_prev,
        list_buffers,
        buffer_delete,
        file_tree,
        file_tree_close,
        oil,
        describe_option,
        list_options,
        describe_events,
        describe_event,
        describe_diff,
        diff_open,
        diff_off,
        diff_this,
        diff_split,
        diff_get_cmd,
        diff_put_cmd,
        diff_accept,
        diff_reject,
        hunk_next,
        hunk_prev,
        list_modes,
        describe_mode,
        describe_option_resolution,
        customize,
        tutor,
        tutor_next,
        tutor_prev,
        hover,
        hover_close,
        help,
        list_diagnostics,
        next_diagnostic,
        prev_diagnostic,
        lsp_log,
        messages,
        lsp_trace,
        lsp_status,
        lsp_server_log,
        lsp_restart,
        lsp_progress_cancel,
        lsp_expand_region,
        lsp_shrink_region,
        lsp_log_level,
        lsp_log_clear,
        lsp_symbols,
        lsp_workspace_symbol,
        lsp_incoming_calls,
        lsp_outgoing_calls,
        lsp_supertypes,
        lsp_subtypes,
        lsp_moniker,
        lsp_code_lens,
        lsp_color_presentation,
        lsp_format,
        lsp_format_range,
        lsp_signature_help,
        lsp_complete,
        lsp_rename,
        lsp_code_action,
        reload_snippets,
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

/// D.4.d.3.b / D.6.c: 1-or-2-path parser for
/// `:diffsplit <file> [<remote>]`. Joins the two paths
/// with a `\x1f` (unit separator) inside `Args::String` so
/// the apply callback can split them back out — `Args` has
/// no `Vec<String>` variant in v1 and adding one for a
/// single 2-arg ex-command is more surface than it's
/// worth. Whitespace splits the two args; the second is
/// optional. Empty first arg errors at parse time.
///
/// Vim's `:diffsplit` is single-arg only — the optional
/// second arg is a Lattice extension for three-way merge
/// (D.6.c). The split-by-whitespace shape matches vim's
/// `vimdiff a b c` argv conventions.
fn parse_required_path(rest: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Err(CommandError::BadArgs(
            "expected file path (`:diffsplit <base> [<remote>]`)".into(),
        ));
    }
    let mut parts = trimmed.split_whitespace();
    let base = parts.next().expect("non-empty after trim");
    let remote = parts.next();
    if parts.next().is_some() {
        return Err(CommandError::BadArgs(
            ":diffsplit takes at most two paths (\
             `:diffsplit <base> [<remote>]`)"
                .into(),
        ));
    }
    let encoded = match remote {
        Some(r) => format!("{base}\x1f{r}"),
        None => base.to_string(),
    };
    Ok(Args::String(encoded))
}

/// D.6.d (2026-05-31): optional `<bufnr>` parser for
/// `:diffput [<bufnr>]` / `:diffget [<bufnr>]`. Empty arg
/// ⇒ `Args::None` (chord-equivalent semantics); non-empty
/// ⇒ `Args::String(bufnr)` after validating it's a
/// non-negative u32.
fn parse_optional_bufnr(rest: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Ok(Args::None);
    }
    trimmed.parse::<u32>().map_err(|e| {
        CommandError::BadArgs(format!(
            "bufnr must be a non-negative integer: {e}"
        ))
    })?;
    Ok(Args::String(trimmed.to_string()))
}

/// D.6.d apply callback for `:diffget [<bufnr>]`. `None`
/// arg → `target = None` (chord semantics: peer in 2-way,
/// "required" error in 3-way). `Some(bufnr)` →
/// `target = Some(bufnr as u32)`.
fn apply_diffget(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    let target = match &ctx.args {
        Args::None => None,
        Args::String(s) => Some(s.parse::<u32>().map_err(|e| {
            CommandError::BadArgs(format!("bufnr: {e}"))
        })?),
        _ => {
            return Err(CommandError::BadArgs(
                "expected optional bufnr (`:diffget [<bufnr>]`)".into(),
            ));
        }
    };
    Ok(Effect::DiffGetCmd { target })
}

/// D.6.d apply callback for `:diffput [<bufnr>]`. Mirror
/// of [`apply_diffget`] for the put direction.
fn apply_diffput(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    let target = match &ctx.args {
        Args::None => None,
        Args::String(s) => Some(s.parse::<u32>().map_err(|e| {
            CommandError::BadArgs(format!("bufnr: {e}"))
        })?),
        _ => {
            return Err(CommandError::BadArgs(
                "expected optional bufnr (`:diffput [<bufnr>]`)".into(),
            ));
        }
    };
    Ok(Effect::DiffPutCmd { target })
}

/// D.4.d.3.b / D.6.c: apply callback for `:diffsplit`.
/// Decodes the parser's `\x1f`-joined paths (when present)
/// and emits `Effect::Diffsplit { path, remote }`. Single
/// path ⇒ `remote = None` (two-way). Two paths ⇒
/// `remote = Some` (three-way merge).
fn apply_diffsplit(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    let encoded = match &ctx.args {
        Args::String(s) => s,
        _ => {
            return Err(CommandError::BadArgs(
                "expected file path (`:diffsplit <base> [<remote>]`)".into(),
            ));
        }
    };
    let (path, remote) = match encoded.split_once('\x1f') {
        Some((base, rem)) => (
            std::path::PathBuf::from(base),
            Some(std::path::PathBuf::from(rem)),
        ),
        None => (std::path::PathBuf::from(encoded), None),
    };
    Ok(Effect::Diffsplit { path, remote })
}

/// `:tabmove [N]` parser (issue #29 slice 3). Optional
/// positional integer; missing → `0` (vim's "move to last").
/// Stored as `Args::String` because `Args` has no Int variant;
/// the apply closure re-parses.
fn parse_tabmove_arg(rest: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Ok(Args::String("0".to_string()));
    }
    // Validate it's a non-negative integer before passing on.
    trimmed.parse::<u32>().map_err(|e| {
        CommandError::BadArgs(format!(
            ":tabmove arg must be a non-negative integer: {e}"
        ))
    })?;
    Ok(Args::String(trimmed.to_string()))
}

fn parse_required_string(rest: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        Err(CommandError::BadArgs("argument required".into()))
    } else {
        Ok(Args::String(trimmed.to_string()))
    }
}

/// `:picker <source> [args...]` parser. First whitespace-delimited
/// token is the source id; the rest pass through as `Raw` values
/// the App's source-specific handler re-interprets against the
/// resolved [`PickerSourceSpec::args_schema`]. The grammar stays
/// agnostic of which sources exist and what args they take --
/// validation happens host-side against the registry.
fn parse_picker_args(rest: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Err(CommandError::BadArgs(
            "picker source required (e.g. `:picker files`)".into(),
        ));
    }
    let mut tokens = trimmed.split_whitespace();
    let source = tokens
        .next()
        .expect("non-empty after trim guarantees at least one token");
    let mut values: Vec<ArgValue> = Vec::with_capacity(1);
    values.push(ArgValue::String(source.to_string()));
    for tok in tokens {
        values.push(ArgValue::Raw(tok.to_string()));
    }
    Ok(Args::List(values))
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

fn apply_set_local(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    match &ctx.args {
        Args::String(s) => Ok(Effect::SetLocalOption { spec: s.clone() }),
        _ => Err(CommandError::BadArgs("expected option string".into())),
    }
}

fn apply_set_global(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    match &ctx.args {
        Args::String(s) => Ok(Effect::SetGlobalOption { spec: s.clone() }),
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
        Args::String(s) => Ok(Effect::DescribeCommand {
            name: s.clone(),
            anchor: None,
        }),
        _ => Err(CommandError::BadArgs("expected command name string".into())),
    }
}

fn apply_apropos(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    match &ctx.args {
        Args::String(s) => Ok(Effect::Apropos { pattern: s.clone() }),
        _ => Err(CommandError::BadArgs("expected pattern string".into())),
    }
}

fn apply_describe_key(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    match &ctx.args {
        Args::String(s) => Ok(Effect::DescribeKey { chord: s.clone() }),
        _ => Err(CommandError::BadArgs(
            "expected chord notation string".into(),
        )),
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
        .as_invocation()
        .ok_or_else(|| {
            CommandError::BadArgs(
                "arg 2 (body) must be a parsed Invocation -- parser front-end is responsible"
                    .into(),
            )
        })?
        .clone();
    Ok(Effect::Global {
        pattern,
        inverted,
        body: Box::new(body),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::CancellationToken;
    use crate::command::CommandInvocation;
    use crate::dispatcher::execute;
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
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::SaveBuffer { path } => assert!(path.is_none()),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn write_with_path_carries_path() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.write.0).with_args(Args::String("foo.txt".into()));
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::SaveBuffer { path: Some(p) } => {
                assert_eq!(p, std::path::PathBuf::from("foo.txt"))
            }
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn quit_bang_propagates_to_force() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.quit.0).with_bang(true);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::QuitEditor { force } => assert!(force),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn quit_no_bang_is_not_forced() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.quit.0);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::QuitEditor { force } => assert!(!force),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn write_quit_emits_many_save_then_quit() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.write_quit.0).with_bang(true);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
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
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::ClearSearchHighlight));
    }

    #[test]
    fn registers_emits_echo_registers() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.list_registers.0);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::EchoRegisters));
    }

    #[test]
    fn marks_emits_echo_marks() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.list_marks.0);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::EchoMarks));
    }

    #[test]
    fn delete_emits_delete_current_line() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.delete_line.0);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::DeleteCurrentLine));
    }

    #[test]
    fn describe_command_emits_describe_command_effect() {
        let (registry, ex, mut doc) = fixture();
        let inv =
            CommandInvocation::of(ex.describe_command.0).with_args(Args::String("ex:write".into()));
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::DescribeCommand { name, anchor } => {
                assert_eq!(name, "ex:write");
                assert!(anchor.is_none());
            }
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn describe_buffer_emits_describe_buffer_effect() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.describe_buffer.0);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::DescribeBuffer));
    }

    #[test]
    fn apropos_emits_apropos_effect() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.apropos.0).with_args(Args::String("write".into()));
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
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
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
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
        let err = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::BadArgs(_)));
    }

    #[test]
    fn edit_with_path_and_bang_carries_force() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.edit.0)
            .with_args(Args::String("/tmp/x".into()))
            .with_bang(true);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
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
    fn parse_picker_args_splits_source_then_raw_tokens() {
        // Empty input rejects -- source is required.
        assert!(matches!(
            parse_picker_args("", false),
            Err(CommandError::BadArgs(_))
        ));
        assert!(matches!(
            parse_picker_args("   ", false),
            Err(CommandError::BadArgs(_))
        ));
        // Just a source id -> single-element list with String source.
        match parse_picker_args("files", false).unwrap() {
            Args::List(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].as_str(), Some("files"));
                assert!(matches!(v[0], ArgValue::String(_)));
            }
            other => panic!("unexpected args: {other:?}"),
        }
        // Source + rest -> first is String source, rest are Raw tokens
        // the App handler will re-interpret per source-specific shape.
        match parse_picker_args("files  /tmp/a  /tmp/b", false).unwrap() {
            Args::List(v) => {
                assert_eq!(v.len(), 3);
                assert_eq!(v[0].as_str(), Some("files"));
                assert!(matches!(v[0], ArgValue::String(_)));
                assert_eq!(v[1].as_str(), Some("/tmp/a"));
                assert!(matches!(v[1], ArgValue::Raw(_)));
                assert_eq!(v[2].as_str(), Some("/tmp/b"));
                assert!(matches!(v[2], ArgValue::Raw(_)));
            }
            other => panic!("unexpected args: {other:?}"),
        }
    }

    #[test]
    fn picker_command_applies_into_open_picker_effect() {
        let (registry, _ex, mut doc) = fixture();
        let picker_id = registry
            .id_by_name("ex:picker")
            .expect("ex:picker must be registered");
        let args = parse_picker_args("files /tmp/a", false).unwrap();
        let inv = CommandInvocation::of(picker_id).with_args(args);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0), Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::OpenPicker { source, args } => {
                assert_eq!(source, "files");
                assert_eq!(args, vec!["/tmp/a".to_string()]);
            }
            other => panic!("expected Effect::OpenPicker, got {other:?}"),
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

    #[test]
    fn option_describing_commands_complete_against_gen_options() {
        // `:describe-option NAME` and `:describe-option-resolution
        // NAME` both consume a typed-option name; both must offer
        // the same completion source so the user gets the same
        // candidates after `<Tab>` regardless of which command
        // they typed.
        let (registry, ex, _) = fixture();
        for id in [ex.describe_option, ex.describe_option_resolution] {
            let spec = registry.ex_command_spec(id.0).unwrap();
            assert_eq!(spec.args_schema.len(), 1);
            assert_eq!(spec.args_schema[0].completion, Some("gen:options"));
        }
    }

    #[test]
    fn introspection_commands_advertise_completion_sources() {
        // Every `:describe-*` / `:customize` / `:lsp-*` command
        // that consumes a registry-like arg must advertise a
        // completion source so `<Tab>` returns candidates. Source
        // names are stable; the boot path registers a generator
        // per name in `lattice-ui-tui::host_generators`.
        let (registry, ex, _) = fixture();
        let cases: &[(crate::ExCommandId, &str)] = &[
            (ex.describe_command, "gen:commands"),
            (ex.describe_event, "gen:events"),
            (ex.describe_mode, "gen:modes"),
            (ex.describe_option, "gen:options"),
            (ex.describe_option_resolution, "gen:options"),
            (ex.customize, "gen:customize"),
        ];
        for (id, expected) in cases {
            let cmd = registry.lookup(id.0).unwrap();
            let spec = registry.ex_command_spec(id.0).unwrap();
            assert_eq!(
                spec.args_schema[0].completion,
                Some(*expected),
                "{} should complete against {expected}",
                cmd.name
            );
        }
    }

    #[test]
    fn lsp_admin_commands_complete_against_server_ids() {
        // `:lsp-log`, `:lsp-trace`, `:lsp-trace-log`,
        // `:lsp-restart`, `:lsp-log-clear` all take a server-id
        // argument; each must offer `gen:lsp-servers` so `<Tab>`
        // surfaces the currently-running set.
        let (registry, ex, _) = fixture();
        let cases: &[crate::ExCommandId] =
            &[ex.lsp_log, ex.lsp_trace, ex.lsp_restart, ex.lsp_log_clear];
        for id in cases {
            let cmd = registry.lookup(id.0).unwrap();
            let spec = registry.ex_command_spec(id.0).unwrap();
            assert_eq!(
                spec.args_schema[0].completion,
                Some("gen:lsp-servers"),
                "{} should complete against gen:lsp-servers",
                cmd.name
            );
        }
        // `:lsp-log-level` completes against the level palette
        // (subsystem-wide common form). The two-token
        // `<server> <level>` form parses correctly at submit; only
        // the first token gets candidates today.
        let cmd = registry.lookup(ex.lsp_log_level.0).unwrap();
        let spec = registry.ex_command_spec(ex.lsp_log_level.0).unwrap();
        assert_eq!(
            spec.args_schema[0].completion,
            Some("gen:log-levels"),
            "{} should complete against gen:log-levels",
            cmd.name
        );
    }
}
