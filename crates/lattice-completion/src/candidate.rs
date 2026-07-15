#![allow(clippy::single_range_in_vec_init)]
//! Candidate value types that flow through the completion pipeline
//! (DESIGN.md §5.11.3).
//!
//! Three shapes, one per pipeline stage:
//!
//! - [`RawCandidate`] -- output of a generator. Insertion text + kind +
//!   structured payload.
//! - [`ScoredCandidate`] -- after the matcher: `RawCandidate` + score +
//!   the byte ranges the matcher considered "matched" (vertico's
//!   match-face highlights these).
//! - [`RenderedCandidate`] -- after annotators: above + a vec of
//!   typed [`Annotation`]s the renderer paints to the right of each
//!   candidate (MARG.1, see `docs/dev/architecture/marginalia.md`).
//!
//! Three shapes (not one with optional fields) so the type system
//! enforces stage ordering: a generator produces only `RawCandidate`s;
//! a matcher produces only `ScoredCandidate`s; annotators only mutate
//! `RenderedCandidate`s. Pipeline order can't accidentally invert.

use std::borrow::Cow;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use lattice_grammar::source::SourceLocation;
use lattice_protocol::KeyChord;

/// What kind of thing this candidate is. Drives icon / colour /
/// grouping in the popup, and (for CommandKind) hints at the
/// follow-up `:describe-*` target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum CandidateKind {
    Command,
    Option,
    File,
    Directory,
    Pattern,
    Buffer,
    Register,
    Mark,
    Chord,
    Plain,
    /// Plugin-defined kind. The numeric tag is registered alongside
    /// the plugin's generator so `:describe-completion-source` can
    /// resolve it back to a name.
    Extension(u32),
}

impl CandidateKind {
    /// Issue #35 (2026-05-22): one-char ASCII glyph for picker
    /// marginalia. The renderer paints this in the left
    /// margin of each picker row so the user can scan the
    /// candidate list by kind. ASCII fallback chosen so it
    /// works even when nerd-fonts are off; the icon system
    /// (Phase 5.6.7) may layer a richer sprite on top later.
    pub fn glyph(&self) -> char {
        match self {
            CandidateKind::Command => ':',
            CandidateKind::Option => '=',
            CandidateKind::File => 'f',
            CandidateKind::Directory => 'd',
            CandidateKind::Pattern => '/',
            CandidateKind::Buffer => 'b',
            CandidateKind::Register => '"',
            CandidateKind::Mark => '\'',
            CandidateKind::Chord => '@',
            CandidateKind::Plain => '·',
            CandidateKind::Extension(_) => '+',
        }
    }
}

/// Generator-supplied metadata travelling alongside the candidate.
/// Annotators read this to produce display text without re-querying
/// the underlying registry. Plugin generators stash their own
/// payload in `Extension` (msgpack on the wire when WASM lands).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CandidateData {
    /// `gen:commands` payload: command's full metadata at the time
    /// of generation.
    Command {
        name: String,
        doc: String,
        kind_label: String,
        source: SourceLocation,
    },
    /// `gen:files` payload.
    File {
        path: PathBuf,
        is_dir: bool,
        size: Option<u64>,
    },
    /// `gen:options` payload (typed options post-§5.12).
    /// Emitted for option-name completion (`:set <Tab>`). `doc`
    /// is `OptionDecl::DOC`.
    Option {
        name: String,
        current_value: String,
        doc: String,
    },
    /// `gen:options` payload — value-completion mode.
    /// Slice `3c.unify.option-doc-annotator`. Emitted for
    /// `:set foo=<Tab>` when the option's `OptionType::enumerate`
    /// returns Some. `doc` is `EnumeratedValue::doc` — per-value
    /// help text the type chose to surface (empty when the type
    /// hasn't overridden `enumerate_with_docs`).
    OptionValue {
        option_name: String,
        value: String,
        doc: String,
    },
    /// `gen:chords` payload.
    Chord {
        chord: String,
        mode_label: String,
        doc: String,
    },
    /// `gen:registers` payload.
    Register { name: char, preview: String },
    /// `gen:marks` payload.
    Mark { name: char, position: String },
    /// Generator that needs no extra metadata (text alone is enough).
    Plain,
    /// Plugin-defined arbitrary payload. The pipeline preserves it
    /// unchanged through matcher / ranker; annotators registered by
    /// the same plugin recognise the `kind_id` and decode `payload`.
    Extension { kind_id: u32, payload: Vec<u8> },
}

