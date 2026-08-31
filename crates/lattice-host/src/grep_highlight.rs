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
use lattice_syntax::{Lang, Syntax};

/// Per-language grammar cache backing the grep preview highlighter.
/// `None` in the map means "this language has no registered grammar" —
/// cached so repeated hits in an unsupported file don't re-probe the
/// registry. Wrapped in a `Mutex` because the trait is `Sync` and grep
/// runs are serial within a task but may overlap across runs.
pub struct SyntaxGrepHighlighter {
    cache: Mutex<HashMap<Lang, Option<Syntax>>>,
}

impl SyntaxGrepHighlighter {
    /// AH.1: takes no registry. It used to take the editor's shared
    /// `Arc<LangRegistry>` "so grep previews highlight with exactly the
    /// grammars the buffers use" — which is what it stopped doing the moment
    /// plugin languages existed.
    ///
    /// `LangRegistry::standard()` *is* `registry::live()`: it returns a
    /// SNAPSHOT of the process-global ArcSwap. A plugin's grammar arrives
    /// later via `install_plugin_config`, which RCUs a new registry in and
    /// leaves every held `Arc` on the pre-plugin value. This highlighter was
    /// built at boot, so its registry was bundled-only forever — every
    /// preview of a `.org` hit (the org-roam node picker's whole surface)
    /// painted plain.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl Default for SyntaxGrepHighlighter {
    fn default() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
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
        // AH.1: the LIVE registry, re-read per call. Wait-free (one ArcSwap
        // load), and the only way a grammar registered after boot is ever
        // seen.
        let Ok(live) = lattice_syntax::registry::live() else {
            return Vec::new();
        };
        // Recover a poisoned lock rather than propagate a panic onto the
        // grep task — a highlight failure must degrade to plain preview.
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        // The cache stores NEGATIVES ("no grammar for this language") so
        // repeated hits in an unsupported file do not re-probe. That is safe
        // across a plugin registration, which is worth stating because it
        // looks like it should not be: the key is `Lang`, and a
        // `Lang::Plugin(name)` only *exists* while that name is registered.
        // Before registration the extension resolves to `Lang::Plain` and
        // returns above without touching the cache; after withdrawal it does
        // so again. So a registration never has to invalidate an entry — it
        // introduces a key that could not have been cached under.
        let entry = cache.entry(lang).or_insert_with(|| {
            Syntax::for_language_with_registry(lang, live)
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
        SyntaxGrepHighlighter::new()
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

    /// **AH.1: a grammar registered AFTER the highlighter was built must
    /// reach previews.**
    ///
    /// This is the org-roam node picker painting every `.org` preview plain.
    /// The highlighter held an `Arc<LangRegistry>` captured at boot, and
    /// `LangRegistry::standard()` returns a SNAPSHOT — a plugin's
    /// `install_plugin_config` RCUs a NEW registry into the process-global
    /// ArcSwap, so every previously-held `Arc` keeps pointing at the
    /// bundled-only value from before the plugin loaded.
    ///
    /// The first assertion is the mechanism itself, stated so it cannot rot
    /// into a coincidence: registration REPLACES the registry rather than
    /// mutating it, which is exactly why holding one is wrong. The second is
    /// the behaviour that depends on it.
    #[test]
    fn a_grammar_registered_after_boot_reaches_previews() {
        const PROV: u64 = 0xA11_7E57;
        let h = highlighter();
        let path = PathBuf::from("notes.testlang-ah1");

        // Held at "boot", exactly as the pre-AH.1 highlighter did.
        let held_at_boot = lattice_syntax::LangRegistry::standard().unwrap();
        assert!(
            h.highlight_line(&path, "fn main() {}").is_empty(),
            "sanity: the extension is unclaimed before registration"
        );

        let spec = lattice_syntax::registry::GrammarSpec {
            grammar: tree_sitter_rust::LANGUAGE.into(),
            highlights: Some(tree_sitter_rust::HIGHLIGHTS_QUERY.to_string()),
            folds: None,
            injections: None,
            indents: None,
            textobjects: None,
            conceal_rules: Vec::new(),
        };
        lattice_syntax::plugin_lang::register_with_grammar(
            "testlang-ah1",
            &["testlang-ah1"],
            &spec,
            PROV,
        )
        .expect("the test language registers");

        // THE MECHANISM: registration swapped in a different registry, so the
        // `Arc` captured above can never see it. Any code holding one is
        // reading the pre-plugin world forever.
        let now_live = lattice_syntax::LangRegistry::standard().unwrap();
        assert!(
            !Arc::ptr_eq(&held_at_boot, &now_live),
            "registration must REPLACE the live registry — if it mutated in \
             place, holding an Arc would have been safe and AH.1 would be \
             solving a non-problem"
        );
        assert!(
            lattice_syntax::Syntax::for_language_with_registry(
                Lang::detect_from_path(Some(&path)),
                held_at_boot,
            )
            .is_ok_and(|o| o.is_none()),
            "…and the held snapshot still does not know the language"
        );

        // THE BEHAVIOUR: the preview highlights anyway, because the
        // highlighter reads live.
        assert!(
            !h.highlight_line(&path, "fn main() {}").is_empty(),
            "a grammar registered after boot must reach previews"
        );

        // Withdrawal is visible too. Note WHY, since it is not the cache
        // being invalidated: `unregister_plugin` withdraws the extension
        // mapping as well, so the path resolves to `Lang::Plain` again and
        // returns before the cache is consulted. The cached entry is keyed by
        // a `Lang::Plugin` that no longer exists and is unreachable rather
        // than stale — which is what makes a cache-invalidation pass
        // unnecessary here.
        lattice_syntax::plugin_lang::unregister_plugin(PROV);
        assert!(
            h.highlight_line(&path, "fn main() {}").is_empty(),
            "a withdrawn grammar must stop highlighting"
        );
    }
}
