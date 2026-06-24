//! BC.6 / DX.7 / CR.6: the crate-owned `install(boot)` entry point.
//!
//! The diff subsystem registers its own **modes + commands** through the
//! generic [`SubsystemBoot`] surface, collapsing the host wiring into one
//! Phase-B line (`lattice_diff::install(&mut boot)`) — the terminal /
//! claude-code / multibuffer shape.
//!
//! **CR.6 (2026-06-24): the diff subsystem registers its own commands.**
//! Every diff action (`action:diff-*`, `action:hunk-*`) and ex-command
//! (`ex:diff*` / `ex:hunk-*` / `ex:describe-diff`) is declared here via
//! `boot.commands_mut()` — the "modes register commands" pattern
//! (multibuffer precedent). The command *declarations* are mode-owned; the
//! command *bodies* split two ways by what they need:
//!
//! - **mode-owned bodies** — the `do`/`dp`, conflict (`d2o`…`dB`), and
//!   hunk-nav (`]c`/`[c`) chords resolve through the modes'
//!   `action_handlers()` (the `ActionHandlerRegistry` is consulted before
//!   the CommandSpec), which read the `DiffSubsystemHandle` service and
//!   return an `Effect`. The action specs here are pure shells.
//! - **host `Effect` appliers** — the ex-command apply closures parse args
//!   and return a host-boundary `Effect` (`DiffOpen`, `Diffsplit`,
//!   `DiffGetCmd`, …) that the host's `handle_effect` applies. The lifecycle
//!   appliers (`do_diff_open`/`diffsplit`/`off`/`accept`/`reject`/`diffthis`)
//!   stay host-side: they mutate `&mut Editor` (pane tree, document actor,
//!   task spawning), which `lattice-diff` cannot reach without a dependency
//!   cycle. This is the Effect-vocabulary-is-the-host-boundary rule.
//!
//! The commands register under their plain user-facing names (`diff`,
//! `diffsplit`, `hunk-next`, …) — no `ex:` prefix and no host alias shim
//! (the multibuffer pattern); `:diff` resolves to them directly.
//!
//! Two diff touch-points still stay host-side, and are *not* mode-ownership
//! violations (see `docs/dev/architecture/diff-extraction.md`, couplings
//! C6/C10):
//!
//! - **The `DiffSubsystem` lifecycle** — `bind` with the host's
//!   `BufferRegistryDocumentResolver`, the `diff_subsystem` /
//!   `diff_subscription_guard` / `diff_forwarders` Editor fields, and the
//!   `apply_pending_diff_mode_changes` dispatch-tail drain — is host
//!   actor-loop state (terminal-invocation-runner category). The subsystem
//!   handle is published as a service (`DiffSubsystemHandle`) so the modes
//!   reach it generically.
//! - **The `+N ~M` modeline element** is registered against the host's
//!   `ModelineService`, created *after* the Phase-B install list (boot
//!   ordering). The descriptor + `diff_content` formatter are mode-owned (in
//!   [`crate::mode`]); only the registration *call* is host-sequenced.

use lattice_grammar::AppEffect;
use lattice_grammar::CommandRegistry;
use lattice_grammar::args::{ArgDefault, ArgKind, ArgSpec, Args};
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::Effect;
use lattice_grammar::error::{CommandError, GrammarResult};
use lattice_grammar::registry::{ActionSpec, ExCommandContext, ExCommandSpec, SurfaceForm};
use lattice_mode::SubsystemBoot;

use crate::mode::register_diff_modes;

/// Wire the diff subsystem's modes + commands into the editor at boot.
pub fn install(boot: &mut impl SubsystemBoot) {
    // D.5.a / DX.8: register `diff-mode` + `diff-conflict-mode`. Each mode's
    // `on_activate` registers per-buffer render providers (DX.3-C7); their
    // `keymap()` + `action_handlers()` contribute the chord surface, picked
    // up by the host's generic K.2.4 + `register_mode_action_handlers` walks.
    register_diff_modes(boot.modes_mut());
    // CR.6: the diff subsystem registers its own commands (multibuffer
    // pattern). Declarations are mode-owned; bodies are mode `action_handlers`
    // (chords) or host `Effect` appliers (lifecycle ex-commands).
    register_diff_commands(boot.commands_mut());
}

