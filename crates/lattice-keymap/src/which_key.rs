//! Which-key's resolver — prefix → continuation model.
//!
//! Design: `docs/dev/architecture/which-key.md` §4 (data model), §4.1
//! (label resolution), §4.2 (ordering). Sequencing:
//! `docs/dev/operations/slice-plans/which-key.md` (WK.1).
//!
//! Pure and synchronous. Everything here is a function of a
//! [`NodeView`](crate::trie::NodeView) plus the command registry — no
//! editor state, no renderer type, no I/O — so the whole model is
//! unit-testable with a hand-built trie and no host at all.
//!
//! **Why this lives in `lattice-keymap`.** Heuristic #6: which-key
//! *extends* the keymap rather than introducing a new mechanism, so it
//! belongs to the crate that already owns the trie, the layers and the
//! resolution. And per the substrate-vs-mode-helper rule, the resolver's
//! only consumer is which-key's own handler — so it is a helper function
//! in the owning crate, not host machinery and not a trait method.

use std::sync::Arc;

use lattice_grammar::CommandRegistry;
use lattice_protocol::KeyChord;

use crate::trie::{BoundCommand, ChildView, NodeView};
use crate::{BindingMode, KeymapLayer};

/// What a row's key leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A binding fires on this key.
    Terminal,
    /// The key opens a deeper prefix, carrying `N` bindings beneath it.
    /// Rendered `+N`, which-key.nvim's convention for an unlabelled
    /// group — which is why prefix labels can be deferred without
    /// structural cost: the count already lives on the entry, and a
    /// label slot can be filled later.
    Prefix(usize),
}

/// One row of the popup.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The key to press next. Rendered through `Display for KeyChord`,
    /// except the wildcard row, which renders `{char}`.
    pub chord: KeyChord,
    /// Never blank — the label chain is total by construction (§4.1).
    pub label: String,
    pub kind: EntryKind,
    /// Which layer the binding came from. Carried for provenance in
    /// tests and future per-layer styling; not rendered in v1.
    pub layer: Option<KeymapLayer>,
}

impl Entry {
    /// The key as the grid renders it. The wildcard row has no real
    /// chord, so it renders `{char}` — the spelling `:keymap` and
    /// `:describe-key` already use for `f{char}` / `'{mark}`.
    pub fn key_text(&self, wildcard: bool) -> String {
        if wildcard {
            "{char}".to_string()
        } else {
            self.chord.to_string()
        }
    }
}

/// Row ordering (§4.2). An explicit total order is required either way,
/// because a node's children live in a `HashMap` — without one the grid
/// reshuffles between openings of the same prefix, which is a worse
/// discoverability surface than no popup at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// Collate by key: digits, lowercase, uppercase, punctuation,
    /// special keys, then modifier-bearing chords.
    #[default]
    Key,
    /// Collate by label, with the key collation as the tiebreak.
    Label,
}

impl Sort {
    /// Parse the `which-key.sort` option value. Unknown values fall back
    /// to `Key` — log-and-skip, never panic (§8).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "key" => Some(Sort::Key),
            "label" => Some(Sort::Label),
            _ => None,
        }
    }
}

/// The popup's content, before layout.
#[derive(Debug, Clone)]
pub struct WhichKeyModel {
    /// The chords already pressed, for the header.
    pub prefix: Vec<KeyChord>,
    /// The binding mode the continuations were resolved in.
    pub mode: BindingMode,
    /// Rows, already collated.
    pub entries: Vec<Entry>,
    /// The `{char}` row, if the node has a wildcard descent. Held apart
    /// so it can be rendered last regardless of collation — a wildcard
    /// matches *any* key, so sorting it among specific keys would imply
    /// an ordering it does not have.
    pub wildcard: Option<Entry>,
    /// Set when the prefix node is ALSO bound (vim's `d`: an operator
    /// and a prefix). Carries the bound command's label for the footer,
    /// rather than a row — pressing nothing more is not a "next key".
    pub terminal_label: Option<String>,
}

impl WhichKeyModel {
    /// Rows in render order: collated entries, wildcard last.
    pub fn rows(&self) -> impl Iterator<Item = (&Entry, bool)> {
        self.entries
            .iter()
            .map(|e| (e, false))
            .chain(self.wildcard.iter().map(|e| (e, true)))
    }

