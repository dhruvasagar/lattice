//! `version-mode` — the `*version*` buffer `:version` opens: what this build
//! is, what it is running on, and where it keeps its files.
//!
//! The page is written for a bug report. It is markdown so that `Y` copies it
//! straight into an issue, and so that `lattice --version --verbose` — which
//! prints the same [`VersionReport::render`] — reads the same in a terminal
//! when the editor will not start.
//!
//! ## Who knows what
//!
//! Almost nothing on the page is known to this crate. The build stamp is
//! compiled into the `lattice` binary (its `build.rs`), the renderer is chosen
//! in its `main`, and the paths come from crates `lattice-mode` does not
//! depend on. So the binary assembles a [`VersionReport`] at startup and hands
//! it over with [`set_version_report`], before the editor boots — the
//! `set_boot_log_level` precedent for launch facts the editor reads but does
//! not own. The mode owns the rest of the surface: the buffer, `:version`, the
//! `Y` chord and its handler.
//!
//! A host that never calls the setter (a test harness, an embedder) still gets
//! a page — the version alone, and a line saying the rest was not supplied.

use std::sync::{Arc, OnceLock};

use lattice_grammar::registry::ActionSpec;
use lattice_grammar::{
    Args, EchoLevel, Effect, ExCommandSpec, LatencyClass, Register, SurfaceForm, YankKind,
};

use crate::{
    ActionContext, ActionHandlerContribution, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture,
    Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet, SubsystemBoot, keymap_entry,
};

/// The major the page opens under. Named rather than imported: the markdown
/// mode lives in `lattice-syntax`, which this crate does not depend on.
const MARKDOWN_MODE_ID: &str = "markdown-mode";

/// The buffer `:version` opens.
pub const VERSION_BUFFER_NAME: &str = "*version*";

/// `Y` — copy the whole page to the system clipboard.
pub const COPY_ACTION: &str = "action:version-copy";

/// One titled block of the page: `## Build`, then its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSection {
    /// The section's title, `Build`.
    pub heading: String,
    /// `(label, value)`, in display order.
    pub rows: Vec<(String, String)>,
}

/// Everything the page shows, assembled by the binary at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionReport {
    /// The editor's version, `0.9.9`.
    pub version: String,
    /// The blocks below the title, in display order.
    pub sections: Vec<VersionSection>,
}

impl VersionReport {
    /// The page, as markdown: a title, then one `##` section per block with a
    /// `- **Label:** value` row each. A list rather than a table so a value of
    /// any length reads without a column to overflow, in a buffer and in an
    /// issue alike.
    pub fn render(&self) -> String {
        let mut out = format!("# Lattice {}\n", self.version);
        for section in &self.sections {
            out.push_str(&format!("\n## {}\n\n", section.heading));
            for (label, value) in &section.rows {
                out.push_str(&format!("- **{label}:** {value}\n"));
            }
        }
        out
    }
}

static REPORT: OnceLock<VersionReport> = OnceLock::new();

/// Hand the editor the page it should show. Called once, by the binary, before
/// the editor boots; a second call is ignored — the facts it carries do not
/// change while the process runs.
pub fn set_version_report(report: VersionReport) {
    let _ = REPORT.set(report);
}

/// The page text: the binary's report when one was supplied, else the version
/// alone — this crate shares the workspace version, so that line is still
/// right.
pub fn version_page() -> String {
    match REPORT.get() {
        Some(report) => report.render(),
        None => VersionReport {
            version: env!("CARGO_PKG_VERSION").to_string(),
            sections: vec![VersionSection {
                heading: "Build".into(),
                rows: vec![(
                    "Details".into(),
                    "not supplied by this host (`lattice --version --verbose` has them)".into(),
                )],
            }],
        }
        .render(),
    }
}

/// Minor mode for the `*version*` buffer, which is otherwise a markdown
/// buffer: the page IS markdown, so `markdown-mode` is its major and the
/// headings, bold labels and code spans highlight as they would in a file.
/// This mode adds what is particular to the page — read-only, and `Y`.
pub struct VersionMode;

impl VersionMode {
    /// `"version-mode"`.
    pub fn mode_id() -> ModeId {
        ModeId::new("version-mode")
    }
}

