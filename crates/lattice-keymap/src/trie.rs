//! `KeymapTrie` -- the lookup data structure the keymap registry
//! consults on the keystroke path. Audit slice 8.b of the M3
//! refactor; see `docs/dev/architecture/keymap-architecture.md` for the design.
//!
//! ## Shape
//!
//! Each `TrieNode` carries:
//!
//! - `children: HashMap<KeyChord, TrieNode>` -- exact-match
//!   descents indexed by chord. `gd`'s second descent comes
//!   from `children[KeyChord::char('d')]`.
//! - `char_wildcard: Option<Box<TrieNode>>` -- "any single
//!   printable char" descent. Used for marks (`'a`), registers
//!   (`"a`), find-char (`fX` / `FX` / `tX` / `TX`), macro
//!   names (`@a` / `qa`). Subsumes today's
//!   `BindingMode::AfterMark` / `AfterRegister` / `AfterFindChar`
//!   special-case states.
//! - `binding: Option<Arc<BoundCommand>>` -- terminal binding at
//!   this depth, if any. Internal nodes hold `None`.
//!
//! Lookup walks chord-by-chord. Exact `children` match wins; if
//! absent, fall back to `char_wildcard` (capturing the matched
//! char). Returns:
//!
//! - `Bound` when the walk ends at a node with a terminal
//!   binding (and the input is exhausted).
//! - `Partial` when the walk ends at an internal node with no
//!   terminal but with children -- caller waits for the next
//!   chord.
//! - `Unbound` when no descent matches at some point.
//!
//! ## Performance
//!
//! Lookup is `O(prefix_length)` `HashMap` lookups. With ~500
//! bindings per layer and chord depths of 1-3, the inner cost
//! is two `HashMap::get` calls plus a few branches; bench
//! `keymap_trie_lookup_*` rows in `BENCHMARKS.md` measure it.
//! Allocation-free on the hot path -- the `Vec<char>` of
//! captured wildcards is built only when the path actually
//! crosses a wildcard, which is rare.
//!
//! ## Mutation
//!
//! `insert` / `remove` / `merge_over` are off the hot path
//! (registry construction, layer push/pop, `:bind` invocations).
//! They take `&mut self`; the registry handle (slice 8.c) wraps
//! the trie in `Arc<ArcSwap<KeymapTrie>>` so wait-free reads
//! coexist with these `&mut`-bound mutations -- the registry
//! builds a new trie, swaps the cell, and the old trie drops
//! once readers release their `Arc`.

use std::collections::HashMap;
use std::sync::Arc;

use lattice_grammar::{CommandInvocation, SourceLocation};
use crate::ModeId;

// K.2.1: `ChordPattern` moved to `lattice-protocol` alongside
// `KeyChord` so mode crates can construct registration paths
// without depending on `lattice-host`. Re-exported below for
// the existing `use crate::keymap_trie::ChordPattern` callers
// (`keymap_registry.rs`, `keymap_replace.rs`,
// `multibuffer_keymap.rs`). The matcher engine
// (`KeymapTrie`, `KeymapLayer`, `BoundCommand`) now lives in `lattice-keymap::trie`.
pub use lattice_protocol::ChordPattern;

use lattice_protocol::{KeyChord, KeyKind, KeyMods};

/// Where in the five-layer model (DESIGN.md §5.2.3) a binding
/// originated. Higher value wins on cross-layer conflict; the
/// trie itself doesn't enforce this, the registry does at merge
/// time.
///
/// K.1.b (2026-05-30): `MinorMode` now carries a typed
/// [`ModeId`] instead of an opaque `u32`. The layer's
/// identity = the mode's identity; one layer per mode (not
/// per-push). A re-push for the same `ModeId` replaces the
/// layer's bindings rather than minting a new layer.
/// `OwnedLayer` capability keys off `ModeId` so user /
/// plugin bindings targeting a specific mode's keymap go
/// into that mode's layer and live + die with the mode's
/// activation lifecycle (matching emacs's `(:map foo-mode-map ...)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeymapLayer {
    /// Built-in vim default keymap. Lowest priority; user /
    /// plugin bindings shadow these.
    Builtin,
    /// Major-mode keymap (rust, markdown, ...).
    MajorMode(ModeId),
    /// Active minor-mode keymap. The `ModeId` is the layer's
    /// identity — push for the same mode is idempotent on the
    /// layer (bindings replaced, not appended as a sibling).
    /// Cross-mode order at merge time comes from
    /// `active_modes[active_buffer]` (K.1.c), not from the
    /// `ModeId` ordering.
    MinorMode(ModeId),
    /// User config (`init.rs`).
    User,
    /// Per-buffer ad-hoc binding (`:nmap <buffer>`).
    Buffer,
}

