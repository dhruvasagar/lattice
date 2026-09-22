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