/// Output of a generator. The pipeline passes these through the
/// matcher next.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawCandidate {
    /// Text inserted when the user accepts this candidate.
    pub text: String,
    /// What appears in the popup before annotators run. May be
    /// `text` verbatim or a richer form (e.g. `"~/Documents"` for a
    /// `File` whose `text` is the absolute path).
    pub display: String,
    pub kind: CandidateKind,
    pub data: CandidateData,
    /// Source that produced this candidate. `None` for legacy /
    /// non-insert callers (cmdline pickers, plain test
    /// fixtures); insert-mode generators tag themselves so the
    /// per-source-priority ranker (Phase 4.2.g.5) can resolve
    /// the right priority bucket. The string is the same id
    /// surfaced in `:set completion.source.<id>.priority=…` and
    /// in `:help completion-sources`.
    #[serde(default)]
    pub source: Option<crate::insert::SourceId>,
    /// What the host should do if the user accepts this
    /// candidate. Slice `3c.unify.picker-generator-trait-unify`
    /// (7b.0): adds a typed accept payload to the candidate so
    /// `AcceptHandler` impls can be stateless — the default
    /// handler (`source_registration::DefaultAcceptHandler`)
    /// just clones this field. `None` ⇒ default behaviour:
    /// cmdline replaces `cmdline[replace_start..]` with `text`;
    /// picker echoes "no accept_action set" via the default
    /// handler. Surfaces that need typed dispatch set this at
    /// candidate-build time.
    ///
    /// Boxed (slice 8): `AcceptAction` is a tagged enum
    /// carrying PathBuf / String / Args among its variants —
    /// its inline size dominates RawCandidate. Boxing keeps
    /// `Option<Box<AcceptAction>>` at 8 bytes (None = null
    /// pointer) so cmdline-completion + insert-completion
    /// candidates (which leave it unset) don't pay the cost.
    /// Picker candidates pay one heap alloc per row at
    /// construction, recovered ~10× over by smaller per-
    /// candidate memcpy during pipeline matcher/ranker passes
    /// (picker::refilter/n=5000 measured 2× faster than the
    /// inline shape — see benchmarks.md).
    ///
    /// `#[serde(skip)]` because the `Custom` variant carries
    /// `Arc<dyn Any>` which isn't serializable; the rest of the
    /// candidate (text + display + data) round-trips through
    /// the cache, but the accept action is recomputed at the
    /// callsite when needed.
    #[serde(skip)]
    pub accept_action: Option<Box<crate::source_registration::AcceptAction>>,
    /// MARG §8: source-supplied marginalia. A picker source that
    /// knows its rows' metadata (the file/dir picker's perms / size /
    /// mtime) attaches typed [`Annotation`]s here at build time; the
    /// pipeline copies them onto the [`RenderedCandidate`] so the
    /// renderer color-codes them. Empty for sources that contribute no
    /// marginalia (the common case). `#[serde(skip)]`: annotations are
    /// render-time data resolved against the live theme, never cached.
    #[serde(skip)]
    pub annotations: Vec<Annotation>,
    /// PH.1: optional syntax-highlight overlay for the `display`
    /// run (the matchable text itself, NOT a trailing column —
    /// that's `annotations`). Each span carries a *semantic*
    /// [`Style`](lattice_cells::style::Style), resolved to a
    /// theme color only at the render seam (`resolve_syntax_style`),
    /// so `:colorscheme` recolors picker previews live. Byte
    /// offsets into `display`. Empty ⇒ today's plain single-color
    /// preview. Producer contract (PH.2): spans are sorted by
    /// `range.start`, non-overlapping, and aligned to char
    /// boundaries; the renderer composes them under
    /// `match_ranges` (fuzzy-match highlight wins on overlap).
    /// `#[serde(skip)]` to match `annotations` — render-time
    /// overlay, not cached.
    /// See `docs/dev/architecture/picker-preview-highlight.md`.
    #[serde(skip)]
    pub display_spans: Vec<DisplaySpan>,
}

