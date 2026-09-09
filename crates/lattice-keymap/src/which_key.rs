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

/// Grid geometry knobs, supplied by which-key's options (§8).
#[derive(Debug, Clone, Copy)]
pub struct GridOpts {
    /// `which-key.max-columns`.
    pub max_columns: usize,
    /// `which-key.max-height`, in content rows (the caller has already
    /// applied the half-pane hard cap).
    pub max_height: usize,
}

impl Default for GridOpts {
    fn default() -> Self {
        Self {
            max_columns: 6,
            max_height: 12,
        }
    }
}

/// Cells within a row: `{key}  {label}`.
const KEY_LABEL_GAP: usize = 2;
/// Between one cell and the next.
const COLUMN_GAP: usize = 2;
/// One cell of breathing room at each edge.
const MARGIN: usize = 2;
/// A label truncated below this is noise; stop shrinking and accept
/// fewer columns instead.
const MIN_LABEL: usize = 4;
/// Below this pane width the popup is suppressed entirely (§8) — a
/// single column of truncated labels is worse than nothing.
pub const MIN_USABLE_WIDTH: usize = 20;

/// What a [`GridSpan`] covers. Deliberately semantic rather than a
/// colour: this crate has no styling dependency, and the consumer maps
/// these onto the editor's existing style vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridSpanKind {
    /// A key you would press — the emphasised column, and the header's
    /// pending prefix.
    Key,
    /// A `+N` group marker: structure, not a key.
    Group,
}

/// A styled byte range within one rendered line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSpan {
    pub start: usize,
    pub end: usize,
    pub kind: GridSpanKind,
}

/// The laid-out popup: lines to write, and where the keys are.
///
/// The spans come from the LAYOUT rather than from re-scanning the
/// rendered text, and that is the point: this function knows the byte
/// offset it wrote each key at, while a scanner would have to guess
/// which run of a padded row was a key. `magit/highlight.rs` carries a
/// note about exactly that hazard — a refs row cannot be scanned back
/// unambiguously, so its producer emits spans directly. Same rule here,
/// applied before the ambiguity can arise.
#[derive(Debug, Clone, Default)]
pub struct RenderedGrid {
    pub lines: Vec<String>,
    /// One entry per line in `lines`, same order. Empty vectors for
    /// lines with nothing to emphasise.
    pub spans: Vec<Vec<GridSpan>>,
}

