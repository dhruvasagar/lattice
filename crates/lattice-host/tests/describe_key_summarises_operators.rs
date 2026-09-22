//! DK.5: `:describe-key` on an operator says it is one, instead of listing
//! every chord that can follow it.
//!
//! An operator composes with every motion and text object by construction, so
//! enumerating `gUw` / `gUap` / `gUi{` / `gUF{char}` / `gU'{char}` is a screen
//! of rows that all say the same thing, and the one fact explaining all of
//! them — that `gU` is an operator — is buried in it.
//!
//! **Why `gU` and not `d`.** A single-key operator is BOUND (`d` runs
//! `action:absorb-operator-delete`), and a bound prefix kills its longer
//! chords: the trie stops there and `dw` resolves through the operator-pending
//! state machine instead. So `d` has no continuations to summarise. The noise
//! is specific to MULTI-KEY operator prefixes — `gU`, `zn`, `gc` — which are
//! unbound, so `register_operator_bindings_in`'s whole subtree sits in the
//! trie under them.

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

fn editor() -> Editor {
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::from_text("fn main() {}\n"))
}

fn text(content: &lattice_help::HelpContent) -> String {
    content.buffer.content.as_string()
}

#[test]
fn an_operator_is_summarised_rather_than_enumerated() {
    let ed = editor();
    let body = text(&ed.build_describe_key_content("gU"));

    assert!(
        body.contains("gU is an operator"),
        "the heading states the kind — the fact that explains every \
         continuation:\n{body}"
    );
    assert!(
        body.contains("any motion or text object"),
        "and says what that means in a sentence:\n{body}"
    );
    assert!(
        !body.contains("─── Continuations of gU ───"),
        "the enumeration is replaced, not merely preceded:\n{body}"
    );

    // The count survives. It is the cheap signal that the operator-pending
    // wiring happened at all — a surprising ZERO here is the shape of a
    // plugin operator whose chord never got bound.
    assert!(
        body.contains("continuation(s) are registered and not listed"),
        "the count is still reported:\n{body}"
    );
    assert!(
        !body.contains(
            "not listed — they are the motion and text-object \
                        grammar"
        ) || !body.contains("  0 continuation(s)"),
        "a real operator has continuations; zero would mean nothing was wired"
    );
}

/// The other half. For a chord that is ONLY a prefix, the subtree IS the
/// answer, so it must keep enumerating — this is what stops the summary from
/// being applied where it would remove the only useful content.
#[test]
fn a_bare_prefix_still_enumerates_its_subtree() {
    let ed = editor();
    let body = text(&ed.build_describe_key_content("z"));

    assert!(
        body.contains("Continuations of z"),
        "a prefix's continuations are the answer, not noise:\n{body}"
    );
    assert!(
        !body.contains("z is an operator"),
        "and `z` is not an operator:\n{body}"
    );
}