/// PH.1: a syntax-styled run within a candidate's `display`
/// text. Carries a semantic [`Style`](lattice_cells::style::Style)
/// (a closed enum with an existing `Style → ElementId` map),
/// NOT a resolved color — the renderer resolves it via
/// `resolve_syntax_style` at paint so `:colorscheme` recolors
/// live, mirroring the `Annotation::Styled` carry-semantic /
/// resolve-at-seam invariant (marginalia §3 / §8.2). `range`
/// is a byte range into `display`.
///
/// Semantic `Style` (not a slot-key `Arc<str>` like
/// `AnnotationSegment`) because syntax styles are a *closed*
/// set with direct typed resolution, whereas annotation slots
/// are *open* (plugins name arbitrary slots). See
/// `docs/dev/architecture/picker-preview-highlight.md` §4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySpan {
    pub range: Range<usize>,
    pub style: lattice_cells::style::Style,
}

impl RawCandidate {
    /// Convenience for plain text candidates with no metadata.
    /// Leaves `source` + `accept_action` unset; insert-mode
    /// producers chain [`Self::with_source`] to tag themselves;
    /// picker generators set `accept_action` per row.
    pub fn plain(text: impl Into<String>, kind: CandidateKind) -> Self {
        let text = text.into();
        Self {
            display: text.clone(),
            text,
            kind,
            data: CandidateData::Plain,
            source: None,
            accept_action: None,
            annotations: Vec::new(),
            display_spans: Vec::new(),
        }
    }

    /// Tag the candidate with its producing source. Used by
    /// insert-mode generators so the ranker can apply per-source
    /// priority (Phase 4.2.g.5 (2/3)). Picker generators leave
    /// the field unset -- their pipeline doesn't apply
    /// per-source priority.
    pub fn with_source(mut self, source: crate::insert::SourceId) -> Self {
        self.source = Some(source);
        self
    }
}

/// Score the matcher assigned. Higher is better; `0` means
/// "doesn't match" (and the candidate is filtered out before it
/// becomes a `ScoredCandidate`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct MatchScore(pub u32);

impl MatchScore {
    pub const PERFECT: MatchScore = MatchScore(1000);
    pub const PREFIX: MatchScore = MatchScore(900);
    pub const FUZZY_HIGH: MatchScore = MatchScore(700);
    pub const SUBSTRING: MatchScore = MatchScore(500);
    pub const FUZZY_LOW: MatchScore = MatchScore(200);

    pub fn get(self) -> u32 {
        self.0
    }
}

/// A `RawCandidate` plus the matcher's verdict. Survives the
/// match stage; consumed by the ranker and (in turn) the annotators.
#[derive(Debug, Clone)]
pub struct ScoredCandidate {
    pub raw: RawCandidate,
    pub score: MatchScore,
    /// Byte ranges in `raw.text` that the matcher consumed. Empty
    /// vec for matchers that don't track ranges (e.g. coarse fuzzy);
    /// renderers tolerate either case.
    pub match_ranges: Vec<Range<usize>>,
}

/// One annotation attached to a completion candidate.
///
/// MARG.1 (2026-06-03): replaces the previous untyped
/// `annotations: Vec<String>` with a tagged enum so the
/// renderer can color-code each annotation by category. The
/// payload preserves semantic info (e.g. `Keybinding` keeps
/// the chord list for future affordances like "show conflicts"
/// or "click to edit binding"); display-time formatting is the
/// renderer's job via [`Annotation::display_text`].
///
/// Variants are open-by-versioning: adding a variant is a
/// minor-version bump; removing one is breaking. The `Custom`
/// variant is the escape hatch for in-tree extension crates
/// (and future WASM plugins) that don't fit any built-in
/// variant — payload includes a `slot` string the renderer
/// resolves against the theme.
///
/// `Severity` variant is intentionally omitted in MARG.1 —
/// no consumer yet (diagnostic-suggestion candidates land in
/// a later slice). Adding it when needed is non-breaking.
///
/// See `docs/dev/architecture/marginalia.md` for the data-model
/// rationale and the rejected `String + style` and pre-styled-
/// spans alternatives.
#[derive(Debug, Clone, PartialEq)]
pub enum Annotation {
    /// Category icon like `→` (motion), `:` (ex-command), `f` (file).
    /// Emitted by `KindLabelAnnotator`. Renderer styles with
    /// the kind-annotation slot.
    Kind(Arc<str>),