impl RenderedGrid {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Lay the model out (§6). Pure: no renderer type crosses in, so both
/// the column algorithm and the span placement are unit-testable with no
/// renderer at all.
///
/// Returns header, grid rows, then any footer lines — plus the byte
/// ranges of every key. The caller writes the lines into the popup
/// buffer verbatim and hands the spans to the highlight path, so
/// everything-is-a-buffer holds and no new render model reaches either
/// peer.
///
/// Returns empty when the model has no rows or the pane is too narrow;
/// the caller suppresses the popup rather than opening an empty box.
pub fn layout_grid(model: &WhichKeyModel, width: usize, opts: GridOpts) -> RenderedGrid {
    if model.is_empty() || width < MIN_USABLE_WIDTH {
        return RenderedGrid::default();
    }
    let cells: Vec<(String, String)> = model
        .rows()
        .map(|(entry, wild)| (entry.key_text(wild), entry.label.clone()))
        .collect();

    let usable = width.saturating_sub(MARGIN);
    let key_w = cells
        .iter()
        .map(|(k, _)| display_width(k))
        .max()
        .unwrap_or(0);
    let natural_label_w = cells
        .iter()
        .map(|(_, l)| display_width(l))
        .max()
        .unwrap_or(0);

    // How many columns fit at a given label width.
    let columns_at = |label_w: usize| -> usize {
        let cell = key_w + KEY_LABEL_GAP + label_w;
        ((usable + COLUMN_GAP) / (cell + COLUMN_GAP)).clamp(1, opts.max_columns.max(1))
    };

    // Elastic truncation (§6): when the natural width yields fewer than
    // two columns, shrink LABELS until two fit — then stop. A key is
    // never truncated: a wrong key is worse than a missing label.
    let mut label_w = natural_label_w;
    if cells.len() > 1 && columns_at(label_w) < 2 {
        // Width available to one label when two cells share the row.
        let per_cell = (usable + COLUMN_GAP) / 2;
        let shrunk = per_cell
            .saturating_sub(COLUMN_GAP)
            .saturating_sub(key_w + KEY_LABEL_GAP);
        if shrunk >= MIN_LABEL {
            label_w = shrunk;
        }
    }

    let columns = columns_at(label_w);
    let rows_needed = cells.len().div_ceil(columns);
    let rows = rows_needed.min(opts.max_height.max(1));
    let capacity = rows * columns;
    let truncated = cells.len().saturating_sub(capacity);
    let shown = &cells[..cells.len().min(capacity)];

    let mut out = Vec::with_capacity(rows + 3);
    let mut spans: Vec<Vec<GridSpan>> = Vec::with_capacity(rows + 3);
    // The header IS the pending prefix — the keys you have already
    // pressed — so it is emphasised for the same reason the key column is.
    let header = model.header();
    spans.push(vec![GridSpan {
        start: 0,
        end: header.len(),
        kind: GridSpanKind::Key,
    }]);
    out.push(header);

    for r in 0..rows {
        let mut line = String::new();
        let mut row_spans: Vec<GridSpan> = Vec::new();
        // COLUMN-MAJOR fill: down, then across. Row-major would place
        // `a b c` across the top and `d e f` on row two, defeating a
        // scan for a letter in a sorted list — `ls` and emacs
        // `which-key` fill column-major for the same reason.
        for c in 0..columns {
            let Some((key, label)) = shown.get(c * rows + r) else {
                continue;
            };
            if !line.is_empty() {
                line.push_str(&" ".repeat(COLUMN_GAP));
            }
            let label = truncate_to(label, label_w);
            // Byte offsets, captured as the row is built — `key` may be
            // multi-byte (`{char}`, a special-key name) and the padding
            // that follows must not be inside the span.
            let key_start = line.len();
            line.push_str(&pad_to(key, key_w));
            row_spans.push(GridSpan {
                start: key_start,
                end: key_start + key.len(),
                kind: GridSpanKind::Key,
            });
            line.push_str(&" ".repeat(KEY_LABEL_GAP));
            let label_start = line.len();
            line.push_str(&pad_to(&label, label_w));
            // `+N` is a group marker, not a command name: dim structure
            // rather than another key.
            if label.starts_with('+') {
                row_spans.push(GridSpan {
                    start: label_start,
                    end: label_start + label.len(),
                    kind: GridSpanKind::Group,
                });
            }
        }
        // Trailing padding is trimmed; no span can point past the line
        // because every span ends at content, never at padding.
        out.push(line.trim_end().to_string());
        spans.push(row_spans);
    }

    if truncated > 0 {
        out.push(format!("+{truncated} more"));
        spans.push(Vec::new());
    }
    if let Some(label) = &model.terminal_label {
        // The prefix is bound on its own (vim's `d`). A footer note, not
        // a row: pressing nothing more is not a "next key".
        let header = model.header();
        out.push(format!("{header} alone: {label}"));
        spans.push(vec![GridSpan {
            start: 0,
            end: header.len(),
            kind: GridSpanKind::Key,
        }]);
    }
    debug_assert_eq!(out.len(), spans.len(), "one span row per rendered line");
    RenderedGrid { lines: out, spans }
}

fn display_width(s: &str) -> usize {
    // Keys and labels are chord notation and docstrings; the codebase
    // has no unicode-width dependency at this layer, and a chars count
    // is exact for both. Revisit if labels ever carry CJK.
    s.chars().count()
}

fn pad_to(s: &str, w: usize) -> String {
    let mut out = s.to_string();
    for _ in display_width(s)..w {
        out.push(' ');
    }
    out
}

/// Truncate with an ellipsis, never past the ellipsis itself.
fn truncate_to(s: &str, w: usize) -> String {
    if display_width(s) <= w || w == 0 {
        return s.to_string();
    }
    let keep = w.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
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

    // ---- WK.2: the grid (design §6) ---------------------------------

    /// A model of `n` rows with predictable keys and labels.
    fn model_of(n: usize, label: &str) -> WhichKeyModel {
        let entries = (0..n)
            .map(|i| Entry {
                chord: KeyChord::char((b'a' + (i as u8 % 26)) as char),
                label: format!("{label}{i}"),
                kind: EntryKind::Terminal,
                layer: Some(KeymapLayer::Builtin),
            })
            .collect();
        WhichKeyModel {
            prefix: vec![press('g')],
            mode: BindingMode::Normal,
            entries,
            wildcard: None,
            terminal_label: None,
        }
    }

    /// Grid rows only — header and footers stripped.
    fn grid_rows(grid: &RenderedGrid) -> Vec<String> {
        grid.lines
            .iter()
            .skip(1)
            .filter(|l| !l.starts_with('+') && !l.contains(" alone: "))
            .cloned()
            .collect()
    }

    #[test]
    fn column_count_scales_with_width() {
        let model = model_of(24, "cmd");
        let cols_at = |w: usize| {
            let grid = layout_grid(&model, w, GridOpts::default());
            let rows = grid_rows(&grid);
            // Columns = ceil(n / rows) given every row is full but the last.
            24_usize.div_ceil(rows.len())
        };
        let (c40, c80, c120, c200) = (cols_at(40), cols_at(80), cols_at(120), cols_at(200));
        assert!(
            c40 <= c80 && c80 <= c120 && c120 <= c200,
            "columns must be monotonic in width: {c40} {c80} {c120} {c200}"
        );
        assert!(c40 >= 1 && c200 <= GridOpts::default().max_columns);
    }

    #[test]
    fn fill_is_column_major_so_a_sorted_scan_reads_down() {
        // 6 entries, a width that yields exactly 2 columns → 3 rows.
        let model = model_of(6, "x");
        let opts = GridOpts {
            max_columns: 2,
            max_height: 12,
        };
        let grid = layout_grid(&model, 40, opts);
        let rows = grid_rows(&grid);
        assert_eq!(rows.len(), 3, "6 entries / 2 columns");
        // Column-major: a b c fill column ONE (rows 0,1,2); d e f fill
        // column two. Row-major would put `a b` on the first row.
        assert!(rows[0].starts_with('a'), "row 0 col 0 is the first entry");
        assert!(rows[1].starts_with('b'), "row 1 col 0 is the SECOND entry");
        assert!(rows[2].starts_with('c'));
        assert!(
            rows[0].contains('d'),
            "the second column starts at the 4th entry, not the 2nd: {:?}",
            rows[0]
        );
    }

    #[test]
    fn labels_truncate_to_reach_two_columns_but_keys_never_do() {
        let mut model = model_of(4, "");
        for (i, e) in model.entries.iter_mut().enumerate() {
            e.label = format!("an extremely long description number {i}");
        }
        let grid = layout_grid(&model, 60, GridOpts::default());
        let rows = grid_rows(&grid);
        assert!(
            rows.iter().any(|r| r.contains('…')),
            "labels shrink so a second column fits: {rows:?}"
        );
        assert_eq!(rows.len(), 2, "4 entries in 2 columns");
        for (i, row) in rows.iter().enumerate() {
            let key = (b'a' + i as u8) as char;
            assert!(
                row.starts_with(key),
                "the key column is never truncated — a wrong key is worse \
                 than a missing label: {row:?}"
            );
        }
    }

    #[test]
    fn a_label_is_not_shrunk_below_the_floor() {
        let mut model = model_of(2, "");
        // Wide keys plus a narrow pane: two columns would leave ~1 char
        // for each label, which is the case the floor exists for.
        for (i, e) in model.entries.iter_mut().enumerate() {
            e.chord = KeyChord::ctrl((b'x' + i as u8) as char);
            e.label = "a very long label indeed".to_string();
        }
        let grid = layout_grid(&model, MIN_USABLE_WIDTH, GridOpts::default());
        let rows = grid_rows(&grid);
        assert_eq!(
            rows.len(),
            2,
            "one column, readable labels — better than two columns of \
             unreadable stubs: {rows:?}"
        );
        assert!(
            rows[0].starts_with("<C-x>"),
            "the key survives intact: {rows:?}"
        );
    }

    #[test]
    fn overflow_becomes_a_plus_n_more_tail_not_a_scrollbar() {
        let model = model_of(40, "cmd");
        let opts = GridOpts {
            max_columns: 2,
            max_height: 4,
        };
        let grid = layout_grid(&model, 80, opts);
        let rows = grid_rows(&grid);
        assert_eq!(rows.len(), 4, "capped at max_height");
        let tail = grid.lines.last().expect("a tail line");
        assert_eq!(
            tail, "+32 more",
            "40 entries, 4 rows × 2 columns shown: {:?}",
            grid.lines
        );
    }

    #[test]
    fn a_bound_prefix_is_a_footer_note_not_a_row() {
        let mut model = model_of(3, "cmd");
        model.terminal_label = Some("delete (operator)".to_string());
        let grid = layout_grid(&model, 80, GridOpts::default());
        assert_eq!(
            grid.lines.last().map(String::as_str),
            Some("g alone: delete (operator)"),
            "pressing nothing more is not a 'next key': {:?}",
            grid.lines
        );
        assert_eq!(grid_rows(&grid).len(), 1, "3 entries still fit one row");
    }

    #[test]
    fn the_header_names_the_pending_prefix() {
        let model = model_of(2, "cmd");
        let grid = layout_grid(&model, 80, GridOpts::default());
        assert_eq!(grid.lines[0], "g", "the prefix, in vim notation");
    }

    // ---- WK.9: the spans that make the keys legible -----------------

    /// Every key gets a span, and the span covers the KEY only — not the
    /// padding that aligns the column. A span that ran to the column
    /// width would paint the gap between key and label.
    #[test]
    fn every_key_is_spanned_and_the_padding_is_not() {
        let mut model = model_of(3, "cmd");
        model.entries[0].chord = KeyChord::ctrl('x'); // a WIDE key
        let grid = layout_grid(&model, 100, GridOpts::default());

        // Row 1 is the first grid row (row 0 is the header).
        let row = &grid.lines[1];
        let row_spans = &grid.spans[1];
        assert_eq!(row_spans.len(), 3, "one span per cell in the row");
        for span in row_spans {
            assert_eq!(span.kind, GridSpanKind::Key);
            let text = &row[span.start..span.end];
            assert!(
                !text.starts_with(' ') && !text.ends_with(' '),
                "a key span must cover the key, not its alignment padding: \
                 {text:?} in {row:?}"
            );
        }
        assert_eq!(&row[row_spans[0].start..row_spans[0].end], "<C-x>");
    }

    /// The header is the keys you have already pressed, so it is
    /// emphasised the same way.
    #[test]
    fn the_header_prefix_is_spanned_as_a_key() {
        let grid = layout_grid(&model_of(2, "cmd"), 80, GridOpts::default());
        assert_eq!(
            grid.spans[0],
            vec![GridSpan {
                start: 0,
                end: 1,
                kind: GridSpanKind::Key
            }],
        );
    }

    /// `+N` is structure, not a key — a distinct kind so a theme can dim
    /// it rather than making a group look pressable.
    #[test]
    fn a_group_marker_is_spanned_as_a_group() {
        let mut model = model_of(1, "");
        model.entries[0].label = "+4".to_string();
        model.entries[0].kind = EntryKind::Prefix(4);
        let grid = layout_grid(&model, 80, GridOpts::default());
        let kinds: Vec<_> = grid.spans[1].iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![GridSpanKind::Key, GridSpanKind::Group]);
    }

