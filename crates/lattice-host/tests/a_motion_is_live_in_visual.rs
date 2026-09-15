//! VM.1 (2026-09-15): a motion is an `nvo` command — Normal, Visual and
//! operator-pending — and that is DERIVED from the command's kind, not
//! hand-listed per binding-mode.
//!
//! ## What went wrong that this pins
//!
//! Visual's motion surface used to be a re-registration of
//! `keymap_normal::motion_rows`, copied into `keymap_visual` and again into
//! `keymap_select`. Every motion that entered the Normal binder by another
//! door missed all three copies and was simply dead in Visual: `gg`, `f` / `F`
//! / `t` / `T`, `<C-d>` / `<C-u>`, `<PageUp>` / `<PageDown>`. Plugin motions
//! had it worse — `bind_mode_keymap` binds the one `binding-mode` a mode
//! declared, so org's `[[` moved the cursor in Normal and did nothing in
//! Visual, with no way for the plugin to fix it short of declaring every
//! chord three times.
//!
//! Nothing about Visual's *behaviour* was broken: `write_through_caret`
//! rebuilds the selection from `visual_anchor` + `cursor` after every
//! dispatch, so any reachable cursor-mover extends the selection. The chord
//! just resolved to nothing.
//!
//! The property below is the fix made checkable. A drift test is the right
//! shape here for the same reason `gr_is_declared_once` is: the bug was not a
//! broken chord, it was a LIST that silently lost entries, and a test of any
//! particular chord would have passed on the broken build.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::CommandKind;
use lattice_host::chord::KeyChord;
use lattice_host::editor::Editor;
use lattice_host::keymap_trie::{KeymapLayer, LookupResult};
use lattice_keymap::BindingMode;
use lattice_protocol::ChordPattern;
use lattice_protocol::chord::SpecialKey;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text(
        "alpha one\nbeta two\ngamma three\ndelta four\nepsilon five\n",
    ))
}

/// THE property. Every terminal Normal binding whose command is a `Motion`
/// resolves to the same command in Visual and in Select, in the same layer.
///
/// Scoped to `KeymapLayer::Builtin` because that is the layer a test can
/// enumerate without standing up plugins; the derivation itself is
/// layer-agnostic (`expand_grammar_rows` takes the layer as a parameter and
/// boot runs it over every mode layer too), and
/// `a_mode_layer_motion_gets_its_visual_peer` below covers the mode-layer
/// case directly.
#[test]
fn every_builtin_motion_is_live_in_visual_and_select() {
    let editor = boot();
    let commands = editor.registry.load();
    let layer = KeymapLayer::Builtin;

    let visual: Vec<Vec<ChordPattern>> = editor
        .keymap
        .layer_bindings(layer, BindingMode::Visual)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    let select: Vec<Vec<ChordPattern>> = editor
        .keymap
        .layer_bindings(layer, BindingMode::Select)
        .into_iter()
        .map(|(path, _)| path)
        .collect();

    let mut checked = 0usize;
    let mut missing: Vec<String> = Vec::new();
    for (path, bound) in editor.keymap.layer_bindings(layer, BindingMode::Normal) {
        let Some(spec) = commands.lookup(bound.command.command) else {
            continue;
        };
        if !matches!(spec.kind, CommandKind::Motion) {
            continue;
        }
        checked += 1;
        if !visual.contains(&path) {
            missing.push(format!(
                "Visual is missing {} -> {}",
                render(&path),
                spec.name
            ));
        }
        if !select.contains(&path) {
            missing.push(format!(
                "Select is missing {} -> {}",
                render(&path),
                spec.name
            ));
        }
    }

    assert!(
        checked >= 40,
        "test premise: expected the builtin motion table, walked only {checked} motions"
    );
    assert!(
        missing.is_empty(),
        "a motion bound in Normal must be live in Visual and Select:\n{}",
        missing.join("\n")
    );
}

