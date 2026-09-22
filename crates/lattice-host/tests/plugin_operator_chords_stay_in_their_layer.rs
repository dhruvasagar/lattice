//! CM.2: EVERY chord a plugin operator's composition creates lands in the
//! plugin's own mode layer — not just the ones `register_operator_bindings_in`
//! binds itself.
//!
//! The function takes the layer from its caller, but three helpers it
//! delegates to — text-object resolutions (`gcap`, `gciw`), find-char
//! (`gcF{char}`) and marks (`gc'{char}`) — hardcoded `KeymapLayer::Builtin`.
//! So a plugin's `gc` was half-scoped: `gcw` lived in `comment-mode` and died
//! with it, while `gcap` sat in the universal layer, fired in every buffer, and
//! outlived `:set comment.enabled=false`. `:describe-key gc` showed it as two
//! registrations, one per layer, which is how it was found.

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_keymap::{BindingMode, ChordPattern, KeymapLayer};
use lattice_protocol::chord::KeyChord;

fn editor() -> Editor {
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::from_text("fn main() {}\n"))
}

fn lit(c: char) -> ChordPattern {
    ChordPattern::Literal(KeyChord::char(c))
}

#[test]
fn every_continuation_of_a_plugin_operator_is_scoped_to_its_mode() {
    let ed = editor();
    let op = {
        let reg = ed.registry.load();
        let id = reg
            .id_by_name("operator:upper")
            .expect("`operator:upper` is registered at boot");
        lattice_grammar::registry::OperatorId(id)
    };
    let wirer = ed
        .services
        .get::<lattice_mode::OperatorChordWirerHandle>()
        .expect("the host publishes its operator-chord wirer at boot");

    // A prefix nothing else binds, so every row under it is this wiring's.
    let mode = lattice_mode::ModeId::new("fixture-op-mode");
    wirer
        .wire(op, "gZ", Some('Z'), mode, 7, "fixture", false)
        .expect("a well-formed chord wires");

    let prefix = [lit('g'), lit('Z')];
    let under = |layer: KeymapLayer| -> Vec<Vec<ChordPattern>> {
        ed.keymap
            .layer_bindings(layer, BindingMode::Normal)
            .into_iter()
            .map(|(path, _)| path)
            .filter(|path| path.starts_with(&prefix))
            .collect()
    };

    let leaked = under(KeymapLayer::Builtin);
    assert!(
        leaked.is_empty(),
        "no continuation of a plugin operator may land in the universal \
         layer — it would fire in every buffer and outlive the mode:\n\
         {leaked:?}"
    );

    // And the three families that leaked are present where they belong, so
    // the assertion above cannot pass by the rows simply not being bound.
    let scoped = under(KeymapLayer::MinorMode(mode));
    let has = |tail: &[ChordPattern]| {
        scoped
            .iter()
            .any(|p| p.len() == prefix.len() + tail.len() && p.ends_with(tail))
    };
    assert!(has(&[lit('a'), lit('p')]), "text object `gZap`: {scoped:?}");
    assert!(
        has(&[lit('F'), ChordPattern::CharLiteral]),
        "find-char `gZF{{char}}`: {scoped:?}"
    );
    assert!(
        has(&[lit('\''), ChordPattern::CharLiteral]),
        "mark `gZ'{{char}}`: {scoped:?}"
    );
}