/// What the trie returns at a terminal node.
///
/// Carries enough provenance for `:describe-key` and enough
/// information for the dispatcher to fire the binding:
/// - `command` -- the typed `CommandInvocation` to dispatch.
/// - `source` -- where the binding was registered (catalog
///   entry, user `init.rs:42`, plugin `foo.wit:7`).
/// - `layer` -- priority tier for tie-break / shadowing.
#[derive(Debug, Clone)]
pub struct BoundCommand {
    pub command: CommandInvocation,
    pub source: SourceLocation,
    pub layer: KeymapLayer,
}

impl BoundCommand {
    /// Construct a binding that dispatches via the
    /// `CommandInvocation`.
    pub fn from_invocation(
        command: CommandInvocation,
        source: SourceLocation,
        layer: KeymapLayer,
    ) -> Self {
        Self {
            command,
            source,
            layer,
        }
    }
}

/// Lookup outcome.
#[derive(Debug, Clone)]
pub enum LookupResult {
    /// Walk terminated at a node with a terminal binding.
    /// `captured` records every char absorbed by `CharLiteral`
    /// wildcards along the path -- empty for binding paths
    /// without wildcards (the common case).
    Bound {
        command: Arc<BoundCommand>,
        captured: Vec<char>,
    },
    /// Walk consumed every input chord but landed at an
    /// internal node (children present, no terminal). Caller
    /// stays in pending state and waits for the next chord.
    Partial,
    /// Walk hit a node with no descent matching the next
    /// chord. Caller falls through (Insert mode literal text,
    /// Normal mode no-op, etc.).
    Unbound,
}

#[derive(Debug, Default)]
struct TrieNode {
    children: HashMap<KeyChord, TrieNode>,
    char_wildcard: Option<Box<TrieNode>>,
    binding: Option<Arc<BoundCommand>>,
}

impl Clone for TrieNode {
    fn clone(&self) -> Self {
        Self {
            children: self.children.clone(),
            char_wildcard: self.char_wildcard.clone(),
            binding: self.binding.clone(),
        }
    }
}

/// One layer's worth of bindings, indexed for
/// `O(prefix_length)` lookup.
#[derive(Debug, Clone, Default)]
pub struct KeymapTrie {
    root: TrieNode,
}

impl KeymapTrie {
    /// Empty trie. Use `insert` to populate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `bound` at the chord path `path`. Replaces any
    /// existing binding at the same path (last-bind-wins within
    /// a single trie / layer). Empty path is a no-op (no
    /// "bind nothing").
    pub fn insert(&mut self, path: &[ChordPattern], bound: Arc<BoundCommand>) {
        if path.is_empty() {
            return;
        }
        let mut node = &mut self.root;
        for seg in path {
            node = match seg {
                ChordPattern::Literal(chord) => node.children.entry(*chord).or_default(),
                ChordPattern::CharLiteral => node
                    .char_wildcard
                    .get_or_insert_with(|| Box::new(TrieNode::default())),
            };
        }
        node.binding = Some(bound);
    }

    /// Remove the binding at `path` if present. Returns the
    /// dropped `Arc<BoundCommand>` for callers that want to
    /// surface "what was unbound" in echo messages. Does not
    /// prune empty intermediate nodes -- a future bind at the
    /// same prefix should reuse them, and the wasted nodes are
    /// bounded by the trie's lifetime.
    pub fn remove(&mut self, path: &[ChordPattern]) -> Option<Arc<BoundCommand>> {
        let mut node = &mut self.root;
        for seg in path {
            node = match seg {
                ChordPattern::Literal(chord) => node.children.get_mut(chord)?,
                ChordPattern::CharLiteral => node.char_wildcard.as_deref_mut()?,
            };
        }
        node.binding.take()
    }

    /// Walk the input chord sequence and return what we found.
    ///
    /// Lookup precedence at each depth: exact `children` match
    /// first; if absent, fall back to `char_wildcard` when the
    /// input chord is a bare printable char (no modifiers).
    /// Modifier-bearing chords (`<C-x>`) never match the
    /// wildcard -- the wildcard's job is "any single typed
    /// char" for marks / registers / find-char.
    pub fn lookup(&self, chords: &[KeyChord]) -> LookupResult {
        let mut node = &self.root;
        let mut captured: Vec<char> = Vec::new();
        for chord in chords {
            if let Some(next) = node.children.get(chord) {
                node = next;
                continue;
            }
            // Wildcard fallback: only bare chars (no
            // modifiers) qualify. `<C-x>` does not match a
            // wildcard intended for `'a` / `"a` / `fX`.
            if chord.mods.is_empty()
                && let KeyKind::Char(c) = chord.key
                && let Some(wild) = node.char_wildcard.as_deref()
            {
                captured.push(c);
                node = wild;
                continue;
            }
            return LookupResult::Unbound;
        }
        match node.binding.as_ref() {
            Some(b) => LookupResult::Bound {
                command: Arc::clone(b),
                captured,
            },
            None => {
                if node.children.is_empty() && node.char_wildcard.is_none() {
                    LookupResult::Unbound
                } else {
                    LookupResult::Partial
                }
            }
        }
    }

