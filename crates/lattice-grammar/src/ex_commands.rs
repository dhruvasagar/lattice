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

use crate::AppEffect;
use crate::args::{ArgDefault, ArgKind, ArgSpec, ArgValue, Args};
use crate::command::LatencyClass;
use crate::effect::{Effect, LspRequest, QuitScope, SubstituteScope};
use crate::error::{CommandError, GrammarResult};
use crate::range::Range;
use crate::registry::{CommandRegistry, ExCommandContext, ExCommandId, ExCommandSpec, SurfaceForm};
use std::sync::Arc;

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
    /// T.9.b (2026-06-18): `:colorscheme <name>` — swap the active
    /// theme by name (`lattice-host` looks the name up in
    /// `lattice_theme::builtin_themes()` and calls `set_theme`).
    pub colorscheme: ExCommandId,
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
    /// T.9.d: `:describe-element` / `:describe-face` — theme-element
    /// introspection (host `build_describe_element_content`).
    pub describe_element: ExCommandId,
    pub list_options: ExCommandId,
    /// PI.2: plugin-API introspection (`:describe-plugin-api [<seam>]`,
    /// `:list-plugin-apis`). The catalog is derived from `wit/` at build
    /// time by `lattice-plugin-api`; the host renders it.
    pub describe_plugin_api: ExCommandId,
    pub list_plugin_apis: ExCommandId,
    /// PI.2b: `:export-plugin-api [markdown|json]` -- dump the catalog to a
    /// savable buffer.
    pub export_plugin_api: ExCommandId,
    /// PI.3: `:list-commands` -- enumerate every command, source-grouped.
    pub list_commands: ExCommandId,
    /// PI.4: `:describe-plugin <name>` / `:list-plugins` -- loaded-plugin
    /// introspection (Facet B).
    pub describe_plugin: ExCommandId,
    pub list_plugins: ExCommandId,
    pub describe_events: ExCommandId,
    pub describe_event: ExCommandId,
    // CR.6 (2026-06-24): the 11 diff/hunk ex-command ids
    // (`describe_diff`/`diff_open`/`diff_off`/`diff_this`/`diff_split`/
    // `diff_get_cmd`/`diff_put_cmd`/`diff_accept`/`diff_reject`/`hunk_next`/
    // `hunk_prev`) are gone — the diff subsystem registers its own commands
    // in `lattice_diff::install()` (the multibuffer pattern). They were
    // unused outside this crate (name-resolved at the `:` line).
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
    /// IN.8b: `:format` — the LSP-independent cascade.
    pub format: ExCommandId,
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
    pub cd: ExCommandId,
    pub pwd: ExCommandId,
    /// PR.2: `:project-root` — the introspection affordance for project
    /// resolution.
    pub project_root: ExCommandId,
}