/// The four families that were dead before VM.1, named individually so a
/// regression reads as the chord the user pressed rather than as a count.
///
/// `f` and `t` are the wildcard-capture shape (`[f, CharLiteral]`), so the
/// bare prefix must come back `Partial` — that is what `dispatch_visual`
/// absorbs into `partial_chord` before the target char resolves the pair.
#[test]
fn the_motions_that_were_dead_in_visual_resolve_now() {
    let editor = boot();

    for (label, path) in [
        ("gg", vec![KeyChord::char('g'), KeyChord::char('g')]),
        ("<C-d>", vec![KeyChord::ctrl('d')]),
        ("<C-u>", vec![KeyChord::ctrl('u')]),
        ("<PageDown>", vec![KeyChord::special(SpecialKey::PageDown)]),
        ("<PageUp>", vec![KeyChord::special(SpecialKey::PageUp)]),
    ] {
        for mode in [BindingMode::Visual, BindingMode::Select] {
            assert!(
                matches!(
                    editor.keymap.lookup(mode, &path),
                    LookupResult::Bound { .. }
                ),
                "{label} must be Bound in {mode:?}"
            );
        }
    }

    for (label, prefix) in [
        ("f", KeyChord::char('f')),
        ("F", KeyChord::char('F')),
        ("t", KeyChord::char('t')),
        ("T", KeyChord::char('T')),
    ] {
        for mode in [BindingMode::Visual, BindingMode::Select] {
            assert!(
                matches!(editor.keymap.lookup(mode, &[prefix]), LookupResult::Partial),
                "{label} must be a Partial in {mode:?} (the CharLiteral resolves it)"
            );
        }
    }
}

/// The operator half of `nvo`. `dgg` and `d<C-d>` were unbound for the same
/// reason `vgg` was, so the derivation writes those rows too.
#[test]
fn the_same_motions_compose_with_an_operator() {
    let editor = boot();
    for (label, path) in [
        (
            "dgg",
            vec![
                KeyChord::char('d'),
                KeyChord::char('g'),
                KeyChord::char('g'),
            ],
        ),
        ("y<C-d>", vec![KeyChord::char('y'), KeyChord::ctrl('d')]),
        (
            "c<PageDown>",
            vec![KeyChord::char('c'), KeyChord::special(SpecialKey::PageDown)],
        ),
    ] {
        assert!(
            matches!(
                editor.keymap.lookup(BindingMode::Normal, &path),
                LookupResult::Bound { .. }
            ),
            "{label} must resolve to an operator invocation"
        );
    }
}

/// Behaviour, not just reachability: `<C-d>` in Visual must move the cursor
/// AND leave the selection spanning what it crossed.
///
/// Worth asserting separately because reachability alone would pass on a
/// build where the motion fired but the anchor was dropped — and the anchor
/// is the half that makes it a *selection* rather than a jump.
#[test]
fn a_derived_visual_motion_extends_the_selection() {
    let mut editor = Editor::boot(CoreDocument::from_text(&"line\n".repeat(60)));
    let mut partial = Vec::new();
    editor.cursor.line = 0;
    let _ = editor.dispatch_chord(KeyChord::char('v'), &mut partial);
    let anchor = editor.visual_anchor.expect("`v` arms the anchor");

    let _ = editor.dispatch_chord(KeyChord::ctrl('d'), &mut partial);

    assert!(
        editor.cursor.line > anchor.line,
        "<C-d> must move the cursor down (anchor {}, cursor {})",
        anchor.line,
        editor.cursor.line
    );
    let region = editor
        .visual_selection_range()
        .expect("Visual is still live after a motion");
    assert_eq!(region.start.line, anchor.line);
    assert_eq!(region.end.line, editor.cursor.line);
}

