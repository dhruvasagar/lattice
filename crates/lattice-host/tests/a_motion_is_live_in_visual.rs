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
//!
//! ## VM.4: by construction, in every layer
//!
//! VM.1 derived the Visual rows in a host pass that ran at boot and on
//! `PluginLoaded`. A re-pushed mode layer, `init.rs` and plugin
//! `register-binding` all wrote bindings outside those two moments, and this
//! file's drift test walked `KeymapLayer::Builtin` only, so it couldn't have
//! noticed. The keymap now mirrors motions at every write, and the drift test
//! walks every layer.
//!
//! Select takes only the motions that can't be typed. In Select a printable
//! replaces the selection, and a bound printable would take the key first, so
//! `gg`, `f`, `%` and `[[` are Visual-only while `<C-d>` and `<PageDown>`
//! extend in both. The "not in Select" half is asserted as firmly as the rest,
//! because a printable in Select is the bug.

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

/// Does a binding at `path` overtype in Select, and so belong in Visual only?
/// Only the first chord matters: it's the one looked up on a fresh keystroke.
fn typed_in_select(path: &[ChordPattern]) -> bool {
    match path.first() {
        Some(ChordPattern::CharLiteral) | None => true,
        Some(ChordPattern::Literal(c)) => lattice_keymap::overtypes_in_select(c),
    }
}

/// THE property. Every terminal Normal binding whose command is a `Motion`
/// has a row at the same path in Visual, in the same layer, and has one in
/// Select exactly when its first chord can't be typed. Every layer, not just
/// `Builtin`.
#[test]
fn every_motion_is_live_in_visual_and_only_non_printables_in_select() {
    let editor = boot();
    let commands = editor.registry.load();

    let mut checked = 0usize;
    let mut missing: Vec<String> = Vec::new();
    for layer in editor.keymap.layers() {
        let paths = |mode| -> Vec<Vec<ChordPattern>> {
            editor
                .keymap
                .layer_bindings(layer, mode)
                .into_iter()
                .map(|(path, _)| path)
                .collect()
        };
        let (visual, select) = (paths(BindingMode::Visual), paths(BindingMode::Select));
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
                    "{layer:?}: Visual is missing {} -> {}",
                    render(&path),
                    spec.name
                ));
            }
            match (typed_in_select(&path), select.contains(&path)) {
                (false, false) => missing.push(format!(
                    "{layer:?}: Select is missing non-printable {} -> {}",
                    render(&path),
                    spec.name
                )),
                (true, true) => missing.push(format!(
                    "{layer:?}: Select binds PRINTABLE {} -> {}, which would take typed text",
                    render(&path),
                    spec.name
                )),
                _ => {}
            }
        }
    }

    assert!(
        checked >= 40,
        "test premise: expected at least the builtin motion table, walked only {checked} motions"
    );
    assert!(
        missing.is_empty(),
        "a Normal motion must be live in Visual, and in Select iff it can't be typed:\n{}",
        missing.join("\n")
    );
}

