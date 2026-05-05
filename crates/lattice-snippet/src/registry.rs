//! Per-language snippet registry. Stores parsed bodies
//! keyed by trigger prefix; the host's `gen:snippet` source
//! consults this registry per-popup-trigger.

use std::collections::HashMap;

use crate::token::SnippetBody;

/// One snippet entry. `prefixes` may have multiple entries
/// (TextMate JSON `"prefix": ["loop", "fr"]` form). Body is
/// pre-parsed; the load path runs the parser once at startup.
#[derive(Debug, Clone)]
pub struct Snippet {
    pub name: String,
    pub prefixes: Vec<String>,
    pub body: SnippetBody,
    pub description: Option<String>,
    /// Comma-separated source-scope filter (e.g.
    /// `"source.rust,source.markdown"`). Empty = all-language.
    /// Host maps `source.<lang>` to the buffer's tree-sitter
    /// language at filter time.
    pub scope: String,
}

/// Display-only metadata for a snippet -- the bits the
/// completion popup row needs without pulling in the body.
#[derive(Debug, Clone)]
pub struct SnippetMeta {
    pub name: String,
    pub prefix: String,
    pub description: Option<String>,
}

/// Per-language store. Snippets index by every prefix they
/// register (multi-prefix snippets register under each).
#[derive(Debug, Default)]
pub struct SnippetRegistry {
    /// `language -> { prefix -> snippet }`. v1 is per-language;
    /// scope-expression filtering (e.g. `source.markdown.injection.rust`)
    /// rides on the same shape once the major-mode plumbing exists.
    by_language: HashMap<String, HashMap<String, Vec<Snippet>>>,
    /// All snippets keyed by name -- handy for `:reload-snippets`
    /// dedup and `:describe-snippet <name>` lookups.
    all_by_name: HashMap<String, Snippet>,
}

impl SnippetRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a snippet for a given language. When the snippet
    /// has multiple prefixes, registers under each. Languages
    /// are matched verbatim against the host's scope -> language
    /// mapping; future scope-expression support extends this.
    pub fn insert(&mut self, language: &str, snippet: Snippet) {
        let by_prefix = self
            .by_language
            .entry(language.to_string())
            .or_default();
        for prefix in &snippet.prefixes {
            by_prefix
                .entry(prefix.clone())
                .or_default()
                .push(snippet.clone());
        }
        self.all_by_name
            .insert(snippet.name.clone(), snippet);
    }

    /// Snippets matching `prefix` for `language`. Walks the
    /// per-language index then a `*` (any-language) index. Each
    /// snippet appears once even when its prefix slot has
    /// multiple registrations (different snippets sharing a
    /// prefix is allowed -- popular for "if-else" vs "if" both
    /// triggering on `if`).
    pub fn lookup(&self, language: &str, prefix: &str) -> Vec<&Snippet> {
        let mut out: Vec<&Snippet> = Vec::new();
        if let Some(by_prefix) = self.by_language.get(language)
            && let Some(snips) = by_prefix.get(prefix)
        {
            out.extend(snips.iter());
        }
        if let Some(by_prefix) = self.by_language.get("*")
            && let Some(snips) = by_prefix.get(prefix)
        {
            out.extend(snips.iter());
        }
        out
    }

    /// Every snippet whose prefix STARTS with the given
    /// `query` (case-insensitive) for the given language.
    /// Used by the `gen:snippet` source to populate the
    /// completion popup -- the host's matcher takes over
    /// from there.
    pub fn matching_prefix<'a>(
        &'a self,
        language: &str,
        query: &str,
    ) -> Vec<&'a Snippet> {
        let q = query.to_lowercase();
        let mut out: Vec<&'a Snippet> = Vec::new();
        let mut seen: std::collections::HashSet<&'a str> =
            std::collections::HashSet::new();
        for source_lang in [language, "*"] {
            let Some(by_prefix) = self.by_language.get(source_lang) else {
                continue;
            };
            for (prefix, snips) in by_prefix {
                if !prefix.to_lowercase().starts_with(&q) {
                    continue;
                }
                for s in snips {
                    if seen.insert(s.name.as_str()) {
                        out.push(s);
                    }
                }
            }
        }
        out
    }

    pub fn by_name(&self, name: &str) -> Option<&Snippet> {
        self.all_by_name.get(name)
    }

    pub fn len(&self) -> usize {
        self.all_by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all_by_name.is_empty()
    }

    /// Languages with at least one registered snippet.
    pub fn languages(&self) -> Vec<&str> {
        self.by_language.keys().map(|s| s.as_str()).collect()
    }

    /// Display-only metadata view -- `:list-snippets` /
    /// `:describe-snippet` consume this without forcing a
    /// body clone.
    pub fn meta_for_language(&self, language: &str) -> Vec<SnippetMeta> {
        let mut out: Vec<SnippetMeta> = Vec::new();
        if let Some(by_prefix) = self.by_language.get(language) {
            for (prefix, snips) in by_prefix {
                for s in snips {
                    out.push(SnippetMeta {
                        name: s.name.clone(),
                        prefix: prefix.clone(),
                        description: s.description.clone(),
                    });
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn snip(name: &str, prefix: &str, body: &str) -> Snippet {
        Snippet {
            name: name.into(),
            prefixes: vec![prefix.into()],
            body: parse::parse(body).unwrap(),
            description: None,
            scope: String::new(),
        }
    }

    #[test]
    fn lookup_returns_matching_snippet() {
        let mut r = SnippetRegistry::new();
        r.insert("rust", snip("for-loop", "for", "for ${1:i} in ${2:iter} {}"));
        let hits = r.lookup("rust", "for");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "for-loop");
    }

    #[test]
    fn matching_prefix_walks_per_language_then_star() {
        let mut r = SnippetRegistry::new();
        r.insert("rust", snip("rust-for", "for", "for $1"));
        r.insert("*", snip("anywhere", "fn", "fn $1"));
        let hits = r.matching_prefix("rust", "f");
        let names: Vec<&str> = hits.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"rust-for"));
        assert!(names.contains(&"anywhere"));
    }

    #[test]
    fn multi_prefix_snippets_register_under_each() {
        let mut r = SnippetRegistry::new();
        let mut s = snip("for-or-fr", "for", "for $1");
        s.prefixes = vec!["for".into(), "fr".into()];
        r.insert("rust", s);
        assert_eq!(r.lookup("rust", "for").len(), 1);
        assert_eq!(r.lookup("rust", "fr").len(), 1);
    }

    #[test]
    fn matching_prefix_dedups_by_snippet_name() {
        let mut r = SnippetRegistry::new();
        let mut s = snip("for-or-fr", "for", "for $1");
        s.prefixes = vec!["for".into(), "fr".into()];
        r.insert("rust", s);
        // Query "f" matches both "for" and "fr" prefix slots
        // pointing at the same snippet -- dedup via name.
        let hits = r.matching_prefix("rust", "f");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn meta_for_language_omits_body() {
        let mut r = SnippetRegistry::new();
        r.insert("rust", snip("for", "for", "for $1"));
        let meta = r.meta_for_language("rust");
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].name, "for");
        assert_eq!(meta[0].prefix, "for");
    }
}