/// The plugin case, without a plugin: a motion bound into a mode layer's
/// Normal trie — the exact shape `bind_mode_keymap` produces for org's `[[`
/// — gets its Visual, Select and operator rows from the same pass.
///
/// `register_plugin_motion` rather than a builtin id so the test proves the
/// derivation keys on the command's KIND, not on membership of any host-side
/// table.
#[test]
fn a_mode_layer_motion_gets_its_visual_peer() {
    use lattice_grammar::command::CommandInvocation;
    use lattice_grammar::registry::{CommandRegistry, MotionResult, MotionSpec};
    use lattice_mode::ModeId;
    use std::sync::Arc;

    let editor = boot();
    let mode = ModeId::new("vm1-fake-org-mode");
    let layer = KeymapLayer::MajorMode(mode);

    // A fresh registry carrying one plugin motion. `expand_grammar_rows`
    // resolves kinds against whatever registry it is handed, so a throwaway
    // is enough — and keeps the editor's own catalog untouched.
    let mut registry = CommandRegistry::new();
    let builtins = lattice_grammar::builtins::populate(&mut registry);
    let motion = registry.register_plugin_motion(
        7,
        "fake-org-next-headline",
        "Move to the next headline",
        MotionSpec {
            jump: true,
            exclusive: true,
            apply: Arc::new(|ctx| {
                Ok(MotionResult {
                    target: ctx.from,
                    linewise: false,
                    exclusive: None,
                })
            }),
            args_schema: Vec::new(),
        },
    );

    let path = [
        ChordPattern::Literal(KeyChord::char(']')),
        ChordPattern::Literal(KeyChord::char(']')),
    ];
    editor.keymap.bind(
        layer,
        BindingMode::Normal,
        &path,
        CommandInvocation::of(motion.0),
        lattice_grammar::SourceLocation::plugin(7),
    );

    lattice_host::keymap_normal::expand_grammar_rows(&editor.keymap, &registry, &builtins, layer);

    for mode_kind in [BindingMode::Visual, BindingMode::Select] {
        let bound = editor
            .keymap
            .layer_bindings(layer, mode_kind)
            .into_iter()
            .find(|(p, _)| p.as_slice() == path.as_slice());
        let (_, bound) =
            bound.unwrap_or_else(|| panic!("a plugin motion must be live in {mode_kind:?}"));
        assert_eq!(
            bound.command.command, motion.0,
            "{mode_kind:?} must invoke the motion itself, not a rewritten command"
        );
    }

    // And `d]]` — the operator half.
    let op_path: Vec<ChordPattern> = std::iter::once(ChordPattern::Literal(KeyChord::char('d')))
        .chain(path.iter().cloned())
        .collect();
    assert!(
        editor
            .keymap
            .layer_bindings(layer, BindingMode::Normal)
            .into_iter()
            .any(|(p, _)| p == op_path),
        "a plugin motion must compose with `d`"
    );
}

/// The derivation fills gaps; it never overwrites. Visual's `x` -> delete and
/// `s` -> change aliases, the find-char paths' `Args::Char` routing under an
/// operator, and any mode's own Visual override are deliberate statements,
/// and a default that clobbers them is worse than no default.
#[test]
fn a_deliberate_visual_binding_survives_the_derivation() {
    let editor = boot();
    let commands = editor.registry.load();

    // `x` in Visual is the delete OPERATOR on the selection, not Normal's
    // delete-char action — proof the pass did not flatten Visual onto Normal.
    let x = editor
        .keymap
        .layer_bindings(KeymapLayer::Builtin, BindingMode::Visual)
        .into_iter()
        .find(|(p, _)| p.as_slice() == [ChordPattern::Literal(KeyChord::char('x'))])
        .map(|(_, b)| b)
        .expect("Visual binds `x`");
    let spec = commands.lookup(x.command.command).expect("`x` resolves");
    assert!(
        matches!(spec.kind, CommandKind::Operator),
        "Visual `x` must stay the delete operator, got {:?} ({})",
        spec.kind,
        spec.name
    );

    // `df<char>` keeps the operator-pending binding that routes the captured
    // char, rather than the derivation's plain `Target::Motion` row.
    let dfc = [
        ChordPattern::Literal(KeyChord::char('d')),
        ChordPattern::Literal(KeyChord::char('f')),
        ChordPattern::CharLiteral,
    ];
    let bound = editor
        .keymap
        .layer_bindings(KeymapLayer::Builtin, BindingMode::Normal)
        .into_iter()
        .find(|(p, _)| p.as_slice() == dfc)
        .map(|(_, b)| b)
        .expect("`df<char>` stays bound");
    let spec = commands
        .lookup(bound.command.command)
        .expect("`df<char>` resolves");
    assert!(
        matches!(spec.kind, CommandKind::Operator),
        "`df<char>` must stay an operator invocation, got {:?}",
        spec.kind
    );
}

fn render(path: &[ChordPattern]) -> String {
    path.iter()
        .map(|p| match p {
            ChordPattern::Literal(c) => format!("{c}"),
            ChordPattern::CharLiteral => "{char}".to_string(),
        })
        .collect::<Vec<_>>()
        .join("")
}
