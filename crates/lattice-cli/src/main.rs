//! `lattice` -- the editor binary.
//!
//! Phase 2: opens a single file, renders it in a terminal viewer with cursor
//! motion. Subsequent phases wire the modal engine, tree-sitter, LSP, and
//! ultimately the GPU UI.
//!
//! ## Why `#[tokio::main]`
//!
//! Slice C.1: `main` is async so a tokio runtime is current from the
//! very first instruction of program execution.
//! `tokio::runtime::Handle::try_current()` succeeds anywhere
//! downstream -- including in `App::new` which constructs
//! `SyntaxHandle`s and (separately) the LSP supervisor handle.
//!
//! Pre-C.1, `main` was synchronous and the runtime didn't exist
//! yet at construction time, so `try_current()` silently failed.
//! Two specific subsystems noticed: LSP papered over with
//! explicit-handle plumbing
//! (`lattice_ui_tui::runtime::lsp_runtime()` + `LspSupervisor::spawn(handle)`);
//! syntax did *not* paper over and the worker was silently
//! never-spawned for the entire lifetime of Option B's
//! incremental-reparse pipeline -- producing the user-visible
//! "highlighting stuck to byte positions" symptom regardless of
//! how correct the algorithm was. C.1 makes that class of bug
//! impossible by ensuring a runtime is always current from start.
//!
//! `lattice_ui_tui::run` stays sync and is called directly from
//! the async main body. It blocks the executor thread for its
//! lifetime; spawned tasks (LSP, syntax, document actor) run on
//! other workers (the multi-thread runtime has `num_cpus` worker
//! threads). `block_on` calls inside the editor route through
//! `lattice_runtime::block_on`, which handles nested-runtime
//! contexts via `block_in_place` (slice C.1.a).

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use lattice_core::Document;

mod scaffold;

#[derive(Debug, Parser)]
#[command(version, about = "lattice editor", long_about = None)]
struct Cli {
    /// Path to the file to open. If omitted, an empty buffer is opened.
    file: Option<PathBuf>,

    /// Route to the GPUI renderer (requires the `gui` build feature).
    /// Default until phase 5.last lands; from then on `--gui` becomes
    /// the default and `--tui` opts in to the terminal renderer. Mutually
    /// exclusive with `--tui`.
    #[arg(long, conflicts_with = "tui")]
    gui: bool,

    /// Route to the TUI renderer (terminal). Default until phase 5.last.
    /// Mutually exclusive with `--gui`.
    #[arg(long, conflicts_with = "gui")]
    tui: bool,

    /// Increase log verbosity. Default is `info`; `-v` bumps to
    /// `debug`, `-vv` to `trace`. Mutually-additive with
    /// `--quiet` / `--log-level` (later wins).
    ///
    /// Logs land in BOTH the in-editor `*messages*` buffer
    /// (open with `:messages`) and stderr. Adjust live with
    /// `:set messages.filter=<level>`.
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    verbose: u8,

    /// Decrease log verbosity. `-q` drops to `warn`, `-qq` to
    /// `error`. Mutually-additive with `--verbose` / `--log-level`.
    #[arg(short = 'q', long = "quiet", action = clap::ArgAction::Count)]
    quiet: u8,

    /// Explicit log level override. Accepts `error` / `warn` /
    /// `info` / `debug` / `trace`, or a full
    /// `tracing-subscriber::EnvFilter` directive (e.g.
    /// `lattice_lsp=debug,lattice_host=info`). Wins over
    /// `-v` / `-q` when both are passed.
    #[arg(long = "log-level", value_name = "LEVEL")]
    log_level: Option<String>,

    /// Issue #36 (2026-05-22): force-enable stderr tracing
    /// output even in TUI mode. By default the TUI peer
    /// disables stderr writes because stderr IS the terminal
    /// ratatui paints into — every `tracing::*` event would
    /// blit a stray line over the screen. This flag is for
    /// users who run TUI with `lattice 2>tracing.log` to
    /// capture events to a file. The GPUI peer always has
    /// stderr enabled (its stderr is a separate stream).
    ///
    /// In any case the `*messages*` buffer ALWAYS captures
    /// events; this flag only controls the EXTRA stderr
    /// write.
    #[arg(long = "stderr-logs")]
    stderr_logs: bool,