/// The partition. An operator prefix is not the operator's private namespace:
/// a minor mode can bind `gUs` to something of its own, and that binding is
/// news — it is precisely what `:describe-key` exists to surface.
///
/// An earlier draft was all-or-nothing and reverted to the full enumeration
/// the moment one foreign binding existed, which buried the one interesting
/// row under seventy that say nothing. So the two are separated: the
/// operator's own grammar is summarised by count, everything else under the
/// prefix is listed under its own heading.
#[test]
fn a_foreign_binding_under_an_operator_prefix_is_still_listed() {
    let ed = editor();

    // A real registered command, so the row resolves to a name rather than to
    // an id that no lookup answers — and a MOTION rather than an operator,
    // because a second operator id under the prefix is exactly the signal
    // that suppresses the summary (that is what makes `g` a bare prefix).
    let victim = {
        let reg = ed.registry.load();
        reg.id_by_name("motion:line-down")
            .expect("`motion:line-down` is registered at boot")
    };
    let mode = lattice_mode::ModeId::new("fixture-foreign-mode");
    let mut trie = lattice_keymap::KeymapTrie::new();
    trie.insert(
        &[
            lattice_keymap::ChordPattern::Literal(lattice_protocol::chord::KeyChord::char('g')),
            lattice_keymap::ChordPattern::Literal(lattice_protocol::chord::KeyChord::char('U')),
            lattice_keymap::ChordPattern::Literal(lattice_protocol::chord::KeyChord::char('s')),
        ],
        std::sync::Arc::new(lattice_keymap::BoundCommand::from_invocation(
            lattice_grammar::CommandInvocation::of(victim),
            lattice_grammar::SourceLocation::synthetic("fixture-foreign-mode"),
            lattice_keymap::KeymapLayer::MinorMode(mode),
        )),
    );
    ed.keymap.push_layer(
        lattice_host::keymap_registry::PushLayerKind::MinorMode(mode),
        "fixture-foreign",
        std::collections::HashMap::from([(lattice_keymap::BindingMode::Normal, trie)]),
    );

    let body = text(&ed.build_describe_key_content("gU"));

    assert!(
        body.contains("gU is an operator"),
        "the summary still applies — one foreign chord does not make the \
         operator's own grammar worth enumerating:\n{body}"
    );
    let (_, also) = body
        .split_once("─── Also bound below gU ───")
        .unwrap_or_else(|| panic!("the foreign binding gets its own heading:\n{body}"));
    assert!(also.contains("gUs"), "…naming the chord:\n{body}");
    // Scoped to the section on purpose: the SUMMARY prints `gUw` / `gUap` as
    // examples of what an operator composes with, so asserting against the
    // whole body would pass on the wall this test exists to prevent.
    assert!(
        !also.contains("gUw") && !also.contains("gUap"),
        "while the operator's own grammar stays out of that section — it is \
         summarised, and listing it under `Also bound` is the same wall \
         wearing a different heading:\n{body}"
    );
}

/// DK.6: the summary keeps the provenance the enumeration carried. Every
/// folded row used to print `layer:` / `source:`; summarising them must not
/// turn "who registered this operator, and in which layer" into a question the
/// view no longer answers.
///
/// `gU` is the right probe because its grammar lives in TWO layers. Built-in's
/// rows come from the host's keymap; multibuffer-mode's are `gU` composed with
/// that mode's own motions, so they name `lattice-multibuffer` — the derived
/// row carries the source of the binding it was derived from.
#[test]
fn the_summary_names_each_layer_and_source_the_operator_came_from() {
    let ed = editor();
    let body = text(&ed.build_describe_key_content("gU"));

    let (_, summary) = body
        .split_once("─── gU is an operator ───")
        .unwrap_or_else(|| panic!("the operator summary is present:\n{body}"));
    let summary = summary.split("───").next().unwrap_or(summary);
    let (_, origins) = summary
        .split_once("Registered in:")
        .unwrap_or_else(|| panic!("the summary says where it was registered:\n{body}"));

    let layer_lines: Vec<&str> = origins.lines().filter(|l| l.contains("layer:")).collect();
    assert_eq!(
        layer_lines.len(),
        2,
        "one line per LAYER — a layer holding rows from several sources is \
         still one layer:\n{body}"
    );
    assert!(
        layer_lines
            .iter()
            .any(|l| l.contains("layer: Built-in ") && l.contains("keymap_normal.rs")),
        "the Built-in layer is named, with its source:\n{body}"
    );
    assert!(
        layer_lines
            .iter()
            .any(|l| l.contains("layer: multibuffer-mode ")),
        "and so is the second layer — collapsing to one would hide it:\n{body}"
    );
    assert!(
        origins.contains("lattice-multibuffer"),
        "multibuffer-mode's rows name the mode that declared their motions, \
         not the host pass that derived them:\n{body}"
    );
    assert!(
        layer_lines.iter().all(|l| l.contains("continuation(s)")),
        "each layer reports its share of the count:\n{body}"
    );
}
