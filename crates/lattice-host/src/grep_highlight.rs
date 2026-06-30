//! PH.3: concrete grep-preview highlighter.
//!
//! `lattice-picker` defines the [`GrepPreviewHighlighter`] trait but has
//! no `lattice-syntax` dependency — that absence is the structural
//! off-thread guarantee (a picker source physically cannot parse on the
//! render thread). The host owns `lattice-syntax`, so the concrete impl
//! lives here and is injected into `GrepSource` at boot.
//!
//! Grep hits come from arbitrary files (not the active buffer's parsed
//! tree), so each preview line is highlighted by selecting a grammar
//! from the file extension and parsing the single line. The expensive
//! part — compiling a grammar's highlight query — is cached per
//! language across calls (and across live-grep keystrokes), so steady
//! state is just a short single-line parse per hit. All of this runs on
//! the grep blocking task (`GrepSource::spawn_grep`), never the render
//! thread.
//!
//! See `docs/dev/architecture/picker-preview-highlight.md` §7.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use lattice_completion::DisplaySpan;
use lattice_picker::picker_sources::GrepPreviewHighlighter;
use lattice_syntax::{Lang, LangRegistry, Syntax};

/// Per-language grammar cache backing the grep preview highlighter.
/// `None` in the map means "this language has no registered grammar" —
/// cached so repeated hits in an unsupported file don't re-probe the
/// registry. Wrapped in a `Mutex` because the trait is `Sync` and grep
/// runs are serial within a task but may overlap across runs.
pub struct SyntaxGrepHighlighter {
    lang_registry: Arc<LangRegistry>,
    cache: Mutex<HashMap<Lang, Option<Syntax>>>,
}

impl SyntaxGrepHighlighter {
    /// Build from the editor's shared `LangRegistry` so grep previews
    /// highlight with exactly the grammars the buffers use.
    pub fn new(lang_registry: Arc<LangRegistry>) -> Arc<Self> {
        Arc::new(Self {
            lang_registry,
            cache: Mutex::new(HashMap::new()),
        })
    }
}

impl GrepPreviewHighlighter for SyntaxGrepHighlighter {
    fn highlight_line(&self, path: &Path, line: &str) -> Vec<DisplaySpan> {
        // Grammar by extension. `Lang::Plain` (and any extension we
        // don't recognise) → no highlighting, plain preview.
        let lang = Lang::detect_from_path(Some(path));
        if lang == Lang::Plain || line.is_empty() {
            return Vec::new();
        }
        // Recover a poisoned lock rather than propagate a panic onto the
        // grep task — a highlight failure must degrade to plain preview.
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let entry = cache.entry(lang).or_insert_with(|| {
            Syntax::for_language_with_registry(lang, self.lang_registry.clone())
                .ok()
                .flatten()
        });
        let Some(syntax) = entry.as_mut() else {
            return Vec::new(); // no grammar for this language
        };
        // Re-parse just this line under the cached grammar (the compiled
        // highlight query is what `Syntax` already holds). `line` IS the
        // candidate `display`, so spans come back display-relative.
        syntax.parse_at(line, 0);
        let Ok(per_line) = syntax.highlight_lines(0, 1) else {
            return Vec::new();
        };
        let Some(spans) = per_line.into_iter().next() else {
            return Vec::new();
        };
        spans
            .into_iter()
            .filter(|s| s.start < line.len())
            .map(|s| DisplaySpan {
                range: s.start..s.end.min(line.len()),
                style: s.style,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::path::PathBuf;

    fn highlighter() -> Arc<SyntaxGrepHighlighter> {
        SyntaxGrepHighlighter::new(LangRegistry::standard().unwrap())
    }

    /// PH.3: a Rust grep hit's preview is highlighted display-relative,
    /// keyword colored, all spans within the line length.
    #[test]
    fn highlights_rust_preview_display_relative() {
        let h = highlighter();
        let line = "let x = 1;";
        let spans = h.highlight_line(&PathBuf::from("src/main.rs"), line);
        assert!(!spans.is_empty(), "a rust line should carry syntax spans");
        assert!(
            spans.iter().all(|s| s.range.end <= line.len()),
            "spans stay within the display run"
        );
        assert!(
            spans
                .iter()
                .any(|s| s.style == lattice_syntax::Style::Keyword),
            "`let` resolves to the Keyword style"
        );
    }

    /// PH.3: an unrecognised extension (or no extension) → plain
    /// preview (no grammar), never a panic.
    #[test]
    fn unknown_extension_is_plain() {
        let h = highlighter();
        assert!(
            h.highlight_line(&PathBuf::from("notes.unknownext"), "let x = 1;")
                .is_empty()
        );
        assert!(
            h.highlight_line(&PathBuf::from("README"), "plain text line")
                .is_empty()
        );
    }

    /// PH.3: empty preview → no spans.
    #[test]
    fn empty_line_is_plain() {
        let h = highlighter();
        assert!(
            h.highlight_line(&PathBuf::from("src/main.rs"), "")
                .is_empty()
        );
    }

    /// PH.3: the per-language grammar cache is reused across calls
    /// (second call for the same language hits the cache, not a fresh
    /// `for_language_with_registry`). Observable as identical output.
    #[test]
    fn caches_grammar_across_calls() {
        let h = highlighter();
        let a = h.highlight_line(&PathBuf::from("a.rs"), "fn main() {}");
        let b = h.highlight_line(&PathBuf::from("b.rs"), "fn main() {}");
        assert_eq!(a, b, "same grammar + same line → identical spans");
        assert!(!a.is_empty());
    }
}