pub fn populate(registry: &mut CommandRegistry) -> ExBuiltins {
    let write = registry.register_ex_command(
        "ex:write",
        "Write the current buffer to disk (`:w [path]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(apply_write),
            args_schema: vec![ArgSpec {
                name: "path".into(),
                kind: ArgKind::String,
                doc: "Destination path. Absent = overwrite current file.".into(),
                prompt: "path:".into(),
                default: ArgDefault::None,
                completion: Some("gen:files".into()),
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(apply_quit),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(apply_write_quit),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // `:qa[!]` -- quit the whole editor regardless of pane / tab count.
    // Distinct from `:q` (which closes a pane unless it's the last);
    // both flow through `Effect::QuitEditor`, differing only in
    // `QuitScope`. Reached by name (aliases `qa` / `qall` / `quitall`
    // live in the host alias table) -- no `ExBuiltins` field needed,
    // mirroring `:tabonly`.
    let _quit_all = registry.register_ex_command(
        "ex:quit-all",
        "Quit the editor, closing every pane and tab (`:qa[!]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: true,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(apply_quit_all),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // `:only` -- close every pane except the active one (vim `<C-w>o`,
    // emacs `C-x 1`). A pane op like `:tabonly`, so it emits the same
    // `Effect::AppAction(AppEffect::OnlyPane)` carrier (aliases `only`
    // / `on` live in the host alias table). Reached by name -- no
    // `ExBuiltins` field, mirroring `:tabonly`.
    let _only = registry.register_ex_command(
        "ex:only",
        "Close every pane except the active one (`:only`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::OnlyPane))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // Pane-management ex-commands (vim `:sp` / `:vs` / `:clo`, emacs
    // `C-x 2` / `C-x 3` / `C-x 0`). No-arg today: the split shows the
    // current buffer (an optional `[file]` arg is a future addition).
    // Pane ops like `:only`, emitting the AppEffect carrier; reached by
    // name (aliases in the host alias table), no `ExBuiltins` field.
    let _split = registry.register_ex_command(
        "ex:split",
        "Split the window horizontally (`:sp[lit]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::SplitPaneHorizontal))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _vsplit = registry.register_ex_command(
        "ex:vsplit",
        "Split the window vertically (`:vs[plit]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::SplitPaneVertical))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _close = registry.register_ex_command(
        "ex:close",
        "Close the active pane (`:clo[se]`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::ClosePane))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // org-cycle fold commands. Plain names (the `:diff` pattern — no `ex:`
    // prefix / host alias); `:fold-cycle` resolves directly. Carry the
    // `AppEffect` to the host fold handlers, same as the `z<Space>` /
    // `z<Tab>` keymap chords.
    let _fold_cycle = registry.register_ex_command(
        "fold-cycle",
        "org-cycle: cycle the fold under the cursor through \
         FOLDED → CHILDREN → SUBTREE (`z<Space>`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::CycleFoldAtCursor))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _fold_cycle_global = registry.register_ex_command(
        "fold-cycle-global",
        "org-cycle: cycle the whole buffer through \
         OVERVIEW → CONTENTS → SHOW-ALL (`z<Tab>`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::CycleFoldsGlobal))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _fold_goto_parent = registry.register_ex_command(
        "fold-goto-parent",
        "Move the cursor to the parent heading, one level up the fold \
         hierarchy (`zp`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::GotoParentFold))),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::ClearSearchHighlight)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::EchoRegisters)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::EchoMarks)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::DeleteCurrentLine)),
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(apply_set),
            // Single arg slot tied to the `gen:options` completion
            // generator so `:set <Tab>` enumerates option names and
            // `:set foldmethod=<Tab>` enumerates valid values.
            args_schema: vec![ArgSpec {
                name: "option".into(),
                kind: ArgKind::String,
                doc: "Option name, `name=value`, `name?`, or `noname`.".into(),
                prompt: "option:".into(),
                default: ArgDefault::Required,
                completion: Some("gen:options".into()),
                picker: None,
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(apply_set_local),
            args_schema: vec![ArgSpec {
                name: "option".into(),
                kind: ArgKind::String,
                doc: "Option name, `name=value`, `noname`, or `name&` to clear.".into(),
                prompt: "option:".into(),
                default: ArgDefault::Required,
                completion: Some("gen:options".into()),
                picker: None,
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(apply_set_global),
            args_schema: vec![ArgSpec {
                name: "option".into(),
                kind: ArgKind::String,
                doc: "Option name, `name=value`, `noname`, or `name?` to query global value."
                    .into(),
                prompt: "option:".into(),
                default: ArgDefault::Required,
                completion: Some("gen:options".into()),
                picker: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let colorscheme = registry.register_ex_command(
        "ex:colorscheme",
        "Swap the active theme by name (`:colorscheme <name>`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            // T.12a: no-arg is now legal — it opens the live-preview
            // theme picker host-side. `parse_optional_path` returns
            // `Args::None` on empty input, `Args::String` otherwise.
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(apply_colorscheme),
            args_schema: vec![ArgSpec {
                name: "name".into(),
                kind: ArgKind::String,
                doc: "Theme name (`catppuccin-mocha`, `catppuccin-macchiato`). Omit to open the live-preview picker.".into(),
                prompt: "colorscheme:".into(),
                // T.12a: optional — no-arg opens the picker.
                default: ArgDefault::None,
                // T.12 wires a `gen:colorschemes` completion generator;
                // no name completion yet.
                completion: None,
                picker: None,
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(apply_edit),
            args_schema: vec![ArgSpec {
                name: "path".into(),
                kind: ArgKind::String,
                doc: "File path to open. Absent = reload current file.".into(),
                prompt: "path:".into(),
                default: ArgDefault::None,
                completion: Some("gen:files".into()),
                picker: None,
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
            parse_args: Arc::new(parse_substitute_args_unreachable),
            apply: Arc::new(apply_substitute),
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
                    name: "flags".into(),
                    kind: ArgKind::String,
                    doc: "Flags string (currently `g` honoured; others ignored)".into(),
                    prompt: "".into(),
                    default: ArgDefault::Literal(ArgValue::String(String::new())),
                    completion: None,
                    picker: None,
                },
            ],
            surface_form: SurfaceForm::Delimiter {
                hint: ":s/pattern/replacement/[flags]  (or :%s/.../.../  for whole buffer)"
                    .into(),
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
            parse_args: Arc::new(parse_global_args_unreachable),
            apply: Arc::new(apply_global),
            args_schema: vec![
                ArgSpec::required("pattern", ArgKind::Pattern, "Match pattern (literal in v1)"),
                ArgSpec {
                    name: "inverted".into(),
                    kind: ArgKind::Bool,
                    doc: "True for `:v` form -- match lines NOT matching the pattern.".into(),
                    prompt: "".into(),
                    default: ArgDefault::Literal(ArgValue::Bool(false)),
                    completion: None,
                    picker: None,
                },
                ArgSpec::required(
                    "body",
                    ArgKind::Raw,
                    "Ex-command to run on each matching line (re-parsed per match)",
                ),
            ],
            surface_form: SurfaceForm::Delimiter {
                hint: ":g/pattern/body  (or :v/pattern/body  for inverted)".into(),
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(apply_describe_command),
            args_schema: vec![ArgSpec {
                name: "name".into(),
                kind: ArgKind::String,
                doc: "Registered command name (`ex:write`, `motion:word-forward`, ...)".into(),
                prompt: "command:".into(),
                default: ArgDefault::Required,
                completion: Some("gen:commands".into()),
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::DescribeBuffer)),
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(apply_apropos),
            args_schema: vec![ArgSpec {
                name: "pattern".into(),
                kind: ArgKind::String,
                doc: "Case-insensitive substring matched against name and doc".into(),
                prompt: "apropos:".into(),
                default: ArgDefault::Required,
                completion: None,
                picker: None,
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(apply_describe_key),
            args_schema: vec![ArgSpec {
                name: "chord".into(),
                kind: ArgKind::Chord,
                doc: "Chord notation (`j`, `dw`, `<C-d>`, `gg`, `<Esc>`, ...)".into(),
                prompt: "key:".into(),
                default: ArgDefault::Required,
                completion: None,
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::ListKeymap)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::BufferNext)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::NextTab))),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::PrevTab))),
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(path) if !path.is_empty() => {
                    Ok(Effect::AppAction(AppEffect::NewTabAt(path.clone())))
                }
                _ => Ok(Effect::AppAction(AppEffect::NewTab)),
            }),
            args_schema: vec![ArgSpec {
                name: "path".into(),
                kind: ArgKind::String,
                doc: "Optional file path to open in the new tab".into(),
                prompt: "".into(),
                default: ArgDefault::None,
                completion: Some("gen:files".into()),
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::CloseTab))),
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(s) if !s.is_empty() => {
                    Ok(Effect::AppAction(AppEffect::TerminalSpawn(Some(s.clone()))))
                }
                _ => Ok(Effect::AppAction(AppEffect::TerminalSpawn(None))),
            }),
            args_schema: vec![ArgSpec {
                name: "cmd".into(),
                kind: ArgKind::String,
                doc: "Optional command line (default: $SHELL or /bin/sh)".into(),
                prompt: "".into(),
                default: ArgDefault::None,
                completion: None,
                picker: None,
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(s) if !s.is_empty() => Ok(Effect::AppAction(
                    AppEffect::TerminalSpawnInNewTab(Some(s.clone())),
                )),
                _ => Ok(Effect::AppAction(AppEffect::TerminalSpawnInNewTab(None))),
            }),
            args_schema: vec![ArgSpec {
                name: "cmd".into(),
                kind: ArgKind::String,
                doc: "Optional command line (default: $SHELL or /bin/sh)".into(),
                prompt: "".into(),
                default: ArgDefault::None,
                completion: None,
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::OnlyTab))),
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
            parse_args: Arc::new(parse_tabmove_arg),
            apply: Arc::new(|ctx| {
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::BufferPrev)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let list_buffers = registry.register_ex_command(
        "ex:buffers",
        "List every open document buffer as a static text view (`:ls`). \
         For the fuzzy switcher, see `:buffers` / `:b`.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::ListBuffers)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _buffer_picker = registry.register_ex_command(
        "ex:buffer-picker",
        "Open the vertico-style buffer switcher (`:buffers` / `:b`). \
         Type to filter; `<CR>` to switch. For the static text listing, see `:ls`.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::OpenBufferPicker)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|ctx| Ok(Effect::BufferDelete { force: ctx.bang })),
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
        "Open the project file picker (`:files [root]`). Absent root = the active buffer's project. Alias for `:picker files [root]`.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| {
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
                name: "root".into(),
                kind: ArgKind::String,
                doc: "Directory to walk. Absent = the active buffer's project root.".into(),
                prompt: "root:".into(),
                default: ArgDefault::None,
                completion: Some("gen:files".into()),
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| {
                Ok(Effect::OpenPicker {
                    source: "recent".into(),
                    args: Vec::new(),
                })
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // MB.5: `:history searches` opens the search-line history picker
    // (also reachable via `q/` / `q?`). `:history commands` (the
    // default when no arg is given) opens the command-line history.
    let _history_picker = registry.register_ex_command(
        "ex:history",
        "Open a history picker (`:history [commands|searches|pane-buffers]`). \
         `commands`: command-line history picker (default, also `q:`). \
         `searches`: search-line history picker (`q/` / `q?`). \
         `pane-buffers`: this pane's buffer history (`<C-6>` / `<C-7>`). \
         `<CR>` loads the chosen entry into the `:` / `/` line (does not execute).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_history_args),
            apply: Arc::new(|ctx| {
                let source = if let Args::List(ref values) = ctx.args
                    && !values.is_empty()
                {
                    match values[0].as_str() {
                        Some("searches") => "search-history",
                        // PBH.5: this pane's buffer trail.
                        Some("pane-buffers") => "pane-buffer-history",
                        _ => "history",
                    }
                } else {
                    "history"
                };
                Ok(Effect::OpenPicker {
                    source: source.into(),
                    args: Vec::new(),
                })
            }),
            args_schema: vec![ArgSpec {
                name: "kind".into(),
                kind: ArgKind::String,
                prompt: "history kind (`commands`, `searches`, or `pane-buffers`)".into(),
                default: ArgDefault::None,
                doc: "`commands` (default), `searches`, or `pane-buffers`.".into(),
                completion: Some("gen:history-kinds".into()),
                picker: None,
            }],
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
            parse_args: Arc::new(parse_picker_args),
            apply: Arc::new(|ctx| {
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
                name: "source".into(),
                kind: ArgKind::String,
                doc: "Picker source id (`files`, `recent`, `buffers`, ...).".into(),
                prompt: "source:".into(),
                default: ArgDefault::Required,
                // `gen:picker-sources` walks the App's
                // `picker_registry` (Arc-shared, captured Weakly
                // by the generator) and emits one candidate per
                // registered source id. Adding a new picker source
                // surfaces in `:picker <Tab>` automatically.
                completion: Some("gen:picker-sources".into()),
                picker: None,
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| {
                let root = match &ctx.args {
                    Args::String(p) if !p.is_empty() => Some(std::path::PathBuf::from(p.as_str())),
                    _ => None,
                };
                Ok(Effect::OpenFileTree { root })
            }),
            args_schema: vec![ArgSpec {
                name: "root".into(),
                kind: ArgKind::String,
                doc: "Directory to open as the tree root. Absent = current dir.".into(),
                prompt: "root:".into(),
                default: ArgDefault::None,
                completion: Some("gen:files".into()),
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::CloseFileTree)),
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| {
                let dir = match &ctx.args {
                    Args::String(p) if !p.is_empty() => Some(std::path::PathBuf::from(p.as_str())),
                    _ => None,
                };
                Ok(Effect::OpenOil { dir })
            }),
            args_schema: vec![ArgSpec {
                name: "dir".into(),
                kind: ArgKind::String,
                doc: "Directory to open. Absent = current document's parent.".into(),
                prompt: "dir:".into(),
                default: ArgDefault::None,
                completion: Some("gen:files".into()),
                picker: None,
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(name) => Ok(Effect::DescribeOption {
                    name: name.to_string(),
                }),
                _ => Err(CommandError::BadArgs("expected option name".into())),
            }),
            args_schema: vec![ArgSpec {
                name: "name".into(),
                kind: ArgKind::String,
                doc: "Registered option name (or alias).".into(),
                prompt: "option:".into(),
                default: ArgDefault::Required,
                completion: Some("gen:options".into()),
                picker: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let describe_element = registry.register_ex_command(
        "ex:describe-element",
        "Open the help view for a theme element / face \
         (`:describe-element NAME`, alias `:describe-face NAME`). \
         Shows owner, doc, the authoring (reference-form) style spec \
         (palette keys + inherit parent), and the concrete resolved \
         style under the active theme.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(name) => Ok(Effect::DescribeElement {
                    name: name.to_string(),
                }),
                _ => Err(CommandError::BadArgs("expected element name".into())),
            }),
            args_schema: vec![ArgSpec {
                name: "name".into(),
                kind: ArgKind::String,
                // `gen:elements` is a host generator (the theme registry is a
                // host-side ServiceRegistry service); it's registered in
                // `editor_boot.rs` and walks `ThemeRegistry::element_names`.
                doc: "Registered theme-element name (e.g. `syntax.keyword`, `diff.add.sign`)."
                    .into(),
                prompt: "element:".into(),
                default: ArgDefault::Required,
                completion: Some("gen:elements".into()),
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::ListOptions)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let describe_plugin_api = registry.register_ex_command(
        "ex:describe-plugin-api",
        "Open the help view for the plugin API (`:describe-plugin-api [<seam>]`). \
         With a seam name (`host-services`, `picker-source`, ...) render that \
         interface's functions, direction, and capability; without, list every \
         seam. The catalog is derived from `wit/` at build time.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_optional_string),
            apply: Arc::new(apply_describe_plugin_api),
            args_schema: vec![ArgSpec {
                name: "seam".into(),
                kind: ArgKind::String,
                doc: "Plugin-API interface name (`host-services`, `picker-source`, \
                      `grammar`, ...). Omit to list every seam."
                    .into(),
                prompt: "seam:".into(),
                default: ArgDefault::None,
                // A `gen:plugin-apis` completion generator is a follow-up (the
                // catalog lives host-side in `lattice-plugin-api`, which the
                // grammar crate's generators can't reach -- the describe-element
                // precedent).
                completion: None,
                picker: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let list_plugin_apis = registry.register_ex_command(
        "ex:list-plugin-apis",
        "List every plugin-API interface the `wit/` package exposes \
         (`:list-plugin-apis`). One row per seam with direction + capability + \
         function count.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::ListPluginApis)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let export_plugin_api = registry.register_ex_command(
        "ex:export-plugin-api",
        "Export the whole plugin-API catalog to a savable buffer \
         (`:export-plugin-api [markdown|json]`). Opens `*plugin-api.md*` (or \
         `*plugin-api.json*`) under text-mode; save it with `:w <path>`. \
         Defaults to markdown.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_optional_string),
            apply: Arc::new(apply_export_plugin_api),
            args_schema: vec![ArgSpec {
                name: "format".into(),
                kind: ArgKind::String,
                doc: "`markdown` (default) or `json`.".into(),
                prompt: "format:".into(),
                default: ArgDefault::None,
                completion: None,
                picker: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let list_commands = registry.register_ex_command(
        "ex:list-commands",
        "List every registered command grouped by source (`:list-commands`): \
         built-in, user config, plugin, ... Each row links to its \
         `:describe-command` view.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::ListCommands)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let describe_plugin = registry.register_ex_command(
        "ex:describe-plugin",
        "Open the help view for a loaded plugin (`:describe-plugin <name>`): its \
         own documentation + contributions. (Loaded-plugin enumeration is \
         Phase-8-gated; today no plugins are loaded.)",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(name) => Ok(Effect::DescribePlugin { name: name.clone() }),
                _ => Err(CommandError::BadArgs("expected a plugin name".into())),
            }),
            args_schema: vec![ArgSpec {
                name: "name".into(),
                kind: ArgKind::String,
                doc: "Loaded plugin name (e.g. `git-gutter`).".into(),
                prompt: "plugin:".into(),
                default: ArgDefault::Required,
                // `gen:plugins` completion is a follow-up (the loaded-plugin
                // registry is host-side + empty until Phase-8).
                completion: None,
                picker: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let list_plugins = registry.register_ex_command(
        "ex:list-plugins",
        "List every loaded plugin (`:list-plugins`): name + doc summary. Empty \
         until the Phase-8 plugin loader is wired in.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::ListPlugins)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::DescribeEvents)),
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(name) => Ok(Effect::DescribeEvent {
                    name: name.to_string(),
                }),
                _ => Err(CommandError::BadArgs("expected event name".into())),
            }),
            args_schema: vec![ArgSpec {
                name: "name".into(),
                kind: ArgKind::String,
                doc: "Registered event name (e.g. `lsp.buffer-attached`).".into(),
                prompt: "event:".into(),
                default: ArgDefault::Required,
                completion: Some("gen:events".into()),
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::ListModes)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // `:describe-active-modes`, deliberately NOT `:describe-modes`.
    // The shorter name is a prefix-sibling of `:describe-mode`, which
    // would push `:describe-mode<Tab>` off the single-candidate
    // completion branch and reintroduce the bug
    // `tab_on_complete_command_name_steps_into_the_arg_slot` guards.
    // Name-resolved at the `:` line and by `keymap_help.rs`; no
    // struct field needed (see the CR.6 note on `ExCommands`).
    let _describe_active_modes = registry.register_ex_command(
        "ex:describe-active-modes",
        "Show the mode stack live on the current buffer \
         (`:describe-active-modes`, `<C-h>m`): the major plus every \
         minor, each with the chords it contributes.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::DescribeActiveModes)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // DAM.6: the buffer-scoped peer of `:keymap`. `:keymap` renders
    // the whole static catalog (the exhaustive reference); this one
    // answers "what can I press *here*".
    let _describe_bindings = registry.register_ex_command(
        "ex:describe-bindings",
        "List the chords that can fire on the current buffer \
         (`:describe-bindings`, `<C-h>K`): builtin bindings live in \
         the current binding-mode plus every active mode's \
         contributions. `:keymap` lists the full default catalog.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::DescribeActiveBindings)),
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(name) => Ok(Effect::DescribeMode {
                    name: name.to_string(),
                }),
                _ => Err(CommandError::BadArgs("expected mode name".into())),
            }),
            args_schema: vec![ArgSpec {
                name: "name".into(),
                kind: ArgKind::String,
                doc: "Registered mode name (e.g. `lsp-mode`, `text-mode`).".into(),
                prompt: "mode:".into(),
                default: ArgDefault::Required,
                completion: Some("gen:modes".into()),
                picker: None,
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(name) => Ok(Effect::DescribeOptionResolution {
                    name: name.to_string(),
                }),
                _ => Err(CommandError::BadArgs("expected option name".into())),
            }),
            args_schema: vec![ArgSpec {
                name: "name".into(),
                kind: ArgKind::String,
                doc: "Registered option name (e.g. `number`, `tabstop`).".into(),
                prompt: "option:".into(),
                default: ArgDefault::Required,
                completion: Some("gen:options".into()),
                picker: None,
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| match &ctx.args {
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
                name: "lesson".into(),
                kind: ArgKind::String,
                doc: "Lesson number (default: 1).".into(),
                prompt: "lesson:".into(),
                default: ArgDefault::None,
                completion: None,
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::TutorAdvance))),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::TutorRetreat))),
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::None => Ok(Effect::Customize { name: None }),
                Args::String(name) => Ok(Effect::Customize {
                    name: Some(name.to_string()),
                }),
                _ => Err(CommandError::BadArgs(
                    "expected at most one argument".into(),
                )),
            }),
            args_schema: vec![ArgSpec {
                name: "name".into(),
                kind: ArgKind::String,
                doc: "Group name (e.g. `editor`, `lsp`) OR mode name \
                      ending in `-mode` (e.g. `lsp-completion-mode`)."
                    .into(),
                prompt: "customize:".into(),
                default: ArgDefault::None,
                completion: Some("gen:customize".into()),
                picker: None,
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| {
                let markdown = match &ctx.args {
                    Args::String(s) if !s.is_empty() => s.to_string(),
                    _ => "(empty hover)".to_string(),
                };
                Ok(Effect::OpenHover { markdown })
            }),
            args_schema: vec![ArgSpec {
                name: "markdown".into(),
                kind: ArgKind::String,
                doc: "Markdown body of the hover popup.".into(),
                prompt: "hover:".into(),
                default: ArgDefault::None,
                completion: None,
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::DismissPopup)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_ctx| Ok(Effect::OpenMessages)),
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| {
                let server_id = match &ctx.args {
                    Args::String(s) if !s.is_empty() => Some(s.to_string()),
                    _ => None,
                };
                Ok(Effect::OpenLspLog { server_id })
            }),
            args_schema: vec![ArgSpec {
                name: "server".into(),
                kind: ArgKind::String,
                doc: "Server id (e.g. `rust`, `python`). Absent = subsystem-wide log.".into(),
                prompt: "server:".into(),
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers".into()),
                picker: None,
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(s) if !s.is_empty() => Ok(Effect::ToggleLspTrace {
                    server_id: s.to_string(),
                }),
                _ => Err(CommandError::BadArgs(
                    ":lsp-trace requires a server id".into(),
                )),
            }),
            args_schema: vec![ArgSpec {
                name: "server".into(),
                kind: ArgKind::String,
                doc: "Server id to toggle trace on.".into(),
                prompt: "server:".into(),
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers".into()),
                picker: None,
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| {
                let server_id = match &ctx.args {
                    Args::String(s) if !s.is_empty() => Some(s.to_string()),
                    _ => None,
                };
                Ok(Effect::OpenLspTraceLog { server_id })
            }),
            args_schema: vec![ArgSpec {
                name: "server".into(),
                kind: ArgKind::String,
                doc: "Server id (e.g. `rust`). Absent = picker over every running instance.".into(),
                prompt: "server:".into(),
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers".into()),
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::LspStatus)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // LR.2: the editable references view. One alias, dashed and
    // `lsp-` namespaced. Deliberately no chord: `gR` is vim's Virtual
    // Replace, unimplemented here, so binding it would foreclose a
    // grammar slot rather than collide with one. `<C-q>` from the
    // picker (LR.5) is the discoverable path.
    let _lsp_references_view = registry.register_ex_command(
        "ex:lsp-references",
        "Open every reference to the symbol under the cursor as an editable multibuffer, \
         one excerpt per site. Edits propagate to the source files. `gr` keeps opening the \
         picker, which is the better surface for jumping to a single site.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::Lsp(LspRequest::ReferencesView))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    // EP.6: references into the error list, on demand. A third
    // terminus on the references drain, not a cache snapshot — there is
    // no standing "current references" state to pull from.
    let _lsp_references_to_error_list = registry.register_ex_command(
        "ex:lsp-references-to-error-list",
        "Find references to the symbol under the cursor and put them in the error list \
         (`:next-error` / `]qq` / `:problems`). The manual peer of \
         `lsp.references-to-error-list`, which is off by default.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::Lsp(LspRequest::ReferencesToErrorList))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    // EP.4: one alias, dashed + `lsp-` namespaced, per the ex-command
    // naming rule. No collapsed spelling, no generic `diagnostics`
    // alias -- a generic name would imply it works without LSP.
    let _lsp_diagnostics_to_error_list = registry.register_ex_command(
        "ex:lsp-diagnostics-to-error-list",
        "Pull the language server's currently published diagnostics into the error list \
         (`:problems` / `:next-error` / the picker). The manual peer of \
         `lsp.diagnostics-to-error-list`; surfaces what servers have published, which is \
         not a workspace scan.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::LspDiagnosticsToErrorList)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::LspServerLogListing)),
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(s) if !s.is_empty() => Ok(Effect::LspRestart {
                    server_id: s.to_string(),
                }),
                _ => Err(CommandError::BadArgs(
                    ":lsp-restart requires a server id".into(),
                )),
            }),
            args_schema: vec![ArgSpec {
                name: "server".into(),
                kind: ArgKind::String,
                doc: "Server id to restart.".into(),
                prompt: "server:".into(),
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers".into()),
                picker: None,
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| match &ctx.args {
                Args::String(s) if !s.trim().is_empty() => Ok(Effect::LspProgressCancel {
                    server_id: Some(s.trim().to_string()),
                }),
                _ => Ok(Effect::LspProgressCancel { server_id: None }),
            }),
            args_schema: vec![ArgSpec {
                name: "server".into(),
                kind: ArgKind::String,
                doc: "Optional server id; omit to cancel on every attached server.".into(),
                prompt: "server:".into(),
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers".into()),
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::LspExpandRegion)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::LspShrinkRegion)),
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
            parse_args: Arc::new(parse_required_string),
            apply: Arc::new(|ctx| match &ctx.args {
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
                name: "spec".into(),
                kind: ArgKind::String,
                // Single-token completion lands the level form
                // (`info`, `debug`, ...). The two-token
                // `<server> <level>` form parses correctly at
                // submit; v1 ships completion for the common
                // (subsystem-wide) shape.
                doc: "Either a level (`error`/`warn`/`info`/`debug`/`trace`) for the subsystem default, or `<server> <level>` for a per-server override.".into(),
                prompt: "[server] level:".into(),
                default: ArgDefault::None,
                completion: Some("gen:log-levels".into()),
                picker: None,
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| {
                let server_id = match &ctx.args {
                    Args::String(s) if !s.is_empty() => Some(s.to_string()),
                    _ => None,
                };
                Ok(Effect::LspLogClear { server_id })
            }),
            args_schema: vec![ArgSpec {
                name: "server".into(),
                kind: ArgKind::String,
                doc: "Server id whose ring to clear. Absent = subsystem-wide.".into(),
                prompt: "server:".into(),
                default: ArgDefault::None,
                completion: Some("gen:lsp-servers".into()),
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::LspDocumentSymbol)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // IN.8b: the LSP-INDEPENDENT format command. Cascades on
    // AVAILABILITY, not on failure: if an attached server advertises
    // formatting it is used, otherwise `formatprg` or the built-in
    // table. Failure-fallback would need a "how long do we wait before
    // giving up" answer, and any number there is arbitrary.
    let format = registry.register_ex_command(
        "ex:format",
        "Format the active buffer: LSP if a server advertises formatting, otherwise `formatprg` \
         or the built-in per-language formatter table (`:format`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::Format)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::LspFormat)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::LspFormatRange)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::LspCodeAction)),
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| {
                let new_name = match &ctx.args {
                    Args::String(s) => s.trim().to_string(),
                    _ => String::new(),
                };
                Ok(Effect::LspRename { new_name })
            }),
            args_schema: vec![ArgSpec {
                name: "new-name".into(),
                kind: ArgKind::String,
                doc: "Replacement identifier. Empty -> use the server's prepareRename placeholder.".into(),
                prompt: "new name:".into(),
                default: ArgDefault::None,
                completion: None,
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::LspComplete)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::LspSignatureHelp)),
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| {
                let query = match &ctx.args {
                    Args::String(s) => s.to_string(),
                    _ => String::new(),
                };
                Ok(Effect::LspWorkspaceSymbol { query })
            }),
            args_schema: vec![ArgSpec {
                name: "query".into(),
                kind: ArgKind::String,
                doc: "Server-side substring filter; empty returns the server's idea of \"every workspace symbol\".".into(),
                prompt: "query:".into(),
                default: ArgDefault::None,
                completion: None,
                picker: None,
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_ctx| Ok(Effect::LspIncomingCalls)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_ctx| Ok(Effect::LspOutgoingCalls)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_ctx| Ok(Effect::LspSupertypes)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_ctx| Ok(Effect::LspSubtypes)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_ctx| Ok(Effect::LspMoniker)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_ctx| Ok(Effect::LspCodeLens)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_ctx| Ok(Effect::LspColorPresentation)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::ListDiagnostics)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _list_errors = registry.register_ex_command(
        "error-list",
        "Open the error list in a fuzzy picker (`:error`; vim `:clist`/`:cl`). The flat browse-and-jump surface — complements `:next-error` (step) and `:problems` (the *problems* multibuffer).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::ListErrors)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::NextDiagnostic)),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::PrevDiagnostic)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // CM.2 / CM.7 / naming-2026-07-22: the error navigation family.
    // Readable canonical names (`next-error`, emacs vocabulary) lead;
    // the vim `:c*` spellings are aliases in `lattice-host::excommand`.
    // Each returns `Effect::AppAction(AppEffect::ErrorNav { target })`;
    // the host's `do_error_nav` walks the core error list (echoes
    // `no error list` when empty — no diagnostic fallback). IDs are
    // not stored (resolved by name), mirroring `_tab_move`.
    let _error_next = registry.register_ex_command(
        "next-error",
        "Jump to the next error / location in the error list (wraps; `:next-error`; vim `:cnext`/`:cn`/`]qq`). Echoes `no error list` when empty (diagnostics live on `]d`/`[d` / `:diag-*`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| {
                Ok(Effect::AppAction(AppEffect::ErrorNav {
                    target: crate::app_effect::ErrorTarget::Next,
                }))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _error_prev = registry.register_ex_command(
        "previous-error",
        "Jump to the previous error / location in the error list (wraps; `:previous-error`; vim `:cprev`/`:cp`/`[qq`). Echoes `no error list` when empty (diagnostics live on `]d`/`[d` / `:diag-*`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| {
                Ok(Effect::AppAction(AppEffect::ErrorNav {
                    target: crate::app_effect::ErrorTarget::Prev,
                }))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _error_cc = registry.register_ex_command(
        "error",
        "Jump to error / location N in the error list (1-based; `:error [N]`; vim `:cc [N]`). Bare `:error` re-visits the current entry.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_cc_arg),
            apply: Arc::new(|ctx| {
                let n: Option<usize> = match &ctx.args {
                    Args::String(s) => s.parse::<usize>().ok(),
                    _ => None,
                };
                Ok(Effect::AppAction(AppEffect::ErrorNav {
                    target: crate::app_effect::ErrorTarget::Jump(n),
                }))
            }),
            args_schema: vec![ArgSpec::optional(
                "n",
                ArgKind::Int,
                "Target error entry (1-indexed; default current)",
            )],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _error_first = registry.register_ex_command(
        "first-error",
        "Jump to the first error / location in the error list (`:first-error`; vim `:cfirst`/`:cr`/`[Q`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| {
                Ok(Effect::AppAction(AppEffect::ErrorNav {
                    target: crate::app_effect::ErrorTarget::First,
                }))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _error_last = registry.register_ex_command(
        "last-error",
        "Jump to the last error / location in the error list (`:last-error`; vim `:clast`/`]Q`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| {
                Ok(Effect::AppAction(AppEffect::ErrorNav {
                    target: crate::app_effect::ErrorTarget::Last,
                }))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    // CM.7 (2026-07-22): file-level error traversal — jump to the
    // first entry of the next / previous file. `:next-error-file`/`]qf`
    // (vim `:cnextfile`/`:cnf`) and `:previous-error-file`/`[qf`.
    let _error_next_file = registry.register_ex_command(
        "next-error-file",
        "Jump to the first error / location in the next file (wraps; `:next-error-file`; vim `:cnextfile`/`:cnf`/`]qf`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| {
                Ok(Effect::AppAction(AppEffect::ErrorNav {
                    target: crate::app_effect::ErrorTarget::NextFile,
                }))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let _error_prev_file = registry.register_ex_command(
        "previous-error-file",
        "Jump to the first error / location in the previous file (wraps; `:previous-error-file`; vim `:cprevfile`/`:cpf`/`[qf`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| {
                Ok(Effect::AppAction(AppEffect::ErrorNav {
                    target: crate::app_effect::ErrorTarget::PrevFile,
                }))
            }),
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::ReloadSnippets)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let cd = registry.register_ex_command(
        "ex:cd",
        "Change the working directory (`:cd [path]`). No arg goes to HOME.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| {
                let path = match &ctx.args {
                    Args::String(s) => Some(s.clone()),
                    Args::None => None,
                    _ => return Err(CommandError::BadArgs("unexpected argument".into())),
                };
                Ok(Effect::ChangeDir(path))
            }),
            args_schema: vec![ArgSpec {
                name: "path".into(),
                kind: ArgKind::String,
                doc: "Directory path. Absent = HOME.".into(),
                prompt: "directory:".into(),
                default: ArgDefault::None,
                completion: Some("gen:directories".into()),
                picker: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let pwd = registry.register_ex_command(
        "ex:pwd",
        "Print the current working directory (`:pwd`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::PrintWorkingDir)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let project_root = registry.register_ex_command(
        "ex:project-root",
        "Print the active buffer's project root and the marker that decided it (`:project-root`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::PrintProjectRoot)),
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
            parse_args: Arc::new(parse_optional_path),
            apply: Arc::new(|ctx| {
                let topic = match &ctx.args {
                    Args::String(s) if !s.is_empty() => Some(s.to_string()),
                    _ => None,
                };
                Ok(Effect::OpenHelpTopic { topic })
            }),
            args_schema: vec![ArgSpec {
                name: "topic".into(),
                kind: ArgKind::String,
                doc: "Topic name (`folding`, `buffers`, ...). Absent = index.".into(),
                prompt: "topic:".into(),
                default: ArgDefault::None,
                completion: Some("gen:help-topics".into()),
                picker: None,
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
        colorscheme,
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
        describe_element,
        list_options,
        describe_plugin_api,
        list_plugin_apis,
        export_plugin_api,
        list_commands,
        describe_plugin,
        list_plugins,
        describe_events,
        describe_event,
        // CR.6: diff/hunk ex-commands now registered by lattice_diff::install().
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
        format,
        lsp_format,
        lsp_format_range,
        lsp_signature_help,
        lsp_complete,
        lsp_rename,
        lsp_code_action,
        reload_snippets,
        cd,
        pwd,
        project_root,
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

/// MB.5: parse `:history [commands|search]`. When no arg is given,
/// defaults to `commands` (command-line history). `search` opens the
/// search-line history picker (`q/` / `q?`).
fn parse_history_args(rest: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Ok(Args::None);
    }
    let lower = trimmed.to_lowercase();
    match lower.as_str() {
        // PBH.5: the accepted set lives here AND in the `apply` mapping
        // above; both must list a kind for it to be reachable. Adding it
        // to only one is the failure this arm's test catches.
        "commands" | "searches" | "pane-buffers" => Ok(Args::List(vec![ArgValue::String(lower)])),
        other => Err(CommandError::BadArgs(format!(
            "unknown history kind `{other}`; expected `commands`, `searches`, or `pane-buffers`"
        ))),
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
        CommandError::BadArgs(format!(":tabmove arg must be a non-negative integer: {e}"))
    })?;
    Ok(Args::String(trimmed.to_string()))
}

/// CM.2 (2026-07-22): `:cc [N]` parser. Optional positional
/// 1-based integer; missing → `Args::None` (bare `:cc` re-visits
/// the current entry). Stored as `Args::String` because `Args` has
/// no Int variant; the apply closure re-parses.
fn parse_cc_arg(rest: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Ok(Args::None);
    }
    // Validate it's a positive integer before passing on.
    trimmed
        .parse::<usize>()
        .map_err(|e| CommandError::BadArgs(format!(":cc arg must be a positive integer: {e}")))?;
    Ok(Args::String(trimmed.to_string()))
}

/// PI.2: `:describe-plugin-api [<seam>]` -- an optional seam name. Missing
/// arg -> `Args::None` (render the full list); present -> `Args::String`.
fn parse_optional_string(rest: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        Ok(Args::None)
    } else {
        Ok(Args::String(trimmed.to_string()))
    }
}

fn apply_describe_plugin_api(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    let seam = match &ctx.args {
        Args::String(s) => Some(s.clone()),
        Args::None => None,
        _ => {
            return Err(CommandError::BadArgs(
                "expected an optional seam name".into(),
            ));
        }
    };
    Ok(Effect::DescribePluginApi { seam })
}

fn apply_export_plugin_api(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    let format = match &ctx.args {
        Args::String(s) => {
            let f = s.trim().to_ascii_lowercase();
            // Validate at parse time so a typo echoes a grammar error rather
            // than silently defaulting; the host maps `markdown`/`json`.
            if f != "markdown" && f != "md" && f != "json" {
                return Err(CommandError::BadArgs(format!(
                    "unknown format `{s}` (expected `markdown` or `json`)"
                )));
            }
            Some(f)
        }
        Args::None => None,
        _ => return Err(CommandError::BadArgs("expected an optional format".into())),
    };
    Ok(Effect::ExportPluginApi { format })
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
    // `:q` is pane-scoped: close the active pane when more than one is
    // open; quit the editor only on the last pane. See `QuitScope`.
    Ok(Effect::QuitEditor {
        force: ctx.bang,
        scope: QuitScope::Pane,
    })
}

fn apply_quit_all(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    // `:qa` is editor-scoped: ignore pane / tab count and shut the
    // editor outright (subject to the same dirty guard as `:q` unless
    // forced). Distinct command from `:q`, same `QuitEditor` effect
    // with `scope = All`.
    Ok(Effect::QuitEditor {
        force: ctx.bang,
        scope: QuitScope::All,
    })
}

fn apply_write_quit(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    // The bang on `:wq!` / `:x!` propagates to the quit step (vim's
    // semantics: force the quit even if the save fails). Save itself is
    // never forced -- writing a path you don't have permission for fails
    // visibly. `:wq` is pane-scoped, mirroring `:q`.
    Ok(Effect::Many(vec![
        Effect::SaveBuffer { path: None },
        Effect::QuitEditor {
            force: ctx.bang,
            scope: QuitScope::Pane,
        },
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

fn apply_colorscheme(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    // T.9.b: `:colorscheme <name>` swaps the active theme directly.
    // T.12a: `:colorscheme` with NO name opens the live-preview theme
    // picker host-side. We encode the no-arg case as an EMPTY
    // `SetColorscheme("")` — the host's `Effect::SetColorscheme` arm
    // branches on the empty string and calls `open_picker("colorscheme")`.
    match &ctx.args {
        Args::String(s) if !s.trim().is_empty() => Ok(Effect::SetColorscheme(s.trim().to_string())),
        // Args::None (no-arg) or empty/whitespace string → picker.
        _ => Ok(Effect::SetColorscheme(String::new())),
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
            lattice_core::BufferId(0),
            Position::ZERO,
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
            lattice_core::BufferId(0),
            Position::ZERO,
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
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::QuitEditor { force, scope } => {
                assert!(force);
                assert_eq!(scope, QuitScope::Pane, ":q is pane-scoped");
            }
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
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::QuitEditor { force, scope } => {
                assert!(!force);
                assert_eq!(scope, QuitScope::Pane, ":q is pane-scoped");
            }
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn describe_active_modes_is_registered_and_takes_no_args() {
        // DAM.1: `:describe-active-modes` resolves by name (no
        // `ExCommands` field) and yields `Effect::DescribeActiveModes`
        // with no argument — the whole point is that it reads the
        // active buffer rather than prompting, which is what
        // `<C-h>m` did wrong for two months.
        let (registry, _ex, mut doc) = fixture();
        let id = registry
            .id_by_name("ex:describe-active-modes")
            .expect("ex:describe-active-modes must be registered");
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            CommandInvocation::of(id),
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::DescribeActiveModes), "got {eff:?}");
    }

    #[test]
    fn describe_active_modes_has_an_empty_args_schema() {
        // Guards the interactive-arg-spec path: a non-empty
        // `args_schema` with a Required default is exactly what makes
        // `:describe-mode` prompt. This command must never prompt.
        let (registry, _ex, _doc) = fixture();
        let spec = registry
            .lookup_by_name("ex:describe-active-modes")
            .expect("registered");
        assert!(
            spec.args_schema.is_empty(),
            "an arg schema would re-arm the prompt this command exists to remove: {:?}",
            spec.args_schema,
        );
    }

    #[test]
    fn describe_active_modes_does_not_shadow_describe_mode() {
        // DAM.1 naming lock-in. `:describe-modes` would have been a
        // prefix-sibling of `:describe-mode`, pushing
        // `:describe-mode<Tab>` off the single-candidate completion
        // branch that
        // `tab_on_complete_command_name_steps_into_the_arg_slot`
        // (lattice-ui-tui) exists to guard. Assert no registered
        // ex-command name has `ex:describe-mode` as a strict prefix.
        let (registry, _ex, _doc) = fixture();
        assert!(
            registry.id_by_name("ex:describe-mode").is_some(),
            "ex:describe-mode must still be registered and untouched",
        );
        assert!(
            registry.id_by_name("ex:describe-modes").is_none(),
            "`:describe-modes` must not exist — it would shadow `:describe-mode` completion",
        );
    }

    #[test]
    fn describe_mode_still_requires_its_argument() {
        // DAM.1 is additive: `:describe-mode` keeps the required arg
        // (and therefore the `gen:modes` prompt) so `<C-h>M` can
        // preserve the browse-any-mode path.
        assert!(matches!(
            parse_required_string("", false),
            Err(CommandError::BadArgs(_))
        ));
        let (registry, _ex, _doc) = fixture();
        let spec = registry
            .lookup_by_name("ex:describe-mode")
            .expect("registered");
        assert_eq!(spec.args_schema.len(), 1, "describe-mode keeps its one arg");
    }

    #[test]
    fn quit_all_is_editor_scoped() {
        // `:qa` is a distinct command from `:q` but the same effect:
        // `QuitEditor` with `scope = All`. `:qa!` forces past the dirty
        // guard. Resolved by name (no `ExBuiltins` field, like `:tabonly`).
        let (registry, _ex, mut doc) = fixture();
        let id = registry
            .id_by_name("ex:quit-all")
            .expect("ex:quit-all must be registered");
        for (bang, want_force) in [(false, false), (true, true)] {
            let mut inv = CommandInvocation::of(id);
            if bang {
                inv = inv.with_bang(true);
            }
            let eff = execute(
                &registry,
                &mut doc,
                lattice_core::BufferId(0),
                Position::ZERO,
                inv,
                &CancellationToken::never(),
            )
            .unwrap();
            match eff {
                Effect::QuitEditor { force, scope } => {
                    assert_eq!(force, want_force);
                    assert_eq!(scope, QuitScope::All, ":qa is editor-scoped");
                }
                other => panic!("unexpected effect: {other:?}"),
            }
        }
    }

    #[test]
    fn write_quit_emits_many_save_then_quit() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.write_quit.0).with_bang(true);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::Many(parts) => {
                assert!(matches!(parts[0], Effect::SaveBuffer { .. }));
                assert!(matches!(
                    parts[1],
                    Effect::QuitEditor {
                        force: true,
                        scope: QuitScope::Pane
                    }
                ));
            }
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn pane_split_and_close_commands_emit_app_effects() {
        // `:split` / `:vsplit` / `:close` are pane ops (like `:only`):
        // each emits the matching `AppEffect` carrier. Resolved by name
        // (no `ExBuiltins` field, mirroring `:tabonly`).
        let (registry, _ex, mut doc) = fixture();
        for (name, want) in [
            ("ex:split", AppEffect::SplitPaneHorizontal),
            ("ex:vsplit", AppEffect::SplitPaneVertical),
            ("ex:close", AppEffect::ClosePane),
        ] {
            let id = registry
                .id_by_name(name)
                .unwrap_or_else(|| panic!("{name} must be registered"));
            let eff = execute(
                &registry,
                &mut doc,
                lattice_core::BufferId(0),
                Position::ZERO,
                CommandInvocation::of(id),
                &CancellationToken::never(),
            )
            .unwrap();
            match eff {
                Effect::AppAction(got) => assert_eq!(got, want, "{name}"),
                other => panic!("{name}: unexpected effect: {other:?}"),
            }
        }
    }

    #[test]
    fn nohlsearch_emits_clear_search_highlight() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.no_hlsearch.0);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
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
            lattice_core::BufferId(0),
            Position::ZERO,
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
            lattice_core::BufferId(0),
            Position::ZERO,
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
            lattice_core::BufferId(0),
            Position::ZERO,
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
            lattice_core::BufferId(0),
            Position::ZERO,
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
    fn describe_plugin_api_emits_effect_with_and_without_seam() {
        // With a seam name -> Some.
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.describe_plugin_api.0)
            .with_args(Args::String("host-services".into()));
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::DescribePluginApi { seam } => {
                assert_eq!(seam.as_deref(), Some("host-services"))
            }
            other => panic!("unexpected effect: {other:?}"),
        }
        // No arg -> None (renders the full list).
        let inv = CommandInvocation::of(ex.describe_plugin_api.0);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::DescribePluginApi { seam } => assert!(seam.is_none()),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn list_plugin_apis_emits_effect() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.list_plugin_apis.0);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::ListPluginApis));
    }

    #[test]
    fn export_plugin_api_validates_format_and_emits_effect() {
        let (registry, ex, mut doc) = fixture();
        let run = |registry: &_, doc: &mut _, args: Args| {
            execute(
                registry,
                doc,
                lattice_core::BufferId(0),
                Position::ZERO,
                CommandInvocation::of(ex.export_plugin_api.0).with_args(args),
                &CancellationToken::never(),
            )
        };
        // json -> Some("json"); no arg -> None (markdown default).
        match run(&registry, &mut doc, Args::String("json".into())).unwrap() {
            Effect::ExportPluginApi { format } => assert_eq!(format.as_deref(), Some("json")),
            other => panic!("unexpected: {other:?}"),
        }
        match run(&registry, &mut doc, Args::None).unwrap() {
            Effect::ExportPluginApi { format } => assert!(format.is_none()),
            other => panic!("unexpected: {other:?}"),
        }
        // An unknown format is a parse-time error (not a silent default).
        assert!(run(&registry, &mut doc, Args::String("yaml".into())).is_err());
    }

    #[test]
    fn list_commands_emits_effect() {
        let (registry, ex, mut doc) = fixture();
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            CommandInvocation::of(ex.list_commands.0),
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::ListCommands));
    }

    #[test]
    fn describe_plugin_and_list_plugins_emit_effects() {
        let (registry, ex, mut doc) = fixture();
        let run = |registry: &_, doc: &mut _, id, args: Args| {
            execute(
                registry,
                doc,
                lattice_core::BufferId(0),
                Position::ZERO,
                CommandInvocation::of(id).with_args(args),
                &CancellationToken::never(),
            )
        };
        match run(
            &registry,
            &mut doc,
            ex.describe_plugin.0,
            Args::String("git-gutter".into()),
        )
        .unwrap()
        {
            Effect::DescribePlugin { name } => assert_eq!(name, "git-gutter"),
            other => panic!("unexpected: {other:?}"),
        }
        // A name is required.
        assert!(run(&registry, &mut doc, ex.describe_plugin.0, Args::None).is_err());
        assert!(matches!(
            run(&registry, &mut doc, ex.list_plugins.0, Args::None).unwrap(),
            Effect::ListPlugins
        ));
    }

    /// Regression guard (plugin-introspection tie-together): a plugin command
    /// (PH7.7 `register_plugin_ex_command`) lands in the SAME store
    /// `registry.names()` iterates — the one `gen:commands` completion,
    /// `:list-commands`, `:describe-command`, and `:apropos` all read live —
    /// carrying `Plugin(id)` provenance so `:list-commands` groups it and PI.3
    /// resolves its name. If this breaks, plugin commands silently vanish from
    /// every introspection/completion surface.
    #[test]
    fn plugin_command_lands_in_the_store_read_by_completion_and_introspection() {
        let mut registry = CommandRegistry::new();
        let spec = ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        };
        registry.register_plugin_ex_command(7, "plugin-cmd", "a plugin command", spec);
        // Visible to the live enumeration every introspection/completion consumer walks.
        assert!(
            registry.names().any(|n| n == "plugin-cmd"),
            "plugin command must be in the store `names()` iterates"
        );
        let spec = registry
            .lookup_by_name("plugin-cmd")
            .expect("plugin command is looked up like any other");
        assert!(
            matches!(spec.source.layer, crate::source::SourceLayer::Plugin(7)),
            "plugin command carries Plugin(id) provenance"
        );
        assert_eq!(spec.doc, "a plugin command");
    }

    #[test]
    fn describe_buffer_emits_describe_buffer_effect() {
        let (registry, ex, mut doc) = fixture();
        let inv = CommandInvocation::of(ex.describe_buffer.0);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
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
            lattice_core::BufferId(0),
            Position::ZERO,
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
            lattice_core::BufferId(0),
            Position::ZERO,
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
            lattice_core::BufferId(0),
            Position::ZERO,
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
            lattice_core::BufferId(0),
            Position::ZERO,
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
            lattice_core::BufferId(0),
            Position::ZERO,
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
            assert_eq!(
                spec.args_schema[0].completion.as_deref(),
                Some("gen:options")
            );
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
                spec.args_schema[0].completion.as_deref(),
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
                spec.args_schema[0].completion.as_deref(),
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
            spec.args_schema[0].completion.as_deref(),
            Some("gen:log-levels"),
            "{} should complete against gen:log-levels",
            cmd.name
        );
    }
}
