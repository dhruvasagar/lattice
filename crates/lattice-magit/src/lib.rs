//! Magit — git porcelain as a core plugin.
//!
//! Feature-buffer crate inverted out of `lattice-host`. Owns every
//! magit buffer view's mode, keymap, action handler, and synthetic-
//! buffer provisioning. Installs through the `SubsystemBoot` seam —
//! one line in `editor_boot.rs`, zero `Editor::do_magit_*` methods.
//!
//! See [`docs/dev/architecture/magit.md`] and
//! [`docs/dev/operations/slice-plans/magit.md`].

pub mod actions;
pub mod magit_blame_mode;
pub mod magit_branch_mode;
pub mod magit_commit_mode;
pub mod magit_core_mode;
pub mod magit_diff_mode;
pub mod magit_log_mode;
pub mod magit_rebase_mode;
pub mod magit_stash_mode;
pub mod magit_status_mode;
pub mod refresh;
pub mod sections;

use std::sync::Arc;

use lattice_grammar::{
    Args, ExCommandSpec, LatencyClass, SurfaceForm,
    registry::CommandRegistry,
};
use lattice_grammar::Effect;
use lattice_mode::{ModeRegistry, SubsystemBoot};

use magit_blame_mode::MagitBlameMode;
use magit_branch_mode::MagitBranchMode;
use magit_commit_mode::MagitCommitMode;
use magit_core_mode::MagitCoreMode;
use magit_diff_mode::MagitDiffMode;
use magit_log_mode::MagitLogMode;
use magit_rebase_mode::MagitRebaseMode;
use magit_stash_mode::MagitStashMode;
use magit_status_mode::MagitStatusMode;

/// Register all magit modes, commands, and keymaps via the generic
/// `SubsystemBoot` seam. Called once from `editor_boot.rs` during
/// the Phase-B subsystem install pass.
pub fn install(boot: &mut impl SubsystemBoot) {
    // ── Modes ──────────────────────────────────────────────

    boot.modes_mut()
        .register(MagitCoreMode)
        .expect("magit-core-mode registers without conflict");

    boot.modes_mut()
        .register(MagitStatusMode)
        .expect("magit-status-mode registers without conflict");

    boot.modes_mut()
        .register(MagitCommitMode)
        .expect("magit-commit-mode registers without conflict");

    boot.modes_mut()
        .register(MagitDiffMode)
        .expect("magit-diff-mode registers without conflict");

    boot.modes_mut()
        .register(MagitLogMode)
        .expect("magit-log-mode registers without conflict");

    boot.modes_mut()
        .register(MagitBlameMode)
        .expect("magit-blame-mode registers without conflict");

    boot.modes_mut()
        .register(MagitStashMode)
        .expect("magit-stash-mode registers without conflict");

    boot.modes_mut()
        .register(MagitBranchMode)
        .expect("magit-branch-mode registers without conflict");

    boot.modes_mut()
        .register(MagitRebaseMode)
        .expect("magit-rebase-mode registers without conflict");

    // ── Ex-commands ────────────────────────────────────────

    register_ex_commands(boot.commands_mut());
}

/// Register all magit ex-commands in the command registry.
fn register_ex_commands(registry: &mut CommandRegistry) {
    let mut mk = |name: &'static str, doc: &'static str, buffer_name: &'static str, mode_id: &'static str| {
        let mode_id = mode_id.to_string();
        registry.register_ex_command(
            name,
            doc,
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|_line: &str, _bang: bool| Ok(Args::None)),
                apply: Arc::new(move |_ctx| {
                    Ok(Effect::OpenSyntheticBuffer {
                        name: buffer_name.to_string(),
                        mode_id: mode_id.clone(),
                    })
                }),
                args_schema: Vec::new(),
                surface_form: SurfaceForm::Keyword,
            },
        );
    };

    mk("magit-status", "Open the Magit status buffer for the current git repository.", "*magit:status*", "magit-status-mode");
    mk("magit-commit", "Open the Magit commit buffer with staged diff preview.", "*magit:commit*", "magit-commit-mode");
    mk("magit-diff", "Open a dedicated side-by-side diff view.", "*magit:diff*", "magit-diff-mode");
    mk("magit-log", "Open the Magit commit history log.", "*magit:log*", "magit-log-mode");
    mk("magit-blame", "Open git blame annotations for the current file.", "*magit:blame*", "magit-blame-mode");
    mk("magit-stash-list", "Open the Magit stash list buffer.", "*magit:stash*", "magit-stash-mode");
    mk("magit-branch", "Open the Magit branch list buffer.", "*magit:branch*", "magit-branch-mode");
    mk("magit-rebase", "Start interactive rebase.", "*magit:rebase*", "magit-rebase-mode");
}
