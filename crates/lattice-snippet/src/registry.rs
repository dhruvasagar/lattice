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
    /// The pack the snippet came from (`"rust"`, or `"*"` for the
    /// any-language pack).
    pub language: String,
}

impl SnippetMeta {
    /// The snippet's identity, for a picker row to hand back on accept.
    pub fn key(&self) -> String {
        snippet_key(&self.language, &self.name)
    }
}

/// A snippet's identity as one string: its pack's language and its name.
/// Names are unique within a pack only — friendly-snippets has an "if" in
/// most languages — so a candidate or picker row that carried the name
/// alone expanded whichever pack loaded last (snippet-integrity SI.5).
pub fn snippet_key(language: &str, name: &str) -> String {
    format!("{language}{KEY_SEPARATOR}{name}")
}

/// Unit separator: cannot appear in a language id, and a snippet name
/// containing it would be a strange name indeed.
const KEY_SEPARATOR: char = '\u{1f}';

/// The packs to consult for a buffer whose language id is `language`, most
/// specific first. A React file reads its own pack and then the base
/// language's, as friendly-snippets' `package.json` maps them; plain text
/// also answers to the LSP id `plaintext`. The any-language `*` pack is
/// always consulted last by the registry itself.
pub fn lookup_languages(language: &str) -> Vec<&str> {
    match language {
        "" => Vec::new(),
        "typescriptreact" => vec!["typescriptreact", "typescript"],
        "javascriptreact" => vec!["javascriptreact", "javascript"],
        "text" => vec!["text", "plaintext"],
        other => vec![other],
    }
}

/// Per-language store. Snippets index by every prefix they
/// register (multi-prefix snippets register under each).
#[derive(Debug, Default, Clone)]
pub struct SnippetRegistry {
    /// `language -> { prefix -> snippets }`, newest first in each list,
    /// so a pack loaded later (the user's, after the builtins) answers a
    /// shared prefix first.
    by_language: HashMap<String, HashMap<String, Vec<Snippet>>>,
    /// Every snippet by its identity, `(language, name)`.
    by_key: HashMap<(String, String), Snippet>,
}

