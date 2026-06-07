//! `KeymapResolution` — trace result for `resolve_trace` and
//! `resolve_trace_all_modes`.
//!
//! K.3 (2026-06-07): lives in `lattice-keymap` so the `:describe-key`
//! handler in `lattice-host` and unit tests can use the type without
//! depending on the full host stack.

use std::sync::Arc;

use crate::{BindingMode, BoundCommand, KeymapLayer};

/// One layer's hit in a [`KeymapResolution`]. Every registered layer
/// that has a terminal binding for the queried chord sequence
/// contributes one `LayerHit`, in priority order ascending (Builtin
/// first, Buffer last). The `active` flag is set by the caller against
/// the active buffer's `ActiveModes` set to show which hit would
/// actually fire.
#[derive(Debug, Clone)]
pub struct LayerHit {
    /// The layer this binding lives in.
    pub layer: KeymapLayer,
    /// The bound command at this layer.
    pub command: Arc<BoundCommand>,
    /// Whether this layer is active for the buffer in question.
    /// Always `true` for `Builtin`, `User`, and `Buffer` (always-on
    /// layers); set by the `resolve_trace` caller for `MajorMode`
    /// and `MinorMode` layers based on the buffer's active-modes list.
    pub active: bool,
}

/// Full trace of all layer hits for a chord sequence in one
/// `BindingMode`. Returned by `KeymapHandle::resolve_trace`.
///
/// `hits` is ordered by layer priority ascending (Builtin first,
/// Buffer last). The winner is the last active hit (highest-priority
/// active layer wins). An empty `hits` vec means the chord is
/// completely unregistered across all layers.
#[derive(Debug, Clone)]
pub struct KeymapResolution {
    /// The binding mode queried.
    pub mode: BindingMode,
    /// All layer hits in priority order (ascending). Empty when no
    /// layer has a terminal binding for the queried chord sequence.
    pub hits: Vec<LayerHit>,
}

impl KeymapResolution {
    /// The winning hit: the last active hit in priority order
    /// (highest-priority active layer). `None` when no active layer
    /// has a binding for the queried chord.
    pub fn winner(&self) -> Option<&LayerHit> {
        self.hits.iter().rev().find(|h| h.active)
    }

    /// `true` if at least one active layer has a binding.
    pub fn is_bound(&self) -> bool {
        self.winner().is_some()
    }
}