    /// Overlay `other` on top of `self`. `other`'s bindings win
    /// on conflict; the merge is structural so paths in `other`
    /// that don't conflict simply add to `self`'s tree.
    ///
    /// The registry's layer-stack collapse calls this in
    /// priority order (lowest first) so the highest-priority
    /// layer's bindings end up authoritative. See
    /// `docs/dev/architecture/keymap-architecture.md` §2 + §4 (layer-merge on
    /// write, not on read).
    pub fn merge_over(&mut self, other: &KeymapTrie) {
        merge_node(&mut self.root, &other.root);
    }

    /// Number of terminal bindings in the trie. O(N) walk;
    /// useful for tests + registry telemetry, not on the hot
    /// path.
    pub fn binding_count(&self) -> usize {
        count_node(&self.root)
    }

    /// MARG.2 (2026-06-03): walk every terminal binding,
    /// invoking `f` with the chord path that reaches it and
    /// the `Arc<BoundCommand>` at that path. Used by the
    /// reverse-keymap-cache builder in `KeymapRegistry` to
    /// produce a `command_name → Vec<KeyChord>` map for the
    /// keybinding annotator (see
    /// `docs/dev/architecture/marginalia.md` §6).
    ///
    /// Path slice is borrowed; the closure must capture-by-
    /// clone if it wants to retain the chord sequence. O(N)
    /// over the bound-chord count — same cost class as
    /// [`Self::binding_count`], not on the hot path.
    pub fn walk_bindings<F>(&self, mut f: F)
    where
        F: FnMut(&[ChordPattern], &Arc<BoundCommand>),
    {
        let mut path: Vec<ChordPattern> = Vec::new();
        walk_node(&self.root, &mut path, &mut f);
    }
}

fn walk_node<F>(node: &TrieNode, path: &mut Vec<ChordPattern>, f: &mut F)
where
    F: FnMut(&[ChordPattern], &Arc<BoundCommand>),
{
    if let Some(b) = node.binding.as_ref() {
        f(path.as_slice(), b);
    }
    for (chord, child) in &node.children {
        path.push(ChordPattern::Literal(*chord));
        walk_node(child, path, f);
        path.pop();
    }
    if let Some(wild) = node.char_wildcard.as_deref() {
        path.push(ChordPattern::CharLiteral);
        walk_node(wild, path, f);
        path.pop();
    }
}

fn merge_node(dst: &mut TrieNode, src: &TrieNode) {
    if let Some(b) = src.binding.as_ref() {
        dst.binding = Some(Arc::clone(b));
    }
    for (chord, src_child) in &src.children {
        let dst_child = dst.children.entry(*chord).or_default();
        merge_node(dst_child, src_child);
    }
    if let Some(src_wild) = src.char_wildcard.as_deref() {
        let dst_wild = dst
            .char_wildcard
            .get_or_insert_with(|| Box::new(TrieNode::default()));
        merge_node(dst_wild, src_wild);
    }
}

fn count_node(node: &TrieNode) -> usize {
    let mut n = if node.binding.is_some() { 1 } else { 0 };
    for child in node.children.values() {
        n += count_node(child);
    }
    if let Some(wild) = node.char_wildcard.as_deref() {
        n += count_node(wild);
    }
    n
}