impl SnippetRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a snippet into `language`'s pack, under each of its
    /// prefixes. A snippet already there under the same name is replaced
    /// — a user pack redefining a builtin wins — and a new one goes ahead
    /// of the snippets already sharing its prefix, so `<C-x><C-s>` expands
    /// the most recently loaded.
    pub fn insert(&mut self, language: &str, snippet: Snippet) {
        let key = (language.to_string(), snippet.name.clone());
        let by_prefix = self.by_language.entry(language.to_string()).or_default();
        if let Some(old) = self.by_key.remove(&key) {
            for prefix in &old.prefixes {
                if let Some(snips) = by_prefix.get_mut(prefix) {
                    snips.retain(|s| s.name != old.name);
                }
            }
        }
        for prefix in &snippet.prefixes {
            by_prefix
                .entry(prefix.clone())
                .or_default()
                .insert(0, snippet.clone());
        }
        self.by_key.insert(key, snippet);
    }

    /// Snippets whose prefix is exactly `prefix`, across `languages` in
    /// order and then the `*` pack, each with the pack it came from.
    pub fn lookup<'a>(&'a self, languages: &[&str], prefix: &str) -> Vec<(&'a str, &'a Snippet)> {
        let mut out = Vec::new();
        for (language, by_prefix) in self.packs(languages) {
            if let Some(snips) = by_prefix.get(prefix) {
                out.extend(snips.iter().map(|s| (language, s)));
            }
        }
        out
    }

    /// Every snippet whose prefix STARTS with `query` (case-insensitive),
    /// across `languages` and then `*`, each with its pack. Used by the
    /// `gen:snippet` source to populate the completion popup -- the host's
    /// matcher takes over from there. A name appears once: a multi-prefix
    /// snippet matching twice, or a more general pack's snippet of the
    /// same name, gives way to the first (most specific) one.
    pub fn matching_prefix<'a>(
        &'a self,
        languages: &[&str],
        query: &str,
    ) -> Vec<(&'a str, &'a Snippet)> {
        let q = query.to_lowercase();
        let mut out = Vec::new();
        let mut seen: std::collections::HashSet<&'a str> = std::collections::HashSet::new();
        for (language, by_prefix) in self.packs(languages) {
            for (prefix, snips) in by_prefix {
                if !prefix.to_lowercase().starts_with(&q) {
                    continue;
                }
                for s in snips {
                    if seen.insert(s.name.as_str()) {
                        out.push((language, s));
                    }
                }
            }
        }
        out
    }

    /// The snippet `name` in `language`'s pack.
    pub fn get(&self, language: &str, name: &str) -> Option<&Snippet> {
        self.by_key.get(&(language.to_string(), name.to_string()))
    }

    /// Resolve a [`snippet_key`]. A bare name (no pack — a plugin routing
    /// `expand-snippet` by name) is looked for in `languages` and then `*`.
    pub fn resolve(&self, languages: &[&str], key: &str) -> Option<&Snippet> {
        match key.split_once(KEY_SEPARATOR) {
            Some((language, name)) => self.get(language, name),
            None => languages
                .iter()
                .copied()
                .chain(std::iter::once("*"))
                .find_map(|language| self.get(language, key)),
        }
    }

    /// How many snippets are registered, across every pack.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// Languages with at least one registered snippet.
    pub fn languages(&self) -> Vec<&str> {
        self.by_language.keys().map(|s| s.as_str()).collect()
    }

    /// Display-only metadata for every snippet a buffer in `languages`
    /// can expand — its own packs and `*` — without forcing a body clone.
    /// One row per prefix.
    pub fn meta_for(&self, languages: &[&str]) -> Vec<SnippetMeta> {
        let mut out = Vec::new();
        for (language, by_prefix) in self.packs(languages) {
            for (prefix, snips) in by_prefix {
                for s in snips {
                    out.push(SnippetMeta {
                        name: s.name.clone(),
                        prefix: prefix.clone(),
                        description: s.description.clone(),
                        language: language.to_string(),
                    });
                }
            }
        }
        out
    }

    /// The packs for `languages`, then `*`, that exist, with their names.
    fn packs<'a>(
        &'a self,
        languages: &[&str],
    ) -> impl Iterator<Item = (&'a str, &'a HashMap<String, Vec<Snippet>>)> {
        let mut names: Vec<&str> = languages.iter().copied().filter(|l| *l != "*").collect();
        names.push("*");
        names
            .into_iter()
            .filter_map(|l| self.by_language.get_key_value(l))
            .map(|(name, pack)| (name.as_str(), pack))
            .collect::<Vec<_>>()
            .into_iter()
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

    fn names<'a>(hits: &[(&'a str, &'a Snippet)]) -> Vec<(&'a str, &'a str)> {
        hits.iter().map(|(l, s)| (*l, s.name.as_str())).collect()
    }

    #[test]
    fn lookup_returns_matching_snippet() {
        let mut r = SnippetRegistry::new();
        r.insert(
            "rust",
            snip("for-loop", "for", "for ${1:i} in ${2:iter} {}"),
        );
        assert_eq!(
            names(&r.lookup(&["rust"], "for")),
            vec![("rust", "for-loop")]
        );
    }

    #[test]
    fn matching_prefix_walks_per_language_then_star() {
        let mut r = SnippetRegistry::new();
        r.insert("rust", snip("rust-for", "for", "for $1"));
        r.insert("*", snip("anywhere", "fn", "fn $1"));
        let hits = r.matching_prefix(&["rust"], "f");
        let found = names(&hits);
        assert!(found.contains(&("rust", "rust-for")));
        assert!(found.contains(&("*", "anywhere")));
    }

    #[test]
    fn multi_prefix_snippets_register_under_each() {
        let mut r = SnippetRegistry::new();
        let mut s = snip("for-or-fr", "for", "for $1");
        s.prefixes = vec!["for".into(), "fr".into()];
        r.insert("rust", s);
        assert_eq!(r.lookup(&["rust"], "for").len(), 1);
        assert_eq!(r.lookup(&["rust"], "fr").len(), 1);
    }

    #[test]
    fn matching_prefix_dedups_by_snippet_name() {
        let mut r = SnippetRegistry::new();
        let mut s = snip("for-or-fr", "for", "for $1");
        s.prefixes = vec!["for".into(), "fr".into()];
        r.insert("rust", s);
        // Query "f" matches both "for" and "fr" prefix slots
        // pointing at the same snippet -- dedup via name.
        assert_eq!(r.matching_prefix(&["rust"], "f").len(), 1);
    }

    /// SI.5: the same name in two packs is two snippets. Before, one map
    /// keyed by name alone let the last pack loaded win everywhere.
    #[test]
    fn the_same_name_in_two_languages_is_two_snippets() {
        let mut r = SnippetRegistry::new();
        r.insert("rust", snip("if", "if", "if $1 {}"));
        r.insert("python", snip("if", "if", "if $1:"));
        assert_eq!(r.len(), 2);
        let rust = r.get("rust", "if").expect("rust's");
        assert_eq!(rust.body, parse::parse("if $1 {}").unwrap());
        let key = snippet_key("rust", "if");
        assert_eq!(
            r.resolve(&["python"], &key).map(|s| &s.body),
            Some(&rust.body)
        );
    }

    /// SI.5: a pack loaded later — the user's, after the builtins — wins a
    /// shared prefix, and replaces a snippet it redefines by name.
    #[test]
    fn a_later_pack_overrides_an_earlier_one() {
        let mut r = SnippetRegistry::new();
        r.insert("rust", snip("for-loop", "for", "builtin"));
        r.insert("rust", snip("my-for", "for", "mine"));
        assert_eq!(
            names(&r.lookup(&["rust"], "for")),
            vec![("rust", "my-for"), ("rust", "for-loop")]
        );
        r.insert("rust", snip("for-loop", "fl", "redefined"));
        assert_eq!(names(&r.lookup(&["rust"], "for")), vec![("rust", "my-for")]);
        assert_eq!(
            names(&r.lookup(&["rust"], "fl")),
            vec![("rust", "for-loop")]
        );
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn a_react_file_reads_its_own_pack_then_the_base_language() {
        let mut r = SnippetRegistry::new();
        r.insert("typescript", snip("iface", "iface", "interface $1 {}"));
        r.insert("typescriptreact", snip("comp", "comp", "<$1 />"));
        let langs = lookup_languages("typescriptreact");
        assert_eq!(r.lookup(&langs, "iface").len(), 1);
        assert_eq!(r.lookup(&langs, "comp").len(), 1);
    }

    #[test]
    fn a_bare_name_resolves_in_the_buffer_languages_then_star() {
        let mut r = SnippetRegistry::new();
        r.insert("*", snip("todo", "todo", "TODO: $0"));
        r.insert("rust", snip("for", "for", "for $1"));
        assert!(r.resolve(&["rust"], "for").is_some());
        assert!(r.resolve(&["rust"], "todo").is_some());
        assert!(r.resolve(&["python"], "for").is_none());
    }

    #[test]
    fn meta_covers_the_language_and_star_and_omits_body() {
        let mut r = SnippetRegistry::new();
        r.insert("rust", snip("for", "for", "for $1"));
        r.insert("*", snip("todo", "todo", "TODO: $0"));
        r.insert("python", snip("def", "def", "def $1():"));
        let mut meta = r.meta_for(&["rust"]);
        meta.sort_by(|a, b| a.name.cmp(&b.name));
        let rows: Vec<(&str, &str)> = meta
            .iter()
            .map(|m| (m.language.as_str(), m.name.as_str()))
            .collect();
        assert_eq!(rows, vec![("rust", "for"), ("*", "todo")]);
        assert_eq!(meta[0].key(), snippet_key("rust", "for"));
    }
}