/// Parse a `:describe-key` argument that may carry a mode prefix.
///
/// Supports the six primary-mode prefix shorthands:
///
/// | prefix | mode |
/// |--------|------|
/// | `n_`   | `Normal` |
/// | `i_`   | `Insert` |
/// | `v_`   | `Visual` |
/// | `r_`   | `Replace` |
/// | `c_`   | `Command` |
/// | `s_`   | `Search` |
///
/// Returns `(mode, chord_str)`:
/// - `mode` is `Some(BindingMode)` when a prefix was recognised;
///   `chord_str` is the remainder after stripping the `x_` prefix.
/// - When no prefix matches, `mode` is `None` and `chord_str` is
///   the original input unchanged.
///
/// # Examples
///
/// ```
/// use lattice_keymap::{BindingMode, parse_describe_key_arg};
///
/// let (mode, chord) = parse_describe_key_arg("n_j");
/// assert_eq!(mode, Some(BindingMode::Normal));
/// assert_eq!(chord, "j");
///
/// let (mode, chord) = parse_describe_key_arg("<C-w>j");
/// assert_eq!(mode, None);
/// assert_eq!(chord, "<C-w>j");
///
/// let (mode, chord) = parse_describe_key_arg("i_<C-n>");
/// assert_eq!(mode, Some(BindingMode::Insert));
/// assert_eq!(chord, "<C-n>");
/// ```
pub fn parse_describe_key_arg(s: &str) -> (Option<BindingMode>, &str) {
    const PREFIXES: &[(&str, BindingMode)] = &[
        ("n_", BindingMode::Normal),
        ("i_", BindingMode::Insert),
        ("v_", BindingMode::Visual),
        ("r_", BindingMode::Replace),
        ("c_", BindingMode::Command),
        ("s_", BindingMode::Search),
    ];
    for (prefix, mode) in PREFIXES {
        if let Some(rest) = s.strip_prefix(prefix) {
            return (Some(*mode), rest);
        }
    }
    (None, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use lattice_grammar::{CommandInvocation, SourceLocation};
    use lattice_protocol::ids::CommandId;
    use crate::KeymapLayer;

    fn fake_bound(layer: KeymapLayer) -> Arc<BoundCommand> {
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(CommandId::new(0)),
            SourceLocation::synthetic("test"),
            layer,
        ))
    }

    // ---- parse_describe_key_arg ----

    #[test]
    fn parse_normal_prefix() {
        let (mode, chord) = parse_describe_key_arg("n_j");
        assert_eq!(mode, Some(BindingMode::Normal));
        assert_eq!(chord, "j");
    }

    #[test]
    fn parse_insert_prefix() {
        let (mode, chord) = parse_describe_key_arg("i_<C-n>");
        assert_eq!(mode, Some(BindingMode::Insert));
        assert_eq!(chord, "<C-n>");
    }

    #[test]
    fn parse_visual_prefix() {
        let (mode, chord) = parse_describe_key_arg("v_d");
        assert_eq!(mode, Some(BindingMode::Visual));
        assert_eq!(chord, "d");
    }

    #[test]
    fn parse_replace_prefix() {
        let (mode, chord) = parse_describe_key_arg("r_x");
        assert_eq!(mode, Some(BindingMode::Replace));
        assert_eq!(chord, "x");
    }

    #[test]
    fn parse_command_prefix() {
        let (mode, chord) = parse_describe_key_arg("c_<Tab>");
        assert_eq!(mode, Some(BindingMode::Command));
        assert_eq!(chord, "<Tab>");
    }

    #[test]
    fn parse_search_prefix() {
        let (mode, chord) = parse_describe_key_arg("s_<C-r>");
        assert_eq!(mode, Some(BindingMode::Search));
        assert_eq!(chord, "<C-r>");
    }

    #[test]
    fn parse_no_prefix_returns_none_and_full_string() {
        let (mode, chord) = parse_describe_key_arg("<C-w>j");
        assert_eq!(mode, None);
        assert_eq!(chord, "<C-w>j");
    }

    #[test]
    fn parse_empty_string() {
        let (mode, chord) = parse_describe_key_arg("");
        assert_eq!(mode, None);
        assert_eq!(chord, "");
    }

    #[test]
    fn parse_prefix_only_no_chord() {
        // "n_" with nothing after the prefix — chord_str is "".
        let (mode, chord) = parse_describe_key_arg("n_");
        assert_eq!(mode, Some(BindingMode::Normal));
        assert_eq!(chord, "");
    }

    #[test]
    fn parse_does_not_treat_x_alone_as_prefix() {
        // "n" alone (no underscore) is just a chord, not a prefix.
        let (mode, chord) = parse_describe_key_arg("n");
        assert_eq!(mode, None);
        assert_eq!(chord, "n");
    }

    // ---- KeymapResolution ----

    #[test]
    fn winner_returns_last_active_hit() {
        let r = KeymapResolution {
            mode: BindingMode::Normal,
            hits: vec![
                LayerHit {
                    layer: KeymapLayer::Builtin,
                    command: fake_bound(KeymapLayer::Builtin),
                    active: true,
                },
                LayerHit {
                    layer: KeymapLayer::User,
                    command: fake_bound(KeymapLayer::User),
                    active: true,
                },
            ],
        };
        // Last active hit (highest priority) is User.
        let winner = r.winner().expect("should have winner");
        assert_eq!(winner.layer, KeymapLayer::User);
    }

    #[test]
    fn winner_skips_inactive_hits() {
        let minor = crate::ModeId::new("diff-mode");
        let r = KeymapResolution {
            mode: BindingMode::Normal,
            hits: vec![
                LayerHit {
                    layer: KeymapLayer::Builtin,
                    command: fake_bound(KeymapLayer::Builtin),
                    active: true,
                },
                LayerHit {
                    layer: KeymapLayer::MinorMode(minor),
                    command: fake_bound(KeymapLayer::MinorMode(minor)),
                    active: false, // not active on this buffer
                },
            ],
        };
        // Minor-mode is registered but not active — Builtin wins.
        let winner = r.winner().expect("should have winner");
        assert_eq!(winner.layer, KeymapLayer::Builtin);
    }

    #[test]
    fn winner_none_when_no_active_hits() {
        let minor = crate::ModeId::new("some-mode");
        let r = KeymapResolution {
            mode: BindingMode::Normal,
            hits: vec![LayerHit {
                layer: KeymapLayer::MinorMode(minor),
                command: fake_bound(KeymapLayer::MinorMode(minor)),
                active: false,
            }],
        };
        assert!(r.winner().is_none());
        assert!(!r.is_bound());
    }

    #[test]
    fn is_bound_true_when_active_hit_present() {
        let r = KeymapResolution {
            mode: BindingMode::Normal,
            hits: vec![LayerHit {
                layer: KeymapLayer::Builtin,
                command: fake_bound(KeymapLayer::Builtin),
                active: true,
            }],
        };
        assert!(r.is_bound());
    }

    #[test]
    fn empty_hits_means_not_bound() {
        let r = KeymapResolution {
            mode: BindingMode::Normal,
            hits: vec![],
        };
        assert!(r.winner().is_none());
        assert!(!r.is_bound());
    }
}