    /// First line of a command's doc string. Emitted by
    /// `DocSnippetAnnotator`. Renderer styles with the doc
    /// annotation slot.
    DocSnippet(Arc<str>),

    /// Chord(s) bound to this candidate's command. Emitted by
    /// the keybinding annotator (MARG.2). The renderer formats
    /// chords via [`KeyChord`]'s `Display` impl and styles with
    /// the keybinding annotation slot. Empty vec is invalid —
    /// annotators should not emit this variant when no chord
    /// binds. Most candidates have 0-1 chords; the rare
    /// multi-binding case uses `Vec` rather than `SmallVec` to
    /// avoid an extra crate dep pre-v1 — perf-driven storage
    /// swap deferred until a bench shows it matters.
    Keybinding(Vec<KeyChord>),

    /// Provenance: which crate / mode / user-config defined
    /// this command. `Arc<str>` because most candidates share
    /// the same source label (`"builtin"`, `"lsp"`,
    /// `"user-init"`); copy-by-reference is cheaper than
    /// cloning the string per-candidate.
    Source(Arc<str>),

    /// Escape hatch for plugin-contributed annotations that
    /// don't fit any built-in variant. The annotator
    /// pre-formats `text`; `slot` names a theme slot the
    /// renderer resolves (unknown slot falls back to the
    /// plugin-annotation default).
    Custom { text: Arc<str>, slot: Arc<str> },

    /// MARG §8: a single column cell whose text is colored
    /// **per segment**. Generalizes `Custom` to N slots — used
    /// for the file-permission string (`drwxr-xr-x`, one segment
    /// per bit class) and any future multi-colored field
    /// (size+unit, path head/tail, git status). Each segment
    /// carries a slot KEY, never a resolved color, so theme
    /// resolution stays at the render seam. `category` keys the
    /// column exactly like the single-variant categories.
    Styled {
        category: Arc<str>,
        segments: Vec<AnnotationSegment>,
    },
}

/// MARG §8: one run of marginalia text sharing a theme slot,
/// the unit of an [`Annotation::Styled`] cell. `slot` is a theme
/// element name the renderer resolves at paint time (unknown slot
/// → the custom/plugin annotation color); it is NEVER a baked
/// color, so a `:colorscheme` swap recolors it live on both peers.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationSegment {
    pub text: Arc<str>,
    pub slot: Arc<str>,
}

impl Annotation {
    /// Borrow-or-format the annotation's text for paint. String-
    /// payload variants return a borrowed `Cow`; structured
    /// variants (`Keybinding`) format on demand.
    pub fn display_text(&self) -> Cow<'_, str> {
        match self {
            Self::Kind(s) | Self::DocSnippet(s) | Self::Source(s) => Cow::Borrowed(s.as_ref()),
            Self::Custom { text, .. } => Cow::Borrowed(text.as_ref()),
            Self::Keybinding(chords) => {
                if chords.is_empty() {
                    Cow::Borrowed("")
                } else {
                    let mut buf = String::with_capacity(chords.len() * 4);
                    for (i, c) in chords.iter().enumerate() {
                        if i > 0 {
                            buf.push(' ');
                        }
                        use std::fmt::Write;
                        let _ = write!(&mut buf, "{c}");
                    }
                    Cow::Owned(buf)
                }
            }
            // §8: the cell's text is its segments concatenated. A
            // single segment borrows; multi-segment owns. `AnnotationColumns`
            // width math consumes this, so a styled cell occupies the same
            // column width as the equivalent flat string.
            Self::Styled { segments, .. } => match segments.as_slice() {
                [] => Cow::Borrowed(""),
                [one] => Cow::Borrowed(one.text.as_ref()),
                many => Cow::Owned(many.iter().map(|s| s.text.as_ref()).collect()),
            },
        }
    }

    /// Stable category key the renderer pattern-matches on to
    /// pick a theme slot. Variant names mirror the theme slot
    /// suffix (`annotation_kind`, `annotation_doc`, ...).
    /// `Custom` returns its `slot` field; unknown slots fall
    /// back to `annotation_plugin` at paint time.
    pub fn category(&self) -> &str {
        match self {
            Self::Kind(_) => "kind",
            Self::DocSnippet(_) => "doc",
            Self::Keybinding(_) => "keybinding",
            Self::Source(_) => "source",
            Self::Custom { slot, .. } => slot.as_ref(),
            Self::Styled { category, .. } => category.as_ref(),
        }
    }
}

