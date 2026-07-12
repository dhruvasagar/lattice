//! Provenance metadata (DESIGN.md §5.11).
//!
//! Every registered / bound / set thing carries a `SourceLocation`.
//! `:describe-*` formatters render it as a `[[file:...]]`-style link
//! the user can follow to inspect or edit the source.
//!
//! Forgery prevention is structural: there is no public function that
//! takes a `SourceLocation` parameter and stores it. Built-in
//! registrations capture the call site via `#[track_caller]`; static
//! slices use declarative macros (`keymap_entry!`, ...) that inject
//! the location at each row's site. Trusted subsystems (config
//! loader, plugin host bridge, runtime dispatcher) reach the
//! `pub(crate) insert_*` registry methods directly and construct
//! sources from their own ground truth.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceLocation {
    pub layer: SourceLayer,
    pub kind: SourceKind,
}

/// Why this thing exists -- editor binary, user config, plugin, etc.
/// Renders as a one-word label next to the `[[link]]` in help output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SourceLayer {
    /// Compiled into the editor binary.
    Builtin,
    /// `~/.config/lattice/config.toml`.
    UserConfig,
    /// `.lattice/config.toml` at workspace root.
    ProjectConfig,
    /// Per-buffer modeline / `:setlocal` directive.
    Modeline,
    /// Typed at `:`, replayed by macro, or `.` re-dispatch.
    Runtime,
    /// WASM plugin (Phase 7). The `u32` is the plugin id issued by
    /// the host.
    Plugin(u32),
}

impl SourceLayer {
    pub fn label(self) -> &'static str {
        match self {
            SourceLayer::Builtin => "built-in",
            SourceLayer::UserConfig => "user config",
            SourceLayer::ProjectConfig => "project config",
            SourceLayer::Modeline => "modeline",
            SourceLayer::Runtime => "runtime",
            SourceLayer::Plugin(_) => "plugin",
        }
    }
}

/// Where to look. Most cases are a real file with a line; a few are
/// synthetic origins that the link follower routes to a different
/// kind of buffer (command-history, macro buffer, transitive
/// dot-repeat).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SourceKind {
    /// Concrete file location, optionally with a line.
    File { path: PathBuf, line: Option<u32> },
    /// `:` invocation. Link follower opens the command-history
    /// buffer at `history_index`.
    CommandLine { history_index: usize },
    /// Replayed from a recorded macro. Link follower opens the
    /// `*macro:<reg>*` buffer at `step`.
    MacroReplay { register: char, step: u32 },
    /// `.` re-dispatch. Boxes the originating source so chains of
    /// dot-repeats trace back to where the change actually came
    /// from. The link follower follows the inner source.
    DotRepeat(Box<SourceLocation>),
    /// Synthetic origin like `<initial-load>` or `<test-fixture>`.
    /// Renders inert. Owned `String` (not `&'static`) so the type
    /// can derive Deserialize for plugin-bridge wire transport.
    Synthetic(String),
}

impl SourceLocation {
    /// Construct a `Builtin` source from a `(file, line)` pair.
    /// Almost always called via `Location::caller()` in a
    /// `#[track_caller]` registration method.
    pub fn builtin_file(file: &str, line: u32) -> Self {
        Self {
            layer: SourceLayer::Builtin,
            kind: SourceKind::File {
                path: PathBuf::from(file),
                line: Some(line),
            },
        }
    }

    /// Synthetic source for tests + the rare runtime case where no
    /// concrete origin exists.
    pub fn synthetic(tag: impl Into<String>) -> Self {
        Self {
            layer: SourceLayer::Runtime,
            kind: SourceKind::Synthetic(tag.into()),
        }
    }