    /// Total row count including the wildcard.
    pub fn len(&self) -> usize {
        self.entries.len() + usize::from(self.wildcard.is_some())
    }

    /// No rows at all — the caller suppresses the popup rather than
    /// rendering an empty box (§8).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The header: the prefix in vim notation.
    pub fn header(&self) -> String {
        self.prefix.iter().map(|c| c.to_string()).collect()
    }
}

/// Build the popup model from a resolved [`NodeView`].
///
/// `registry` supplies rungs 2 and 3 of the label chain; pass the live
/// `CommandRegistry`. `mode` is only carried through for the static
/// catalog lookup and the model's own field.
pub fn build_model(
    node: NodeView,
    prefix: &[KeyChord],
    mode: BindingMode,
    registry: &CommandRegistry,
    sort: Sort,
) -> WhichKeyModel {
    let mut entries: Vec<Entry> = node
        .children
        .iter()
        .map(|child| entry_for(child, prefix, mode, registry, false))
        .collect();

    sort_entries(&mut entries, sort);

    let wildcard = node
        .wildcard
        .as_ref()
        .map(|child| entry_for(child, prefix, mode, registry, true));

    let terminal_label = node
        .terminal
        .as_ref()
        .map(|bound| label_for(bound, prefix, mode, registry));

    WhichKeyModel {
        prefix: prefix.to_vec(),
        mode,
        entries,
        wildcard,
        terminal_label,
    }
}

fn entry_for(
    child: &ChildView,
    prefix: &[KeyChord],
    mode: BindingMode,
    registry: &CommandRegistry,
    wildcard: bool,
) -> Entry {
    let mut full_path: Vec<KeyChord> = prefix.to_vec();
    if !wildcard {
        full_path.push(child.chord);
    }
    match &child.binding {
        // A bound child is a row that fires. When it ALSO has a subtree
        // beneath it, that subtree is unreachable (the trie stops at the
        // first binding), so it is not counted here — reporting `+N` for
        // chords that can never fire would be a lie in the one surface
        // whose whole job is telling the truth about what comes next.
        Some(bound) => Entry {
            chord: child.chord,
            label: label_for(bound, &full_path, mode, registry),
            kind: EntryKind::Terminal,
            layer: Some(bound.layer),
        },
        // An unbound child is a group. The wildcard case resolves its
        // label from the wildcard SUBTREE's binding, so `f` renders one
        // `{char}  find char forward` row rather than an empty grid —
        // which would read as a broken popup (§4.1).
        None => Entry {
            chord: child.chord,
            label: format!("+{}", child.descendants),
            kind: EntryKind::Prefix(child.descendants),
            layer: None,
        },
    }
}

/// The four-rung label chain (§4.1), first hit wins. **Total by
/// construction** — the last rung always produces a string, so a label
/// is never blank and the path never panics.
fn label_for(
    bound: &Arc<BoundCommand>,
    full_path: &[KeyChord],
    mode: BindingMode,
    registry: &CommandRegistry,
) -> String {
    let chord_text: String = full_path.iter().map(|c| c.to_string()).collect();
    // 1. The curated one-liner from the static catalog, matched on the
    //    full chord path AND the binding mode.
    if let Some(entry) = crate::keymap_entry::lookup(&chord_text)
        .into_iter()
        .find(|e| e.modes.contains(&mode))
        && !entry.doc.is_empty()
    {
        return entry.doc.to_string();
    }
    let id = bound.command.command;
    // 2. the registry's doc for the bound command, then 3. its name.
    if let Some(spec) = registry.lookup(id) {
        if !spec.doc.is_empty() {
            return spec.doc.clone();
        }
        if !spec.name.is_empty() {
            return spec.name.clone();
        }
    }
    // 4. The terminal rung. A `CommandId` missing from the registry is a
    //    real possibility (a plugin unloaded between bind and render), and
    //    it must not blank the row.
    "<unbound>".to_string()
}