/// MARG.5 (2026-06-03): pre-computed per-category column
/// layout for the picker / completion annotation column.
/// The renderer builds one of these from the visible
/// candidate set, then renders each row against it — every
/// row's annotation cells line up vertically because each
/// column width is the max across visible candidates and
/// rows that don't have a particular category render a
/// blank cell of the same width.
///
/// Why this lives in `lattice-completion` rather than the
/// renderer crates: both peer renderers (TUI + GPUI) need
/// identical column-width math; centralising avoids two
/// implementations drifting apart. The layout is data, not
/// paint — peers consume it differently (ratatui spans vs.
/// GPUI element-tree), but the column widths are universal.
///
/// Display order is variant-fixed via `category_order`:
/// keybinding -> source -> kind -> doc -> custom (custom
/// slots come last, grouped at the end). Source sits right
/// after keybinding so the user sees which mode contributes
/// the chord at a glance.
#[derive(Debug, Clone, Default)]
pub struct AnnotationColumns {
    /// (category_key, max display width in `chars`)
    /// ordered by display order.
    cols: Vec<(String, usize)>,
}

impl AnnotationColumns {
    /// Build the column layout from a borrowed iterator over
    /// the visible candidates. `chars().count()` is used for
    /// width — matches what `display_text()` yields and what
    /// monospace terminals draw. (Combining-char / wide-glyph
    /// edge cases are not handled here for parity with the
    /// existing `display_col_chars` calculation in the picker
    /// caller; a future Unicode-width pass would land in both
    /// sites at once.)
    pub fn from_visible<'a, I>(candidates: I) -> Self
    where
        I: IntoIterator<Item = &'a RenderedCandidate>,
    {
        let mut widths: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for c in candidates {
            for a in &c.annotations {
                let cat = a.category().to_string();
                let w = a.display_text().chars().count();
                let e = widths.entry(cat).or_insert(0);
                *e = (*e).max(w);
            }
        }
        let mut cols: Vec<(String, usize)> = widths.into_iter().collect();
        cols.sort_by(|(a, _), (b, _)| {
            category_order(a)
                .cmp(&category_order(b))
                .then_with(|| a.cmp(b))
        });
        Self { cols }
    }

    /// Iterate `(category_key, column_width)` pairs in
    /// display order. Renderers walk this once per row; for
    /// each column they either render the candidate's
    /// matching annotation (padded to `column_width`) or a
    /// blank cell of `column_width` spaces.
    pub fn iter(&self) -> impl Iterator<Item = (&str, usize)> {
        self.cols.iter().map(|(k, w)| (k.as_str(), *w))
    }

    /// True when no visible candidate carries any annotation.
    /// Renderers skip the annotation-column rendering
    /// entirely (including the leading pad-to-display_col
    /// spacing) when this is true.
    pub fn is_empty(&self) -> bool {
        self.cols.is_empty()
    }
}

/// Display-order rank for an annotation `category()` key.
/// Lower values render leftmost. Keybinding leads because
/// the user's eye is on the command name and chord
/// proximity is the high-value scan affordance (per the
/// MARG.2 placement-fix discussion). Unknown categories
/// (typically `Custom` `slot` strings) get the rank reserved
/// for plugin-supplied annotations.
fn category_order(category: &str) -> u8 {
    match category {
        "keybinding" => 0,
        "source" => 1,
        "kind" => 2,
        "doc" => 3,
        // MARG §8: file-metadata columns render in `ls`-style order
        // (permissions → size → mtime), to the right of any command
        // columns. Explicit ranks because the default tie-break is
        // alphabetical, which would mis-order them (mtime < perm < size).
        "perm" => 5,
        "size" => 6,
        "mtime" => 7,
        // MARG §9: picker rollout columns, to the right of file metadata.
        "location" => 8,
        "status" => 9,
        "latency" => 10,
        "args" => 11,
        "buffer-id" => 12,
        "register" => 13,
        _ => 4,
    }
}