impl Mode for VersionMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }

    /// `ReadOnly` above stops typing only; `read-only-mode` is what refuses
    /// `x` / `dd` / `p` — see `messages-mode` for why both are needed.
    fn implies(&self) -> &[ModeId] {
        static IMPLIED: OnceLock<Vec<ModeId>> = OnceLock::new();
        IMPLIED.get_or_init(|| vec![crate::modes::ReadOnlyMode::mode_id()])
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    /// `Y`, not `y`: `y` stays vim's yank operator, so `yiw` on the commit
    /// hash still copies just the hash. `Y` is "the big yank" — the whole
    /// page, ready to paste into an issue.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(version_keymap_entries())
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![ActionHandlerContribution {
            action_name: COPY_ACTION,
            handler: Arc::new(|_ctx: &ActionContext<'_>| Some(copy_page_effect())),
        }]
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async { Ok(()) })
    }
}

fn version_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![keymap_entry! {
            mode: Normal, chord: "Y",
            doc: "version: copy the whole page to the clipboard, for a bug report",
            cmd: "action:version-copy"
        }]
    })
}

/// `Y`: the page into the system clipboard (the `+` register), and a word
/// saying so — a copy with no feedback reads as a key that did nothing.
fn copy_page_effect() -> Effect {
    Effect::Many(vec![
        Effect::Yank {
            register: Register::System,
            content: version_page(),
            kind: YankKind::Linewise,
            explicit_yank: true,
        },
        Effect::Echo {
            level: EchoLevel::Info,
            text: "version: copied to the clipboard".into(),
        },
    ])
}

/// Register `version-mode`, its `Y` action and the `:version` ex-command.
pub fn install(boot: &mut impl SubsystemBoot) {
    boot.modes_mut()
        .register(VersionMode)
        .expect("version-mode registers without conflict");
    // Dead-body: the mode's handler does the work; this makes the keymap's
    // `cmd:` name resolve.
    boot.commands_mut().register_action(
        COPY_ACTION,
        "version: copy the `*version*` page to the clipboard (mode-owned).",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
        },
    );
    boot.commands_mut().register_ex_command(
        "version",
        "Open `*version*` (`:version`) — this build's version, commit, toolchain and \
         target, the renderer it is running in, the plugin API it speaks, and the \
         paths it uses. `Y` copies the page for a bug report.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|_line: &str, _bang: bool| Ok(Args::None)),
            apply: Arc::new(|_ctx| {
                Ok(Effect::OpenSyntheticBuffer {
                    name: VERSION_BUFFER_NAME.to_string(),
                    mode_id: MARKDOWN_MODE_ID.to_string(),
                    content: Some(version_page()),
                    cursor: None,
                    activate_minor: Some(VersionMode::mode_id().as_str().to_string()),
                })
            }),
            args_schema: Vec::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn sample() -> VersionReport {
        VersionReport {
            version: "0.9.9".into(),
            sections: vec![
                VersionSection {
                    heading: "Build".into(),
                    rows: vec![
                        ("Commit".into(), "3f505890abcd (clean), 2026-10-16".into()),
                        ("Profile".into(), "release".into()),
                    ],
                },
                VersionSection {
                    heading: "Paths".into(),
                    rows: vec![("Config".into(), "~/.config/lattice".into())],
                },
            ],
        }
    }

    #[test]
    fn the_page_renders_as_markdown_sections() {
        assert_eq!(
            sample().render(),
            "# Lattice 0.9.9\n\
             \n## Build\n\n\
             - **Commit:** 3f505890abcd (clean), 2026-10-16\n\
             - **Profile:** release\n\
             \n## Paths\n\n\
             - **Config:** ~/.config/lattice\n"
        );
    }

    #[test]
    fn copy_puts_the_page_on_the_system_register_and_says_so() {
        let Effect::Many(effects) = copy_page_effect() else {
            panic!("copy is a yank plus an echo");
        };
        assert!(matches!(
            &effects[0],
            Effect::Yank { register: Register::System, content, explicit_yank: true, .. }
                if content.starts_with("# Lattice ")
        ));
        assert!(matches!(
            &effects[1],
            Effect::Echo {
                level: EchoLevel::Info,
                ..
            }
        ));
    }
}
