//! Syntax-highlight style types and the `ExcerptHighlighter` trait.
//!
//! Moved from `lattice-syntax` so that `lattice-runtime` (which defines the
//! `Document` trait) can reference `ExcerptHighlighter` without pulling in
//! `lattice-syntax` — which has a transitive dep on `lattice-mode` →
//! `lattice-runtime` (cycle). `lattice-cells` has no lattice deps, breaking
//! the chain cleanly.
//!
//! `lattice-syntax` re-exports everything from here so call-sites outside
//! `lattice-cells` / `lattice-runtime` see no path change.

use std::sync::Arc;

/// Semantic style category emitted by the tree-sitter highlighter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Default,
    Comment,
    LineComment,
    String,
    Keyword,
    Type,
    Number,
    Function,
    Constant,
    Variable,
    Operator,
    Punctuation,
    Attribute,
    // ---- Markup styles (markdown / org / future rich-text modes) ----
    /// `# Heading` — level 1.
    Heading1,
    /// `## Heading` — level 2.
    Heading2,
    /// `### Heading` — level 3.
    Heading3,
    /// `#### Heading` — level 4.
    Heading4,
    /// `##### Heading` — level 5.
    Heading5,
    /// `###### Heading` — level 6.
    Heading6,
    /// `**bold**` / `__bold__` text.
    Bold,
    /// `*italic*` / `_italic_` text.
    Italic,
    /// Link label / link text (`[label]`). Distinct from [`Style::Url`] so
    /// the renderer can underline navigable labels without underlining the URL.
    Link,
    /// Link destination (`(url)`) and autolinks.
    Url,
    /// Inline `` `code` ``, fenced code blocks without an info string, link
    /// titles.
    MarkupRaw,
    /// List markers, thematic breaks, blockquote markers, and other markup
    /// punctuation.
    Markup,
    // ---- Diagnostic severities (L4b: the `gl` popup colours each line
    // by its diagnostic's severity). These resolve to the same theme
    // colours the gutter glyph + inline underline use
    // (`diagnostic_{error,warning,info,hint}` elements), via
    // `syntax_element_id`. Not produced by any tree-sitter grammar —
    // only the diagnostics-popup highlight builder emits them. ----
    /// Error-severity diagnostic line.
    DiagnosticError,
    /// Warning-severity diagnostic line.
    DiagnosticWarning,
    /// Information-severity diagnostic line.
    DiagnosticInfo,
    /// Hint-severity diagnostic line.
    DiagnosticHint,
    // ---- Diff text styles (magit inline diff content) ----
    /// Added line content (`+` lines) in a unified diff.
    DiffAdd,
    /// Removed line content (`-` lines) in a unified diff.
    DiffRemove,
    // ---- Magit-owned styles ----
    // Git concepts magit's own buffers render (commit SHAs, the
    // checked-out branch, ref-decoration lists, rebase-todo verbs,
    // blame authors) are NOT tree-sitter syntax categories — giving
    // them their own `Style` variants (rather than reusing
    // `Keyword`/`Link`/`Type`/`Comment`, which name unrelated
    // source-code concepts) keeps the mapping honest and lets a theme
    // retune magit's palette independently of its code-syntax colors.
    /// A commit SHA (magit-log, magit-blame, magit-rebase's todo).
    MagitSha,
    /// The checked-out branch in a branch list (magit-branch's `* `
    /// marker + name).
    MagitBranchCurrent,
    /// A ref-decoration list after a log SHA (`(HEAD -> main, ...)`).
    MagitRefDecoration,
    /// A rebase-todo verb (`pick`/`reword`/`edit`/`squash`/`fixup`/`drop`).
    MagitRebaseVerb,
    /// The author column in `magit-blame` output.
    MagitAuthor,
    // ---- Help-owned styles (HP.2) ----
    // Help pages are markdown, and the markdown BLOCK grammar has no
    // `code_span` node — that lives in the inline grammar, which is not
    // wired up. So every `` `gr` ``, `` `:magit-status` `` and
    // `` `action:magit-refresh` `` in every help page rendered as plain
    // prose with visible backticks.
    //
    // Classifying them into four styles rather than one is what lets a
    // theme make a KEY YOU PRESS look different from a COMMAND YOU TYPE
    // — the distinction a reader is actually scanning for. They get
    // their own variants for the same reason the `Magit*` family does:
    // reusing `Keyword` or `Link` would name an unrelated source-code
    // concept and tie help's palette to the code palette.
    /// A key or chord you press (`` `gr` ``, `` `<C-c>g` ``, `` `]]` ``).
    HelpKey,
    /// An ex-command you type (`` `:magit-status` ``).
    HelpCommand,
    /// An action id (`` `action:magit-refresh` ``).
    HelpAction,
    /// Any other inline literal — a path, a filename, a git argument.
    HelpLiteral,
    /// Inline virtual text — an LSP inlay hint, or any other leading /
    /// trailing text a producer splices into a row without it being in
    /// the buffer.
    ///
    /// DL.3a: inlay runs used to be painted with a hardcoded
    /// `Color::Named(DarkGray)` in the cells worker, with the run's
    /// style discarded — while `inlay.hint` had been a registered theme
    /// element the whole time. Giving inlays a real style variant is
    /// what routes them through the theme like everything else, and it
    /// is what lets a producer override the colour per inlay (the
    /// listing icons use [`Style::Element`]).
    InlayHint,
    // ---- The open end of the vocabulary (DL.1) ----
    /// A span styled by a **registered theme element**, named directly.
    ///
    /// Every variant above is a closed, editor-owned category, and that
    /// is right for concepts the editor itself understands. It cannot
    /// work for vocabularies that are open by nature — a per-language
    /// file-icon palette has ~50 entries and grows, and a WASM plugin
    /// can register a theme element by name but can **never** add a
    /// variant to a Rust enum. Without this, themed highlighting is
    /// reachable only by editing core, which makes it impossible for
    /// plugins by construction (paramount goal #2).
    ///
    /// `syntax_element_id` returns the id unchanged, so this resolves
    /// through exactly the same `ResolvedTheme` lookup as every builtin
    /// category — a theme retunes it by name like any other element.
    ///
    /// Carries [`lattice_theme::ElementId`] (a `u32` newtype), so
    /// `Style` stays `Copy` and its size is unchanged.
    Element(lattice_theme::ElementId),
}