/// What the renderer paints. Annotators append to `annotations`;
/// the renderer paints each typed [`Annotation`] with the style
/// that category resolves to, joined by two spaces of row-styled
/// padding. MARG.1 (2026-06-03): replaced `Vec<String>` with
/// `Vec<Annotation>` so annotation category survives into the
/// paint path.
#[derive(Debug, Clone)]
pub struct RenderedCandidate {
    pub raw: RawCandidate,
    pub score: MatchScore,
    pub match_ranges: Vec<Range<usize>>,
    pub annotations: Vec<Annotation>,
}

impl RenderedCandidate {
    pub fn from_scored(s: ScoredCandidate) -> Self {
        // MARG §8: carry any source-supplied marginalia through to the
        // rendered candidate. Annotators (when present) append more on
        // top; the picker runs no annotators, so for it this IS the
        // annotation source.
        let annotations = s.raw.annotations.clone();
        Self {
            raw: s.raw,
            score: s.score,
            match_ranges: s.match_ranges,
            annotations,
        }
    }
}

/// Cache key for [`crate::CandidateGenerator::cache_key`]. Treated as
/// opaque by the caching layer; semantic meaning is per-generator.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey(pub String);

impl CacheKey {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn plain_candidate_uses_text_as_display() {
        let c = RawCandidate::plain("hello", CandidateKind::Plain);
        assert_eq!(c.text, "hello");
        assert_eq!(c.display, "hello");
        assert!(matches!(c.data, CandidateData::Plain));
    }

    #[test]
    fn match_score_constants_are_ordered() {
        assert!(MatchScore::PERFECT > MatchScore::PREFIX);
        assert!(MatchScore::PREFIX > MatchScore::FUZZY_HIGH);
        assert!(MatchScore::FUZZY_HIGH > MatchScore::SUBSTRING);
        assert!(MatchScore::SUBSTRING > MatchScore::FUZZY_LOW);
    }

    #[test]
    fn from_scored_initialises_empty_annotations() {
        let scored = ScoredCandidate {
            raw: RawCandidate::plain("x", CandidateKind::Plain),
            score: MatchScore::PERFECT,
            match_ranges: vec![0..1],
        };
        let r = RenderedCandidate::from_scored(scored);
        assert!(r.annotations.is_empty());
        assert_eq!(r.match_ranges, vec![0..1]);
    }

    #[test]
    fn cache_key_round_trips() {
        let k = CacheKey::new("commands:v1");
        assert_eq!(k.0, "commands:v1");
    }

    /// Build a `RenderedCandidate` carrying the given annotations
    /// for `AnnotationColumns` layout tests.
    fn candidate_with(annotations: Vec<Annotation>) -> RenderedCandidate {
        let scored = ScoredCandidate {
            raw: RawCandidate::plain("cmd", CandidateKind::Plain),
            score: MatchScore::PERFECT,
            match_ranges: vec![],
        };
        let mut c = RenderedCandidate::from_scored(scored);
        c.annotations = annotations;
        c
    }

    #[test]
    fn columns_width_is_max_across_visible() {
        // Two candidates, same category, different widths — the
        // column width is the max so every row's cell lines up.
        let cands = vec![
            candidate_with(vec![Annotation::Kind("→".into())]),
            candidate_with(vec![Annotation::Kind(":".into())]),
        ];
        let cols = AnnotationColumns::from_visible(cands.iter());
        let kind = cols.iter().find(|(c, _)| *c == "kind").unwrap();
        assert_eq!(kind.1, "→".chars().count());
    }

    fn seg(text: &str, slot: &str) -> AnnotationSegment {
        AnnotationSegment {
            text: text.into(),
            slot: slot.into(),
        }
    }