/// CR.6: register every diff action + ex-command against the boot
/// `CommandRegistry`.
fn register_diff_commands(registry: &mut CommandRegistry) {
    register_diff_actions(registry);
    register_diff_ex_commands(registry);
}

/// The `action:*` commands the diff chords resolve. All are mode-owned: the
/// real bodies live in `DiffMode`/`DiffConflictMode::action_handlers()`,
/// consulted before these CommandSpecs in dispatch. `diff-get`/`diff-put`
/// keep their `AppEffect` fallback (the host arm is emptied to a no-op since
/// CR.1); the conflict + hunk-nav actions fall back to `Effect::None`.
fn register_diff_actions(registry: &mut CommandRegistry) {
    registry.register_action(
        "action:diff-get",
        "diff-mode `do`: rewrite the current side's hunk to match the baseline.",
        ActionSpec {
            apply: Box::new(|_| Ok(Effect::AppAction(AppEffect::DiffGet))),
            args_schema: vec![],
        },
    );
    registry.register_action(
        "action:diff-put",
        "diff-mode `dp`: push the current side's hunk into the peer buffer.",
        ActionSpec {
            apply: Box::new(|_| Ok(Effect::AppAction(AppEffect::DiffPut))),
            args_schema: vec![],
        },
    );
    for (name, doc) in [
        ("action:diff-keep-ours", "diff-conflict `d2o`: keep the local (ours) side."),
        (
            "action:diff-keep-theirs",
            "diff-conflict `d3o`: take the remote (theirs) side into local.",
        ),
        ("action:diff-put-ours", "diff-conflict `d2p`: put local into the ours side."),
        (
            "action:diff-put-theirs",
            "diff-conflict `d3p`: put local into the remote (theirs) side.",
        ),
        ("action:diff-keep-both", "diff-conflict `dB`: keep both — splice ours then theirs."),
        ("action:hunk-next", "diff `]c`: jump the cursor to the next hunk start (wraps)."),
        ("action:hunk-prev", "diff `[c`: jump the cursor to the previous hunk start (wraps)."),
    ] {
        registry.register_action(
            name,
            doc,
            ActionSpec {
                apply: Box::new(|_| Ok(Effect::None)),
                args_schema: vec![],
            },
        );
    }
}

