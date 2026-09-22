//! The rows `expand_grammar_rows` DERIVES from a motion or text object carry
//! the source of the binding they were derived from.
//!
//! A plugin declares `]]` (a motion) or `ih` (a text object) once; the host
//! pass adds `d]]` / `c]]` / `dih` / `yah` and the text object's Visual row.
//! Those rows were stamped with the pass's own `source()`, so
//! `:describe-key dih` named `keymap_normal.rs` for a text object org
//! declared, and the plugin that owns the chord was invisible exactly where a
//! user would ask. The layer was already right; the provenance was not.

use lattice_core::Document as CoreDocument;
use lattice_grammar::SourceLocation;
use lattice_grammar::command::CommandInvocation;
use lattice_host::editor::Editor;
use lattice_keymap::{BindingMode, ChordPattern, KeymapLayer};
use lattice_protocol::chord::KeyChord;

fn lit(c: char) -> ChordPattern {
    ChordPattern::Literal(KeyChord::char(c))
}

#[test]
fn derived_operator_and_visual_rows_name_the_declaring_source() {
    lattice_plugin_loader::disable_autoload();
    let ed = Editor::boot(CoreDocument::from_text("\n"));
    let layer = KeymapLayer::MinorMode(lattice_mode::ModeId::new("fixture-derive-mode"));
    let declared = SourceLocation::plugin_named(7, "fixture");

    // Paths nothing else binds in this layer, so every row under them below is
    // one the pass derived.
    let motion_path = [lit('Q'), lit('m')];
    let tobj_path = [lit('Q'), lit('o')];
    ed.keymap.bind(
        layer,
        BindingMode::Normal,
        &motion_path,
        CommandInvocation::of(ed.builtins.word_forward.0),
        declared.clone(),
    );
    ed.keymap.bind(
        layer,
        BindingMode::Normal,
        &tobj_path,
        CommandInvocation::of(ed.builtins.inner_paragraph.0),
        declared.clone(),
    );

    let added = lattice_host::keymap_normal::expand_grammar_rows(
        &ed.keymap,
        &ed.registry.load(),
        &ed.builtins,
        layer,
    );
    assert!(added > 0, "the pass derived rows to check");

    let mut checked = 0;
    for mode in [BindingMode::Normal, BindingMode::Visual] {
        for (path, bound) in ed.keymap.layer_bindings(layer, mode) {
            let derived_from_ours = path.ends_with(&motion_path) || path.ends_with(&tobj_path);
            if !derived_from_ours {
                continue;
            }
            checked += 1;
            assert_eq!(
                bound.source, declared,
                "{mode:?} {path:?} was derived from the fixture's binding and \
                 must name it, not the host pass"
            );
        }
    }
    // `d` + the Visual row alone make this > 2; a count this low means the
    // loop saw nothing and the assertion above proved nothing.
    assert!(checked > 2, "derived rows were found ({checked})");
}