    /// Open the interactive Lattice tutor at lesson N (1–5).
    /// Omitting N defaults to lesson 1. Mutually exclusive with FILE.
    #[arg(
        long = "tutor",
        value_name = "N",
        default_missing_value = "1",
        num_args = 0..=1,
        conflicts_with = "file"
    )]
    tutor: Option<u32>,

    /// Scaffold a starter `init.rs` config into `~/.config/lattice/init/` and
    /// exit (does not open the editor). Writes a buildable WASM-component config
    /// crate — `Cargo.toml`, `plugin.toml`, `src/lib.rs`, and a `wit/` copy of
    /// the editor's API. Refuses to overwrite an existing non-empty config.
    #[arg(long = "scaffold-init")]
    scaffold_init: bool,

    /// Scaffold a starter plugin project named NAME into
    /// `~/.config/lattice/plugins/NAME/` and exit. Writes a buildable
    /// WASM-component plugin (a grammar action + a minor mode that binds a key to
    /// it + the `NAME.enabled` gate) with a `wit/` copy of the editor's API.
    /// NAME must be lowercase kebab-case. Refuses to overwrite an existing dir.
    #[arg(long = "scaffold-plugin", value_name = "NAME")]
    scaffold_plugin: Option<String>,
}

// WT.1: lattice's `wit/` API package now lives in `lattice-wit`, which is the
// crate a PLUGIN depends on too. This binary re-exports it under the old name
// so the scaffold keeps one source of truth with every plugin's build rather
// than a second embedding that could drift from it.
pub use lattice_wit::FILES as WIT_FILES;