    /// Provenance for a WASM-plugin contribution (Phase 7). The layer is
    /// [`SourceLayer::Plugin`] carrying the **host-issued** `plugin_id` (§6 —
    /// the guest never supplies it, so a plugin cannot forge a builtin/user
    /// provenance); the kind is a synthetic `<plugin:N>` tag (a plugin has no
    /// file/line the link follower could open — the plugin-manager view is the
    /// eventual target). This is the *only* forgery-safe way a cross-crate
    /// trusted subsystem stamps `Plugin` provenance: the public
    /// `CommandRegistry::register_plugin_*` methods take a `u32`, never a
    /// `SourceLocation`, and route through here.
    pub fn plugin(plugin_id: u32) -> Self {
        Self {
            layer: SourceLayer::Plugin(plugin_id),
            kind: SourceKind::Synthetic(format!("<plugin:{plugin_id}>")),
        }
    }

    /// Render as a markdown link for inclusion in a help body.
    /// Format: `[label](scheme:value)`. The link's URL portion is
    /// what `parse_help_links` (in `lattice-ui-tui::help`)
    /// classifies into a typed `lattice_ui_tui::help::HelpLinkTarget`; the label
    /// is what the user sees.
    pub fn as_link(&self) -> String {
        match &self.kind {
            SourceKind::File {
                path,
                line: Some(n),
            } => {
                let label = format!("{}:{}", path.display(), n);
                format!("[{label}](file:{label})")
            }
            SourceKind::File { path, line: None } => {
                let label = format!("{}", path.display());
                format!("[{label}](file:{label})")
            }
            SourceKind::CommandLine { history_index } => {
                format!("[command:{history_index}](history:command:{history_index})")
            }
            SourceKind::MacroReplay { register, step } => {
                format!("[macro:{register}:{step}](macro:{register}:step:{step})")
            }
            SourceKind::DotRepeat(inner) => {
                // The inner source already renders as a markdown link;
                // wrap it as the label of an outer link whose URL is
                // a synthetic dot-repeat-of: scheme. Consumers can
                // peel one level off if they care.
                format!("[dot-repeat-of:{}](dot-repeat-of:inner)", inner.as_link())
            }
            SourceKind::Synthetic(tag) => format!("[<{tag}>](synthetic:{tag})"),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn layer_labels_are_human_readable() {
        assert_eq!(SourceLayer::Builtin.label(), "built-in");
        assert_eq!(SourceLayer::UserConfig.label(), "user config");
        assert_eq!(SourceLayer::Plugin(7).label(), "plugin");
    }

    #[test]
    fn builtin_file_renders_as_file_link_with_line() {
        let s = SourceLocation::builtin_file("src/foo.rs", 42);
        assert_eq!(s.as_link(), "[src/foo.rs:42](file:src/foo.rs:42)");
    }

    #[test]
    fn file_without_line_renders_path_only() {
        let s = SourceLocation {
            layer: SourceLayer::Builtin,
            kind: SourceKind::File {
                path: PathBuf::from("foo.rs"),
                line: None,
            },
        };
        assert_eq!(s.as_link(), "[foo.rs](file:foo.rs)");
    }

    #[test]
    fn command_line_kind_renders_history_link() {
        let s = SourceLocation {
            layer: SourceLayer::Runtime,
            kind: SourceKind::CommandLine { history_index: 3 },
        };
        assert_eq!(s.as_link(), "[command:3](history:command:3)");
    }

    #[test]
    fn macro_replay_kind_renders_macro_link() {
        let s = SourceLocation {
            layer: SourceLayer::Runtime,
            kind: SourceKind::MacroReplay {
                register: 'q',
                step: 5,
            },
        };
        assert_eq!(s.as_link(), "[macro:q:5](macro:q:step:5)");
    }

    #[test]
    fn dot_repeat_chains_through_inner_source() {
        let inner = SourceLocation::builtin_file("src/foo.rs", 7);
        let s = SourceLocation {
            layer: SourceLayer::Runtime,
            kind: SourceKind::DotRepeat(Box::new(inner)),
        };
        assert_eq!(
            s.as_link(),
            "[dot-repeat-of:[src/foo.rs:7](file:src/foo.rs:7)](dot-repeat-of:inner)"
        );
    }

    #[test]
    fn synthetic_kind_renders_tagged_link() {
        let s = SourceLocation::synthetic("initial-load");
        assert_eq!(s.as_link(), "[<initial-load>](synthetic:initial-load)");
    }
}