    #[test]
    fn styled_display_text_concatenates_segments() {
        // MR.2: the cell's text is its segments joined; `category()`
        // keys the column like any single-variant annotation.
        let perm = Annotation::Styled {
            category: "perm".into(),
            segments: vec![
                seg("d", "completion.annotation.perm.type"),
                seg("rwx", "completion.annotation.perm.read"),
            ],
        };
        assert_eq!(perm.display_text(), "drwx");
        assert_eq!(perm.category(), "perm");
    }

    #[test]
    fn styled_single_segment_borrows_display_text() {
        let one = Annotation::Styled {
            category: "size".into(),
            segments: vec![seg("1.2k", "completion.annotation.size")],
        };
        assert_eq!(one.display_text(), "1.2k");
        // Empty segment list is a valid (blank) cell, not a panic.
        let empty = Annotation::Styled {
            category: "x".into(),
            segments: vec![],
        };
        assert_eq!(empty.display_text(), "");
    }

    #[test]
    fn annotation_columns_width_counts_styled_cell() {
        // A styled cell occupies the width of its concatenated text, so
        // it aligns alongside single-variant cells in the same column.
        let cands = vec![
            candidate_with(vec![Annotation::Styled {
                category: "perm".into(),
                segments: vec![seg("drwxr-xr-x", "completion.annotation.perm.type")],
            }]),
            candidate_with(vec![Annotation::Styled {
                category: "perm".into(),
                segments: vec![seg("lrwx", "completion.annotation.perm.type")],
            }]),
        ];
        let cols = AnnotationColumns::from_visible(cands.iter());
        let perm = cols.iter().find(|(c, _)| *c == "perm").unwrap();
        assert_eq!(perm.1, "drwxr-xr-x".chars().count());
    }

    #[test]
    fn columns_ordered_keybinding_then_source() {
        // Display order: keybinding -> source -> kind -> doc ->
        // custom. Source sits right after keybinding so the user
        // sees the contributing mode at a glance.
        let cands = vec![candidate_with(vec![
            Annotation::DocSnippet("docs".into()),
            Annotation::Source("builtin".into()),
            Annotation::Kind("ex".into()),
            Annotation::Keybinding(vec![]),
        ])];
        let cols = AnnotationColumns::from_visible(cands.iter());
        let order: Vec<&str> = cols.iter().map(|(c, _)| c).collect();
        assert_eq!(order, vec!["keybinding", "source", "kind", "doc"]);
    }

    #[test]
    fn columns_empty_when_no_annotations() {
        let cands = vec![candidate_with(vec![]), candidate_with(vec![])];
        let cols = AnnotationColumns::from_visible(cands.iter());
        assert!(cols.is_empty());
        assert_eq!(cols.iter().count(), 0);
    }

    #[test]
    fn columns_include_category_missing_from_some_rows() {
        // The whole point of the alignment fix: one row has a
        // keybinding, the other doesn't. The keybinding column
        // still exists (width from the row that has it) so the
        // row without one renders a blank cell of that width and
        // the kind column stays aligned across both rows.
        let cands = vec![
            candidate_with(vec![
                Annotation::Keybinding(vec![]),
                Annotation::Kind("ex".into()),
            ]),
            candidate_with(vec![Annotation::Kind("motion".into())]),
        ];
        let cols = AnnotationColumns::from_visible(cands.iter());
        let keys: Vec<&str> = cols.iter().map(|(c, _)| c).collect();
        assert_eq!(keys, vec!["keybinding", "kind"]);
        // kind width is the max across both rows.
        let kind = cols.iter().find(|(c, _)| *c == "kind").unwrap();
        assert_eq!(kind.1, 6);
    }

    #[test]
    fn columns_custom_slots_sort_after_builtins() {
        let cands = vec![candidate_with(vec![
            Annotation::Custom {
                text: "plug".into(),
                slot: "annotation_plugin".into(),
            },
            Annotation::Kind("ex".into()),
        ])];
        let cols = AnnotationColumns::from_visible(cands.iter());
        let order: Vec<&str> = cols.iter().map(|(c, _)| c).collect();
        assert_eq!(order, vec!["kind", "annotation_plugin"]);
    }
}