/// Resolve the final log-level directive from CLI flags +
/// env override. Precedence (last wins):
///   1. Default: `info`.
///   2. `-v` / `-q` counts shift relative to info.
///   3. `--log-level=...` overrides absolutely.
///   4. Pre-existing `LATTICE_LOG` env var trumps flags so
///      `LATTICE_LOG=trace lattice file.txt` works without
///      remembering the CLI flag name.
fn compute_log_level(cli: &Cli) -> String {
    // 4: caller-supplied env wins outright.
    if let Ok(env) = std::env::var("LATTICE_LOG")
        && !env.is_empty()
    {
        return env;
    }
    // 3: explicit --log-level overrides verbose/quiet.
    if let Some(spec) = cli.log_level.as_deref() {
        return spec.to_string();
    }
    // 2: verbose/quiet count shifts. Levels in ascending order:
    //    error < warn < info < debug < trace
    //    info index = 2; -v bumps +1, -q drops -1.
    let levels = ["error", "warn", "info", "debug", "trace"];
    let info_idx: i32 = 2;
    let shift = i32::from(cli.verbose) - i32::from(cli.quiet);
    let idx = (info_idx + shift).clamp(0, (levels.len() - 1) as i32) as usize;
    levels[idx].to_string()
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // `--scaffold-*`: write a starter project and exit — never opens the editor.
    if cli.scaffold_init {
        return scaffold::scaffold_init();
    }
    if let Some(name) = cli.scaffold_plugin.as_deref() {
        return scaffold::scaffold_plugin(name);
    }

    // 2026-05-22 messages-overhaul: removed the prior
    // `tracing_subscriber::fmt().try_init()` install — it won the
    // global subscriber race and silenced `*messages*`. The
    // composed fmt+MessagesLayer subscriber installed by
    // `install_messages_subscriber` (in editor_boot) now provides
    // BOTH the stderr stream AND the in-editor `*messages*`
    // surface from a single subscriber.
    //
    // CLI computes the boot-time log level from flags + env and
    // hands it to the runtime via `set_boot_log_level`; editor_boot
    // calls `boot_log_level()` inside the install path. Using a
    // `OnceLock<String>` setter avoids `std::env::set_var` (denied
    // by `#![deny(unsafe_code)]` and unsafe-flagged in recent
    // Rust).
    let level = compute_log_level(&cli);
    lattice_runtime::set_boot_log_level(level);

    // Bundle-context detection: when the binary lives inside a macOS
    // `.app` bundle (`…/Contents/MacOS/lattice`), `open Lattice.app`
    // provides no CLI flags. Defaulting to TUI in that context renders
    // nothing — there is no controlling terminal. Auto-select GUI
    // whenever we detect a bundle, unless `--tui` was given explicitly.
    //
    // The check is macOS-only and feature-gated on `gui` so the TUI-
    // only build path is unaffected. `current_exe` can fail in rare
    // sandboxed environments; the `unwrap_or(false)` falls back to the
    // normal TUI default in that case.
    #[cfg(all(target_os = "macos", feature = "gui"))]
    let running_in_bundle = std::env::current_exe()
        .ok()
        .map(|p| p.components().any(|c| c.as_os_str() == "Contents"))
        .unwrap_or(false);
    #[cfg(not(all(target_os = "macos", feature = "gui")))]
    let running_in_bundle = false;

    // Issue #36 (2026-05-22): gate the fmt-to-stderr layer.
    // TUI's stderr is the editor terminal — every event
    // would blit a stray line over ratatui's paint. Defaults
    // OFF for TUI, ON for GPUI. `--stderr-logs` forces ON
    // (e.g. `lattice --tui --stderr-logs 2>tracing.log`).
    // 2026-06-29: `--stderr-logs` must NOT write to the TUI's OWN terminal.
    // In the TUI stderr IS the alternate-screen tty, so the fmt layer would
    // blit stray log lines over ratatui's paint — corruption ratatui's diff
    // can't repair (it persists until a force redraw), and each line's
    // newline scrolls the screen, leaving fragments of the previous frame
    // behind. Honor `--stderr-logs` only when stderr is REDIRECTED
    // (`!is_terminal()`). The GUI's stderr is separate from its window, so it
    // stays unconditionally enabled there.
    let use_gui = cli.gui || (running_in_bundle && !cli.tui);
    let stderr_is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let stderr_enabled = use_gui || (cli.stderr_logs && !stderr_is_tty);
    lattice_runtime::set_boot_stderr_enabled(stderr_enabled);
    if cli.stderr_logs && stderr_is_tty && !use_gui {
        // Note BEFORE the alt-screen is entered (stderr is still the normal
        // terminal here, so this is safe). `--stderr-logs` is being ignored
        // because writing logs to the TUI's own terminal would corrupt it.
        eprintln!(
            "lattice: --stderr-logs ignored (stderr is the terminal — would corrupt the TUI). \
             Redirect it, e.g. `lattice --stderr-logs 2>lattice.log`."
        );
    }

    let document = match cli.file {
        Some(path) => {
            Document::open(&path).with_context(|| format!("opening {}", path.display()))?
        }
        // `--tutor` conflicts_with = "file" so cli.file is None here too;
        // do_tutor creates and opens the lesson buffer itself.
        None => Document::empty(),
    };

    // Phase 5.9 / 5.8.M: single-binary entry. `--gui` routes to the
    // GPUI peer (feature-gated by `gui`). `--tui` (or no flag) routes
    // to the TUI peer. After phase 5.last achieves full feature parity
    // the default flips to GUI; until then TUI stays the default for
    // direct invocations; `open Lattice.app` auto-selects GUI via the
    // bundle-context guard above. `clap`'s `conflicts_with` enforces
    // mutual exclusivity at parse time.
    // Plugin auto-discovery is opt-IN, and this is the opt. It sits here
    // rather than inside `Editor::boot` because *this* is what distinguishes a
    // user running the editor from the 45 test files that boot the same
    // `Editor`: those must never pick up whatever is installed in the
    // developer's `~/.config/lattice`, and making them each remember to say so
    // is a thing the next one forgets (see `enable_autoload`).
    //
    // Before both branches, so the TUI and GPUI peers cannot disagree about
    // whether the user's plugins load.
    lattice_plugin_loader::enable_autoload();

    if use_gui {
        run_gui(document)
    } else {
        lattice_ui_tui::run(document, cli.tutor)
    }
}

/// Route to the GPUI peer. Feature-gated: the `gui` Cargo feature
/// pulls in `lattice-ui-gpui` (with its `window` feature) and
/// exposes the real entry. Without the feature, `--gui` produces a
/// helpful error so users know to rebuild with `--features gui`.
#[cfg(feature = "gui")]
fn run_gui(document: Document) -> Result<()> {
    lattice_ui_gpui::run(document)
}

#[cfg(not(feature = "gui"))]
fn run_gui(_document: Document) -> Result<()> {
    anyhow::bail!(
        "GUI renderer not compiled in. Rebuild with `cargo build --features gui` \
         (Linux: requires `libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev`)."
    )
}