impl Style {
    /// A stable-within-this-process numeric fingerprint, for folding a
    /// style into a cache-version hash.
    ///
    /// DL.1: this exists because [`Style`] stopped being field-less.
    /// Consumers used to write `style as u64`, which the compiler
    /// allowed only while every variant was a unit — so adding
    /// [`Style::Element`] would have broken them silently in spirit
    /// (loudly in practice, which is how this was found). Routing them
    /// through a named method means the payload is *included* in the
    /// fingerprint: two spans differing only in which registered
    /// element they name must not collide, or a theme-element change
    /// would leave a stale matrix on screen.
    pub fn fingerprint(self) -> u64 {
        use std::hash::{Hash, Hasher};
        struct Fnv(u64);
        impl Hasher for Fnv {
            fn finish(&self) -> u64 {
                self.0
            }
            fn write(&mut self, bytes: &[u8]) {
                for b in bytes {
                    self.0 ^= u64::from(*b);
                    self.0 = self.0.wrapping_mul(1099511628211);
                }
            }
        }
        let mut h = Fnv(14695981039346656037);
        std::mem::discriminant(&self).hash(&mut h);
        if let Style::Element(id) = self {
            h.write(&id.0.to_le_bytes());
        }
        h.finish()
    }
}

/// Byte-range span within one source line, carrying a semantic [`Style`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyledSpan {
    /// Byte offset within the line where the span starts.
    pub start: usize,
    /// Byte offset within the line (exclusive).
    pub end: usize,
    pub style: Style,
}

/// DR.2 (2026-08-12): a byte range whose **background** differs from
/// its row's — intra-line diff refinement.
///
/// A second, independent axis from [`StyledSpan`], and deliberately a
/// separate type rather than a `bg` field on that one. The foreground
/// axis resolves by first-match-wins over a concatenated list; a
/// background is a different question with different precedence, and
/// fusing them would force every existing span producer to have an
/// opinion about a concern it does not have.
///
/// See `docs/dev/architecture/diff-refinement.md` §3 and
/// `span-layering.md` §1 (the two-axis contract this extends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefineSpan {
    /// Byte offset within the line where the span starts.
    pub start: usize,
    /// Byte offset within the line (exclusive).
    pub end: usize,
    pub kind: RefineKind,
}

/// Which side of a refined pair a [`RefineSpan`] belongs to. Picks the
/// theme element, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefineKind {
    /// Bytes added on this line — `diff.add.refine.bg`.
    Added,
    /// Bytes removed from this line — `diff.remove.refine.bg`.
    Removed,
}

/// Trait implemented by `SyntaxHandle` (and future highlight providers).
/// Used by `Document::excerpt_highlights` so that `lattice-runtime` can
/// expose per-excerpt highlighting through the `Document` trait without
/// taking a direct dep on `lattice-syntax`.
pub trait ExcerptHighlighter: Send + Sync {
    /// Return per-line styled spans for source rows `lo..hi` (exclusive).
    /// The returned `Vec` has exactly `(hi - lo)` entries; an empty inner
    /// `Vec` means "no spans on that line" (fall back to default fg).
    /// Returns `None` when the highlight snapshot is stale or unavailable.
    fn highlight_lines(&self, lo: u32, hi: u32) -> Option<Vec<Vec<StyledSpan>>>;

    /// Monotonic version of the last-published parse result. Used to build
    /// `MatrixVersion::syntax` without taking a dep on `SyntaxHandle` internals.
    fn highlight_version(&self) -> u64;
}

/// Per-excerpt entry produced by [`Document::excerpt_highlights`].
///
/// `composed_start` / `composed_end` are line numbers in the multibuffer's
/// composed coordinate space. `source_start` is the first source line mapped
/// to `composed_start`. The highlighter operates in source coordinates.
pub struct ExcerptHighlight {
    pub composed_start: u32,
    pub composed_end: u32,
    pub source_start: u32,
    pub highlighter: Arc<dyn ExcerptHighlighter>,
    /// OA.7b: the excerpt's grammar name, for the conceal rules that apply to
    /// its rows.
    ///
    /// Conceal is otherwise resolved once per PANE from the buffer's single
    /// syntax handle — and a multibuffer has no single language, so it
    /// resolved to no rules and an org link in an agenda row showed its raw
    /// brackets while the same line concealed correctly in its own file.
    ///
    /// A `&'static str` rather than the rules themselves: `lattice-cells` has
    /// no business knowing what a conceal rule is, and the name is what the
    /// registry is keyed by anyway.
    pub lang: Option<&'static str>,
}