/// The families that were dead before VM.1, named individually so a
/// regression reads as the chord the user pressed rather than as a count.
///
/// `f` and `t` are the wildcard-capture shape (`[f, CharLiteral]`), so the
/// bare prefix must come back `Partial` in Visual — that is what
/// `dispatch_visual` absorbs into `partial_chord` before the target char
/// resolves the pair.
///
/// Select differs on purpose. `g`, `f`, `F`, `t` and `T` are typed text there,
/// so they must stay `Unbound` for the overtype fallback to see them.
#[test]
fn the_motions_that_were_dead_in_visual_resolve_now() {
    let editor = boot();

    for (label, path) in [
        // VM.3j-2: `<C-d>` / `<C-u>` are scroll COMMANDS now rather than
        // motions, and still bound in Visual — vim scrolls there too, so the
        // requirement this row encodes is unchanged even though the mechanism
        // that satisfies it is.
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

    assert!(
        matches!(
            editor.keymap.lookup(
                BindingMode::Visual,
                &[KeyChord::char('g'), KeyChord::char('g')]
            ),
            LookupResult::Bound { .. }
        ),
        "gg must be Bound in Visual"
    );
    for (label, prefix) in [
        ("g", KeyChord::char('g')),
        ("f", KeyChord::char('f')),
        ("F", KeyChord::char('F')),
        ("t", KeyChord::char('t')),
        ("T", KeyChord::char('T')),
    ] {
        if label != "g" {
            assert!(
                matches!(
                    editor.keymap.lookup(BindingMode::Visual, &[prefix]),
                    LookupResult::Partial
                ),
                "{label} must be a Partial in Visual (the CharLiteral resolves it)"
            );
        }
        assert!(
            matches!(
                editor.keymap.lookup(BindingMode::Select, &[prefix]),
                LookupResult::Unbound
            ),
            "{label} must stay Unbound in Select, so it overtypes"
        );
    }
}

/// The operator half of `nvo`. `dgg` was unbound for the same reason `vgg`
/// was, so the derivation writes those rows too.
///
/// VM.3j-2 removed the `y<C-d>` row: `<C-d>` was bound to the `j` MOTION with a
/// baked count, which is what made `y<C-d>` resolve at all. It is a scroll
/// command now, and vim composes no operator with it — `y<C-d>` and `d<C-d>`
/// do nothing there.
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
/// build where the command fired but the anchor was dropped — and the anchor
/// is the half that makes it a *selection* rather than a jump.
///
/// VM.3j-2: the second step goes through `Editor::dispatch` rather than
/// `dispatch_chord`, and that is load-bearing rather than incidental.
/// `dispatch_chord` calls `handle_action` directly, so it skips the
/// end-of-dispatch `write_through_caret` that rebuilds the document's
/// selection from `visual_anchor` + `cursor` — which is the very step this
/// test is about. It passed on the chord path only while `<C-d>` was a
/// MOTION, because a motion writes the selection inside its own application;
/// as a scroll command it relies on the write-through, exactly as `<C-f>` and
/// every other reachable cursor-mover does (see this file's header). That
/// `<C-d>` reaches the command at all is the drift test above, so nothing is
/// lost by naming the action here.
#[test]
fn a_derived_visual_motion_extends_the_selection() {
    let mut editor = Editor::boot(CoreDocument::from_text(&"line\n".repeat(60)));
    let mut partial = Vec::new();
    editor.cursor.line = 0;
    let _ = editor.dispatch_chord(KeyChord::char('v'), &mut partial);
    let anchor = editor.visual_anchor.expect("`v` arms the anchor");

    let _ = editor.dispatch(lattice_host::action::Action::HalfPageDown);

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

/// The plugin case, without a plugin: motions bound into a mode layer's Normal
/// trie, the exact shape `bind_mode_keymap` produces for org's `[[`.
///
/// `register_plugin_motion` rather than a builtin id, so the test proves the
/// mirror keys on the command's KIND and not on membership of any host-side
/// table. A fresh keymap handed the registry those motions live in, as boot
/// hands its keymap the editor's own. Each half is proved by its actual
/// mechanism: Visual and Select by the keymap at the write, `d]]` by the host
/// pass.
#[test]
fn a_mode_layer_motion_gets_its_visual_peer() {
    use lattice_grammar::command::CommandInvocation;
    use lattice_grammar::registry::{CommandRegistry, MotionResult, MotionSpec};
    use lattice_host::keymap_registry::KeymapHandle;
    use lattice_mode::ModeId;
    use std::sync::Arc;

    let spec = || MotionSpec {
        curswant: lattice_grammar::CurswantEffect::default(),
        jump: true,
        exclusive: true,
        apply: Arc::new(|ctx| {
            Ok(MotionResult {
                target: ctx.from,
                linewise: false,
                exclusive: None,
                notice: None,
            })
        }),
        args_schema: Vec::new(),
    };
    let mut registry = CommandRegistry::new();
    let builtins = lattice_grammar::builtins::populate(&mut registry);
    let headline = registry.register_plugin_motion(
        7,
        "fake-org-next-headline",
        "Move to the next headline",
        spec(),
    );
    let link =
        registry.register_plugin_motion(7, "fake-org-next-link", "Move to the next link", spec());
    let registry = Arc::new(arc_swap::ArcSwap::from_pointee(registry));

    let keymap = KeymapHandle::new();
    keymap.set_command_registry(registry.clone());

    let mode = ModeId::new("vm1-fake-org-mode");
    let layer = KeymapLayer::MajorMode(mode);
    let printable = [
        ChordPattern::Literal(KeyChord::char(']')),
        ChordPattern::Literal(KeyChord::char(']')),
    ];
    let non_printable = [ChordPattern::Literal(KeyChord::ctrl(']'))];
    for (path, motion) in [(&printable[..], headline), (&non_printable[..], link)] {
        keymap.bind(
            layer,
            BindingMode::Normal,
            path,
            CommandInvocation::of(motion.0),
            lattice_grammar::SourceLocation::plugin(7),
        );
    }

    let at = |mode_kind: BindingMode, path: &[ChordPattern]| {
        keymap
            .layer_bindings(layer, mode_kind)
            .into_iter()
            .find(|(p, _)| p.as_slice() == path)
            .map(|(_, b)| b.command.command)
    };
    assert_eq!(
        at(BindingMode::Visual, &printable),
        Some(headline.0),
        "a plugin motion must be live in Visual the moment it's bound, as the motion itself"
    );
    assert_eq!(
        at(BindingMode::Select, &printable),
        None,
        "`]]` is typed text in Select"
    );
    for mode_kind in [BindingMode::Visual, BindingMode::Select] {
        assert_eq!(
            at(mode_kind, &non_printable),
            Some(link.0),
            "a plugin motion that can't be typed must be live in {mode_kind:?}"
        );
    }

    // `d]]`: the operator half is still the host pass's job.
    lattice_host::keymap_normal::expand_grammar_rows(&keymap, &registry.load(), &builtins, layer);
    let op_path: Vec<ChordPattern> = std::iter::once(ChordPattern::Literal(KeyChord::char('d')))
        .chain(printable.iter().cloned())
        .collect();
    assert!(
        keymap
            .layer_bindings(layer, BindingMode::Normal)
            .into_iter()
            .any(|(p, _)| p == op_path),
        "a plugin motion must compose with `d`"
    );
}

/// The gap VM.4 closed, driven through the real editor rather than a bare
/// handle. A mode layer re-pushed after boot (K.1.b replaces its tries
/// wholesale) keeps its Visual motions, and its Select one where the chord
/// can't be typed, without any host pass re-running.
#[test]
fn a_re_pushed_mode_layer_keeps_its_visual_motions() {
    use lattice_grammar::command::CommandInvocation;
    use lattice_host::keymap_registry::PushLayerKind;
    use lattice_host::keymap_trie::{BoundCommand, KeymapTrie};
    use lattice_mode::ModeId;
    use std::collections::HashMap;
    use std::sync::Arc;

    let editor = boot();
    let word_forward = editor.builtins.word_forward.0;
    let mode = ModeId::new("vm4-repush-editor");
    let layer = KeymapLayer::MinorMode(mode);
    let printable = vec![
        ChordPattern::Literal(KeyChord::char('g')),
        ChordPattern::Literal(KeyChord::char('W')),
    ];
    let non_printable = vec![ChordPattern::Literal(KeyChord::ctrl(']'))];
    let tries = || {
        let mut normal = KeymapTrie::new();
        for path in [&printable, &non_printable] {
            normal.insert(
                path,
                Arc::new(BoundCommand::from_invocation(
                    CommandInvocation::of(word_forward),
                    lattice_grammar::SourceLocation::builtin_file(file!(), line!()),
                    layer,
                )),
            );
        }
        HashMap::from([(BindingMode::Normal, normal)])
    };

    editor
        .keymap
        .push_layer(PushLayerKind::MinorMode(mode), "vm4", tries());
    editor
        .keymap
        .push_layer(PushLayerKind::MinorMode(mode), "vm4", tries());

    let has = |mode_kind: BindingMode, path: &[ChordPattern]| {
        editor
            .keymap
            .layer_bindings(layer, mode_kind)
            .into_iter()
            .any(|(p, _)| p.as_slice() == path)
    };
    assert!(
        has(BindingMode::Visual, &printable),
        "a re-pushed layer must keep its Visual motion"
    );
    assert!(
        has(BindingMode::Visual, &non_printable) && has(BindingMode::Select, &non_printable),
        "a re-pushed layer must keep a non-printable motion in Visual and Select"
    );
    assert!(
        !has(BindingMode::Select, &printable),
        "`gW` is typed text in Select"
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