    /// A label that merely BEGINS with a plus is not a group marker's
    /// business, but it is indistinguishable from one by text alone —
    /// which is why the kind is decided at layout time from the entry,
    /// not recovered by scanning. Pinning the invariant that matters:
    /// spans never point past their line.
    #[test]
    fn no_span_points_past_its_line() {
        for width in [20, 40, 80, 120, 200] {
            let mut model = model_of(12, "a longer command label");
            model.wildcard = Some(Entry {
                chord: KeyChord::char('\0'),
                label: "find char".to_string(),
                kind: EntryKind::Terminal,
                layer: None,
            });
            model.terminal_label = Some("operator".to_string());
            let grid = layout_grid(&model, width, GridOpts::default());
            for (line, spans) in grid.lines.iter().zip(&grid.spans) {
                for s in spans {
                    assert!(
                        s.end <= line.len()
                            && line.is_char_boundary(s.start)
                            && line.is_char_boundary(s.end),
                        "span {s:?} out of range for {line:?} at width {width}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_pane_too_narrow_suppresses_the_grid_entirely() {
        let model = model_of(6, "cmd");
        assert!(
            layout_grid(&model, MIN_USABLE_WIDTH - 1, GridOpts::default()).is_empty(),
            "a single column of truncated labels is worse than nothing"
        );
    }

    #[test]
    fn an_empty_model_renders_nothing() {
        let model = model_of(0, "cmd");
        assert!(layout_grid(&model, 80, GridOpts::default()).is_empty());
    }

    #[test]
    fn the_wildcard_row_appears_in_the_grid() {
        let mut model = model_of(1, "cmd");
        model.wildcard = Some(Entry {
            chord: KeyChord::char('\0'),
            label: "find char forward".to_string(),
            kind: EntryKind::Terminal,
            layer: Some(KeymapLayer::Builtin),
        });
        let grid = layout_grid(&model, 80, GridOpts::default());
        assert!(
            grid.lines.iter().any(|l| l.contains("{char}")),
            "the wildcard renders as a `{{char}}` row: {:?}",
            grid.lines
        );
    }
}