/// §4.2's collation. Applied to the entry list in place.
fn sort_entries(entries: &mut [Entry], sort: Sort) {
    match sort {
        Sort::Key => entries.sort_by(|a, b| key_order(&a.chord).cmp(&key_order(&b.chord))),
        Sort::Label => entries.sort_by(|a, b| {
            a.label
                .cmp(&b.label)
                .then_with(|| key_order(&a.chord).cmp(&key_order(&b.chord)))
        }),
    }
}

/// A chord's sort position: digits, lowercase, uppercase, punctuation,
/// special keys, then modifier-bearing chords. Returned as a tuple so
/// the derived `Ord` does the work; the second element keeps the order
/// within a class stable and total.
fn key_order(chord: &KeyChord) -> (u8, String) {
    use lattice_protocol::KeyKind;
    let text = chord.to_string();
    if !chord.mods.is_empty() {
        return (5, text);
    }
    let class = match chord.key {
        KeyKind::Char(c) if c.is_ascii_digit() => 0,
        KeyKind::Char(c) if c.is_lowercase() => 1,
        KeyKind::Char(c) if c.is_uppercase() => 2,
        KeyKind::Char(_) => 3,
        KeyKind::Special(_) => 4,
    };
    (class, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trie::{KeymapLayer, KeymapTrie};
    use crate::{ChordPattern, ModeId};
    use lattice_grammar::{CommandInvocation, SourceLocation};
    use lattice_protocol::ids::CommandId;

    fn bound(id: u64, layer: KeymapLayer) -> Arc<BoundCommand> {
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(CommandId::new(id)),
            SourceLocation::synthetic("test"),
            layer,
        ))
    }

    fn lit(c: char) -> ChordPattern {
        ChordPattern::Literal(KeyChord::char(c))
    }

    fn press(c: char) -> KeyChord {
        KeyChord::char(c)
    }

    /// A trie with `gd`, `gr` (bound) and `gs{a,b}` (a group).
    fn g_trie() -> KeymapTrie {
        let mut t = KeymapTrie::new();
        t.insert(&[lit('g'), lit('d')], bound(1, KeymapLayer::Builtin));
        t.insert(&[lit('g'), lit('r')], bound(2, KeymapLayer::Builtin));
        t.insert(
            &[lit('g'), lit('s'), lit('a')],
            bound(3, KeymapLayer::Builtin),
        );
        t.insert(
            &[lit('g'), lit('s'), lit('b')],
            bound(4, KeymapLayer::Builtin),
        );
        t
    }

    #[test]
    fn node_view_reports_children_and_group_counts() {
        let view = g_trie().node_view(&[press('g')]).expect("g is a prefix");
        assert_eq!(view.children.len(), 3, "d, r, s");
        let by_chord = |c: char| {
            view.children
                .iter()
                .find(|ch| ch.chord == press(c))
                .expect("child present")
        };
        assert!(by_chord('d').binding.is_some(), "gd is bound");
        assert_eq!(by_chord('d').descendants, 0);
        assert!(by_chord('s').binding.is_none(), "gs is a group");
        assert_eq!(by_chord('s').descendants, 2, "gsa + gsb");
        assert!(view.terminal.is_none(), "g itself is not bound");
        assert!(view.wildcard.is_none());
    }

    #[test]
    fn node_view_reports_a_prefix_that_is_also_bound() {
        let mut t = g_trie();
        t.insert(&[lit('g')], bound(9, KeymapLayer::Builtin));
        let view = t.node_view(&[press('g')]).expect("still a node");
        assert!(
            view.terminal.is_some(),
            "a bound prefix is reported for the footer, not as a row"
        );
        assert_eq!(view.children.len(), 3, "…and its children still listed");
    }

    #[test]
    fn node_view_is_none_for_an_unknown_prefix() {
        assert!(g_trie().node_view(&[press('q')]).is_none());
    }

    #[test]
    fn node_view_reports_the_wildcard_descent() {
        let mut t = KeymapTrie::new();
        t.insert(
            &[lit('f'), ChordPattern::CharLiteral],
            bound(1, KeymapLayer::Builtin),
        );
        let view = t.node_view(&[press('f')]).expect("f is a prefix");
        assert!(view.children.is_empty());
        let wild = view.wildcard.expect("wildcard present");
        assert!(
            wild.binding.is_some(),
            "the label comes from the wildcard subtree's binding — without \
             it `f` would render an empty grid, which reads as a broken popup"
        );
    }

    #[test]
    fn a_group_entry_carries_its_count() {
        let view = g_trie().node_view(&[press('g')]).unwrap();
        let model = build_model(
            view,
            &[press('g')],
            BindingMode::Normal,
            &CommandRegistry::new(),
            Sort::Key,
        );
        let s = model
            .entries
            .iter()
            .find(|e| e.chord == press('s'))
            .expect("gs row");
        assert_eq!(s.kind, EntryKind::Prefix(2));
        assert_eq!(s.label, "+2", "which-key.nvim's unlabelled-group form");
    }

    /// The label chain's terminal rung. A `CommandId` the registry does
    /// not know must not blank the row.
    #[test]
    fn the_label_chain_is_total() {
        let mut t = KeymapTrie::new();
        t.insert(&[lit('z'), lit('q')], bound(0xDEAD, KeymapLayer::Builtin));
        let view = t.node_view(&[press('z')]).unwrap();
        let model = build_model(
            view,
            &[press('z')],
            BindingMode::Normal,
            &CommandRegistry::new(),
            Sort::Key,
        );
        assert_eq!(model.entries[0].label, "<unbound>");
        assert!(
            !model.entries[0].label.is_empty(),
            "a label is never blank — the chain's last rung always produces one"
        );
    }

    #[test]
    fn the_registry_supplies_the_label_when_the_catalog_does_not() {
        let mut registry = CommandRegistry::new();
        let id = registry.register_action(
            "action:test-jump-somewhere",
            "Jump somewhere",
            lattice_grammar::registry::ActionSpec {
                apply: Arc::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
            },
        );
        let mut t = KeymapTrie::new();
        t.insert(
            &[lit('z'), lit('q')],
            Arc::new(BoundCommand::from_invocation(
                CommandInvocation::of(id),
                SourceLocation::synthetic("test"),
                KeymapLayer::Builtin,
            )),
        );
        let view = t.node_view(&[press('z')]).unwrap();
        let model = build_model(
            view,
            &[press('z')],
            BindingMode::Normal,
            &registry,
            Sort::Key,
        );
        assert_eq!(model.entries[0].label, "Jump somewhere");
    }

    /// The collation must be TOTAL and stable, because `children` is a
    /// `HashMap`: without it the grid reshuffles between openings of the
    /// same prefix, which is worse than no popup at all.
    #[test]
    fn collation_is_stable_across_rebuilds() {
        let order_of = || {
            let mut t = KeymapTrie::new();
            for c in ['b', 'A', '2', 'a', '-', 'B'] {
                t.insert(&[lit('g'), lit(c)], bound(1, KeymapLayer::Builtin));
            }
            t.insert(
                &[lit('g'), ChordPattern::Literal(KeyChord::ctrl('x'))],
                bound(1, KeymapLayer::Builtin),
            );
            let view = t.node_view(&[press('g')]).unwrap();
            let model = build_model(
                view,
                &[press('g')],
                BindingMode::Normal,
                &CommandRegistry::new(),
                Sort::Key,
            );
            model
                .entries
                .iter()
                .map(|e| e.chord.to_string())
                .collect::<Vec<_>>()
        };
        let first = order_of();
        assert_eq!(
            first,
            vec!["2", "a", "b", "A", "B", "-", "<C-x>"],
            "digits, lowercase, uppercase, punctuation, then modifier-bearing"
        );
        for _ in 0..8 {
            assert_eq!(order_of(), first, "order must not depend on HashMap order");
        }
    }

    #[test]
    fn sort_by_label_falls_back_to_key_order() {
        let mut t = KeymapTrie::new();
        t.insert(&[lit('g'), lit('b')], bound(1, KeymapLayer::Builtin));
        t.insert(&[lit('g'), lit('a')], bound(1, KeymapLayer::Builtin));
        let view = t.node_view(&[press('g')]).unwrap();
        let model = build_model(
            view,
            &[press('g')],
            BindingMode::Normal,
            &CommandRegistry::new(),
            Sort::Label,
        );
        // Both labels are `<unbound>`, so the key collation decides.
        assert_eq!(
            model
                .entries
                .iter()
                .map(|e| e.chord.to_string())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn the_wildcard_row_renders_as_char_and_sorts_last() {
        let mut t = KeymapTrie::new();
        t.insert(&[lit('f'), lit('z')], bound(1, KeymapLayer::Builtin));
        t.insert(
            &[lit('f'), ChordPattern::CharLiteral],
            bound(2, KeymapLayer::Builtin),
        );
        let view = t.node_view(&[press('f')]).unwrap();
        let model = build_model(
            view,
            &[press('f')],
            BindingMode::Normal,
            &CommandRegistry::new(),
            Sort::Key,
        );
        let rendered: Vec<String> = model
            .rows()
            .map(|(e, wild)| e.key_text(wild))
            .collect::<Vec<_>>();
        assert_eq!(
            rendered,
            vec!["z", "{char}"],
            "a wildcard matches any key, so it renders last rather than \
             collated among specific keys"
        );
    }

    #[test]
    fn header_renders_the_prefix_in_vim_notation() {
        let model = build_model(
            g_trie().node_view(&[press('g')]).unwrap(),
            &[press('g')],
            BindingMode::Normal,
            &CommandRegistry::new(),
            Sort::Key,
        );
        assert_eq!(model.header(), "g");
        assert!(!model.is_empty());
    }

    // ---- The one correctness property (design §2) -------------------
    //
    // These go through `KeymapHandle::continuations_with_context` rather
    // than a bare trie, because the property under test is the FOLD.

    fn handle_with_shadowing_minor() -> (crate::KeymapHandle, ModeId) {
        use crate::PushLayerKind;
        use std::collections::HashMap;

        let h = crate::KeymapHandle::new();
        // Builtin `gd` → command 1.
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit('g'), lit('d')],
            CommandInvocation::of(CommandId::new(1)),
            SourceLocation::synthetic("builtin"),
        );
        // A minor mode shadows `gd` with its own command, and adds `gx`.
        let mode = ModeId::new("shadowing-mode");
        let mut trie = KeymapTrie::new();
        trie.insert(
            &[lit('g'), lit('d')],
            bound(2, KeymapLayer::MinorMode(mode)),
        );
        trie.insert(
            &[lit('g'), lit('x')],
            bound(3, KeymapLayer::MinorMode(mode)),
        );
        let mut bindings = HashMap::new();
        bindings.insert(BindingMode::Normal, trie);
        h.push_layer(PushLayerKind::MinorMode(mode), "shadowing-mode", bindings);
        (h, mode)
    }

    /// The regression test design §2 names explicitly: activate a mode
    /// that shadows a builtin chord, and assert the popup shows the
    /// MODE's binding and that the builtin does not also appear.
    #[test]
    fn a_shadowing_minor_wins_and_the_builtin_does_not_also_appear() {
        let (h, mode) = handle_with_shadowing_minor();
        let view = h
            .continuations_with_context(BindingMode::Normal, &[press('g')], &[mode])
            .expect("g is a prefix in the composite");
        let gd = view
            .children
            .iter()
            .find(|c| c.chord == press('d'))
            .expect("gd row");
        assert_eq!(
            gd.binding.as_ref().map(|b| b.layer),
            Some(KeymapLayer::MinorMode(mode)),
            "the composite's winner is the mode's binding, not the builtin's"
        );
        assert_eq!(
            view.children.len(),
            2,
            "one row per next key — the shadowed builtin is not a second `d` row"
        );
    }

    /// …and with the mode inactive, its chords are absent entirely.
    #[test]
    fn an_inactive_mode_contributes_nothing() {
        let (h, _mode) = handle_with_shadowing_minor();
        let view = h
            .continuations_with_context(BindingMode::Normal, &[press('g')], &[])
            .expect("builtin g still resolves");
        assert_eq!(view.children.len(), 1, "only the builtin `gd`");
        assert_eq!(
            view.children[0].binding.as_ref().map(|b| b.layer),
            Some(KeymapLayer::Builtin)
        );
    }

    #[test]
    fn continuations_with_context_is_none_for_an_unknown_prefix() {
        let (h, mode) = handle_with_shadowing_minor();
        assert!(
            h.continuations_with_context(BindingMode::Normal, &[press('q')], &[mode])
                .is_none()
        );
    }
}
