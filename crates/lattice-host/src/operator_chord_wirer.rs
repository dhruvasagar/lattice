//! CM.2: the host's answer to a plugin operator that declared a chord.
//!
//! `lattice-plugin-loader` learns, at drain time, that a plugin's operator
//! wants `gc`. It cannot bind it: the operator-pending composition — motion
//! targets, the doubled linewise form, `i_` / `a_` text-object pendings, the
//! `f` / `F` / `t` / `T` find-char pendings — is built from host-resolved
//! builtin ids, and the loader does not (and should not) depend on
//! `lattice-host`.
//!
//! So the host implements [`lattice_mode::OperatorChordWirer`] over
//! [`crate::keymap_normal::register_operator_bindings_in`] and publishes it as
//! a service. This is the same split N.1.3 made for narrow's `zn`, with a
//! process boundary in the middle: the provider owns the spec and `apply`, the
//! host owns the chord wiring.

use lattice_keymap::{KeymapHandle, KeymapLayer};
use lattice_protocol::chord::ChordPattern;

// From their origin crates: `keymap_normal` re-exports these privately.
use lattice_grammar::builtins::Builtins;
use lattice_syntax::motions::SyntaxMotionIds;
use lattice_syntax::text_objects::SyntaxTextObjectIds;

/// Holds the boot-resolved pieces the composition needs. Cheap to clone; the
/// id tables are `Copy`-ish value types captured once at boot.
pub struct HostOperatorChordWirer {
    keymap: KeymapHandle,
    builtins: Builtins,
    syntax_textobjects: SyntaxTextObjectIds,
    syntax_motions: SyntaxMotionIds,
}

impl HostOperatorChordWirer {
    pub fn new(
        keymap: KeymapHandle,
        builtins: Builtins,
        syntax_textobjects: SyntaxTextObjectIds,
        syntax_motions: SyntaxMotionIds,
    ) -> Self {
        Self {
            keymap,
            builtins,
            syntax_textobjects,
            syntax_motions,
        }
    }
}

impl lattice_mode::OperatorChordWirer for HostOperatorChordWirer {
    fn wire(
        &self,
        op: lattice_grammar::registry::OperatorId,
        chord: &str,
        doubled: Option<char>,
        mode: lattice_mode::ModeId,
        plugin_id: u32,
        plugin_name: &str,
        post_motion_char: bool,
    ) -> Result<(), String> {
        // A chord that does not parse is a manifest error, reported rather than
        // guessed at. `register-binding` takes the same line: a plugin never
        // silently mis-binds.
        let prefix: Vec<ChordPattern> = lattice_protocol::parse_chord_sequence(chord)
            .map_err(|e| format!("chord {chord:?} does not parse: {e}"))?
            .into_iter()
            .map(ChordPattern::Literal)
            .collect();
        if prefix.is_empty() {
            return Err(format!("chord {chord:?} is empty"));
        }

        crate::keymap_normal::register_operator_bindings_in(
            // Scoped to the plugin's own mode, never `Builtin`: a chord bound
            // at `Builtin` would outlive `:set <id>.enabled=false` and point at
            // a handler that is gone.
            KeymapLayer::MinorMode(mode),
            // CM.4: stamped as the PLUGIN's, not this file's. `:describe-key
            // gc` answers "where did this come from", and the honest answer is
            // the plugin that declared the chord.
            lattice_grammar::SourceLocation::plugin_named(plugin_id, plugin_name),
            &self.keymap,
            &prefix,
            op,
            doubled.map(|c| ChordPattern::Literal(lattice_protocol::chord::KeyChord::char(c))),
            &self.builtins,
            &self.syntax_textobjects,
            &self.syntax_motions,
            post_motion_char,
        );
        Ok(())
    }
}