// `KeyMods::is_empty` is `pub const` on `KeyChord::mods`; this
// helper is only here to silence an unused-import warning if
// future revisions of this file stop using `KeyMods` directly.
#[allow(dead_code)]
fn _assert_mods_used(m: KeyMods) -> bool {
    m.is_empty()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_protocol::ids::CommandId;

    fn fake_bound(label: &'static str) -> Arc<BoundCommand> {
        // Tests don't dispatch the invocation; they just verify
        // the trie returns the right `Arc<BoundCommand>` at the
        // right path.
        let _ = label;
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(CommandId::new(0)),
            SourceLocation::synthetic("test"),
            KeymapLayer::Builtin,
        ))
    }

    fn lit(c: char) -> ChordPattern {
        ChordPattern::Literal(KeyChord::char(c))
    }

    fn ctrl_lit(c: char) -> ChordPattern {
        ChordPattern::Literal(KeyChord::ctrl(c))
    }

    fn pressed(c: char) -> KeyChord {
        KeyChord::char(c)
    }

    #[test]
    fn lookup_returns_bound_at_terminal() {
        let mut t = KeymapTrie::new();
        let bound = fake_bound("dd");
        t.insert(&[lit('d'), lit('d')], Arc::clone(&bound));

        let r = t.lookup(&[pressed('d'), pressed('d')]);
        match r {
            LookupResult::Bound { command, captured } => {
                assert!(Arc::ptr_eq(&command, &bound));
                assert!(captured.is_empty());
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn lookup_returns_partial_at_internal_node() {
        let mut t = KeymapTrie::new();
        t.insert(&[lit('g'), lit('d')], fake_bound("gd"));

        let r = t.lookup(&[pressed('g')]);
        assert!(matches!(r, LookupResult::Partial), "got {r:?}");
    }

    #[test]
    fn lookup_returns_unbound_when_no_descent_matches() {
        let mut t = KeymapTrie::new();
        t.insert(&[lit('g'), lit('d')], fake_bound("gd"));

        // `q` has no entry at root.
        let r = t.lookup(&[pressed('q')]);
        assert!(matches!(r, LookupResult::Unbound), "got {r:?}");

        // `g` then `q` -- `g` is a partial node, but `q` doesn't
        // descend from it.
        let r = t.lookup(&[pressed('g'), pressed('q')]);
        assert!(matches!(r, LookupResult::Unbound), "got {r:?}");
    }

    #[test]
    fn lookup_walks_wildcard_and_captures_char() {
        let mut t = KeymapTrie::new();
        t.insert(
            &[lit('f'), ChordPattern::CharLiteral],
            fake_bound("find_char"),
        );

        let r = t.lookup(&[pressed('f'), pressed('x')]);
        match r {
            LookupResult::Bound { captured, .. } => {
                assert_eq!(captured, vec!['x']);
            }
            other => panic!("expected Bound, got {other:?}"),
        }

        // Different char -> still bound, captures that char.
        let r = t.lookup(&[pressed('f'), pressed('Q')]);
        match r {
            LookupResult::Bound { captured, .. } => {
                assert_eq!(captured, vec!['Q']);
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn lookup_prefers_exact_match_over_wildcard() {
        let mut t = KeymapTrie::new();
        t.insert(
            &[lit('f'), ChordPattern::CharLiteral],
            fake_bound("find_char_wild"),
        );
        let exact = fake_bound("find_char_q_specific");
        t.insert(&[lit('f'), lit('q')], Arc::clone(&exact));

        // `f q` -- exact match wins.
        let r = t.lookup(&[pressed('f'), pressed('q')]);
        match r {
            LookupResult::Bound { command, captured } => {
                assert!(Arc::ptr_eq(&command, &exact));
                assert!(captured.is_empty(), "exact-match path captures nothing");
            }
            other => panic!("expected Bound, got {other:?}"),
        }

        // `f x` -- falls through to wildcard.
        let r = t.lookup(&[pressed('f'), pressed('x')]);
        match r {
            LookupResult::Bound { captured, .. } => {
                assert_eq!(captured, vec!['x']);
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn wildcard_does_not_match_modifier_bearing_chord() {
        let mut t = KeymapTrie::new();
        t.insert(
            &[lit('f'), ChordPattern::CharLiteral],
            fake_bound("find_char"),
        );

        // `f <C-x>` -- the wildcard is intended for "any TYPED
        // char", not Ctrl-x.
        let r = t.lookup(&[pressed('f'), KeyChord::ctrl('x')]);
        assert!(matches!(r, LookupResult::Unbound), "got {r:?}");
    }

    #[test]
    fn remove_returns_dropped_binding_and_makes_path_unbound() {
        let mut t = KeymapTrie::new();
        let bound = fake_bound("dd");
        t.insert(&[lit('d'), lit('d')], Arc::clone(&bound));

        let dropped = t.remove(&[lit('d'), lit('d')]).expect("had binding");
        assert!(Arc::ptr_eq(&dropped, &bound));

        // After removal, the full `dd` lookup hits a node with
        // no binding AND no further descents -- Unbound. The
        // `d` prefix is still Partial because the empty
        // intermediate node remains (a future bind at the
        // same prefix can repopulate it without rebuilding
        // the tree).
        let full = t.lookup(&[pressed('d'), pressed('d')]);
        assert!(matches!(full, LookupResult::Unbound), "got {full:?}");
        let prefix = t.lookup(&[pressed('d')]);
        assert!(matches!(prefix, LookupResult::Partial), "got {prefix:?}");
    }

    #[test]
    fn merge_over_overlays_other_bindings_on_conflict() {
        let mut base = KeymapTrie::new();
        let base_dd = fake_bound("base.dd");
        base.insert(&[lit('d'), lit('d')], Arc::clone(&base_dd));

        let mut over = KeymapTrie::new();
        let over_dd = fake_bound("over.dd");
        over.insert(&[lit('d'), lit('d')], Arc::clone(&over_dd));
        let over_yy = fake_bound("over.yy");
        over.insert(&[lit('y'), lit('y')], Arc::clone(&over_yy));

        base.merge_over(&over);

        // `dd` -> over wins.
        let r = base.lookup(&[pressed('d'), pressed('d')]);
        match r {
            LookupResult::Bound { command, .. } => {
                assert!(Arc::ptr_eq(&command, &over_dd));
            }
            other => panic!("expected Bound, got {other:?}"),
        }

        // `yy` -> only over had it; carried over.
        let r = base.lookup(&[pressed('y'), pressed('y')]);
        assert!(matches!(r, LookupResult::Bound { .. }));
    }

    #[test]
    fn merge_over_preserves_non_conflicting_paths() {
        let mut base = KeymapTrie::new();
        base.insert(&[lit('d'), lit('d')], fake_bound("base.dd"));

        let mut over = KeymapTrie::new();
        over.insert(&[lit('y'), lit('y')], fake_bound("over.yy"));

        base.merge_over(&over);

        // Both paths bind.
        assert!(matches!(
            base.lookup(&[pressed('d'), pressed('d')]),
            LookupResult::Bound { .. }
        ));
        assert!(matches!(
            base.lookup(&[pressed('y'), pressed('y')]),
            LookupResult::Bound { .. }
        ));
    }

    #[test]
    fn merge_over_combines_wildcard_subtrees() {
        let mut base = KeymapTrie::new();
        base.insert(
            &[lit('f'), ChordPattern::CharLiteral],
            fake_bound("base.find"),
        );

        let mut over = KeymapTrie::new();
        // Override the wildcard binding with a layer-higher
        // one. Same wildcard slot, different bound.
        let over_find = fake_bound("over.find");
        over.insert(
            &[lit('f'), ChordPattern::CharLiteral],
            Arc::clone(&over_find),
        );

        base.merge_over(&over);
        let r = base.lookup(&[pressed('f'), pressed('z')]);
        match r {
            LookupResult::Bound { command, captured } => {
                assert!(Arc::ptr_eq(&command, &over_find));
                assert_eq!(captured, vec!['z']);
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn binding_count_walks_every_terminal() {
        let mut t = KeymapTrie::new();
        t.insert(&[lit('j')], fake_bound("j"));
        t.insert(&[lit('k')], fake_bound("k"));
        t.insert(&[lit('g'), lit('d')], fake_bound("gd"));
        t.insert(&[lit('g'), lit('g')], fake_bound("gg"));
        t.insert(&[lit('f'), ChordPattern::CharLiteral], fake_bound("find"));
        assert_eq!(t.binding_count(), 5);
    }

    #[test]
    fn ctrl_modified_chord_routes_through_children_not_wildcard() {
        // `<C-w>j` -- the second-key trie node is reached via
        // an exact `<C-w>` child, not via a wildcard at root.
        let mut t = KeymapTrie::new();
        t.insert(&[ctrl_lit('w'), lit('j')], fake_bound("window_down"));

        let r = t.lookup(&[KeyChord::ctrl('w'), pressed('j')]);
        assert!(matches!(r, LookupResult::Bound { .. }));
    }

    #[test]
    fn empty_path_insert_is_noop() {
        let mut t = KeymapTrie::new();
        t.insert(&[], fake_bound("ignored"));
        assert_eq!(t.binding_count(), 0);
    }

    #[test]
    fn empty_input_on_populated_trie_returns_partial() {
        let mut t = KeymapTrie::new();
        t.insert(&[lit('j')], fake_bound("j"));
        let r = t.lookup(&[]);
        assert!(matches!(r, LookupResult::Partial), "got {r:?}");
    }

    #[test]
    fn empty_input_on_empty_trie_returns_unbound() {
        let t = KeymapTrie::new();
        let r = t.lookup(&[]);
        assert!(matches!(r, LookupResult::Unbound), "got {r:?}");
    }
}