/// The `:diff*` / `:hunk-*` ex-commands, registered under their plain
/// user-facing names (no `ex:` prefix / host alias — multibuffer pattern).
/// The apply closures parse args + return a host-boundary `Effect` the host
/// applies.
fn register_diff_ex_commands(registry: &mut CommandRegistry) {
    registry.register_ex_command(
        "diff",
        "Open an inline diff session for the active document against its on-disk content (`:diff`).",
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
    registry.register_ex_command(
        "diffoff",
        "Close the active pane's diff session, if any (`:diffoff[!]`).",
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
    registry.register_ex_command(
        "diffthis",
        "Stage the active pane for a two-pane diff; the second `:diffthis` in a different pane \
         completes the session. Same pane twice unstages.",
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
    registry.register_ex_command(
        "diffsplit",
        "Open `<base>` (and optionally `<remote>`) in new vertical splits and register a diff \
         session. One arg ⇒ two-way; two args ⇒ three-way merge (`:diffsplit <base> [<remote>]`).",
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
                    doc: "File path. Two-way: the baseline; three-way: the common ancestor.",
                    prompt: "base path:",
                    default: ArgDefault::Required,
                    completion: Some("gen:files"),
                },
                ArgSpec {
                    name: "remote",
                    kind: ArgKind::String,
                    doc: "Optional second file path → three-way merge (current pane = local).",
                    prompt: "remote path:",
                    default: ArgDefault::None,
                    completion: Some("gen:files"),
                },
            ],
            surface_form: SurfaceForm::Keyword,
        },
    );
    registry.register_ex_command(
        "diffget",
        "Pull the hunk under the cursor from another buffer's side into the active buffer \
         (`:diffget [<bufnr>]`). `<bufnr>` is required for three-way.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_bufnr),
            apply: Box::new(apply_diffget),
            args_schema: vec![ArgSpec {
                name: "bufnr",
                kind: ArgKind::String,
                doc: "Optional buffer number to pull from (required in three-way merge).",
                prompt: "bufnr:",
                default: ArgDefault::None,
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    registry.register_ex_command(
        "diffput",
        "Push the hunk under the cursor from the active buffer into another buffer's side \
         (`:diffput [<bufnr>]`). `<bufnr>` is required for three-way.",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_bufnr),
            apply: Box::new(apply_diffput),
            args_schema: vec![ArgSpec {
                name: "bufnr",
                kind: ArgKind::String,
                doc: "Optional buffer number to push to (required in three-way merge).",
                prompt: "bufnr:",
                default: ArgDefault::None,
                completion: None,
            }],
            surface_form: SurfaceForm::Keyword,
        },
    );
    registry.register_ex_command(
        "diff-accept",
        "Resolve the active pane's diff session with Accept, tear it down, and fire the Accept \
         signal on any bound completion channel (`:diff-accept`).",
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
    registry.register_ex_command(
        "diff-reject",
        "Resolve the active pane's diff session with Reject, tear it down, and fire the Reject \
         signal on any bound completion channel (`:diff-reject`).",
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
    registry.register_ex_command(
        "hunk-next",
        "Jump cursor to the start of the next diff hunk on the current side. Wraps to top. \
         `]c` / `:hunk-next`.",
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
    registry.register_ex_command(
        "hunk-prev",
        "Jump cursor to the start of the previous diff hunk on the current side. Wraps to \
         bottom. `[c` / `:hunk-prev`.",
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
    registry.register_ex_command(
        "describe-diff",
        "List every active diff session (`:describe-diff`): BufferId, algorithm, revision, hunk \
         count, watched buffers.",
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
}

// ── parse / apply helpers (relocated from lattice-grammar's ex_commands.rs) ──

/// Reject any trailing characters; the command takes no args.
fn parse_no_args(rest: &str, _bang: bool) -> GrammarResult<Args> {
    if rest.trim().is_empty() {
        Ok(Args::None)
    } else {
        Err(CommandError::BadArgs("trailing characters after command".into()))
    }
}

/// 1-or-2-path parser for `:diffsplit <base> [<remote>]`. Joins the two
/// paths with `\x1f` inside `Args::String` (no `Vec<String>` variant in v1);
/// `apply_diffsplit` splits them back. Empty first arg errors.
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
            ":diffsplit takes at most two paths (`:diffsplit <base> [<remote>]`)".into(),
        ));
    }
    let encoded = match remote {
        Some(r) => format!("{base}\x1f{r}"),
        None => base.to_string(),
    };
    Ok(Args::String(encoded))
}

/// Optional `<bufnr>` parser for `:diffget`/`:diffput`. Empty ⇒ `Args::None`;
/// non-empty ⇒ `Args::String(bufnr)` after validating it's a non-negative u32.
fn parse_optional_bufnr(rest: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Ok(Args::None);
    }
    trimmed
        .parse::<u32>()
        .map_err(|e| CommandError::BadArgs(format!("bufnr must be a non-negative integer: {e}")))?;
    Ok(Args::String(trimmed.to_string()))
}

/// `:diffget [<bufnr>]` → `Effect::DiffGetCmd { target }`.
fn apply_diffget(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    Ok(Effect::DiffGetCmd {
        target: parse_bufnr_arg(ctx, "diffget")?,
    })
}

/// `:diffput [<bufnr>]` → `Effect::DiffPutCmd { target }`.
fn apply_diffput(ctx: &ExCommandContext) -> GrammarResult<Effect> {
    Ok(Effect::DiffPutCmd {
        target: parse_bufnr_arg(ctx, "diffput")?,
    })
}

/// Shared `Args → Option<bufnr>` decode for `:diffget`/`:diffput`.
fn parse_bufnr_arg(ctx: &ExCommandContext, cmd: &str) -> GrammarResult<Option<u32>> {
    match &ctx.args {
        Args::None => Ok(None),
        Args::String(s) => Ok(Some(
            s.parse::<u32>().map_err(|e| CommandError::BadArgs(format!("bufnr: {e}")))?,
        )),
        _ => Err(CommandError::BadArgs(format!(
            "expected optional bufnr (`:{cmd} [<bufnr>]`)"
        ))),
    }
}

/// `:diffsplit <base> [<remote>]` → `Effect::Diffsplit { path, remote }`,
/// decoding the parser's `\x1f`-joined paths.
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
