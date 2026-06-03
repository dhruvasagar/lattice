//! Help topic registry (DESIGN.md §5.11).
//!
//! Hand-written, free-form help docs (the `:help <topic>` surface,
//! distinct from the introspection-driven `:describe-*` views) live
//! here. v1 ships a small built-in set sourced from `docs/user/*.md`
//! at build time via `include_str!`; plugins / future LSP / config
//! extensions register additional topics through the same registry,
//! so `:help` becomes the single user-facing entry point for
//! discoverable documentation regardless of who supplies it.
//!
//! The registry is intentionally indirection-friendly:
//!
//! - [`HelpTopicBody::Static`] embeds compile-time markdown.
//! - [`HelpTopicBody::Dynamic`] takes a closure that produces text
//!   on demand -- this is the seam for LSP-driven topics
//!   (`:help symbol::Foo`), in-process introspection that can't be
//!   captured at compile time, or any plugin-supplied source.
//!
//! Topics also carry an optional list of substring patterns that
//! match command names; `:describe-command` walks these to emit a
//! `See also: [topic](help:topic)` cross-link when a primitive
//! covered by a topic is described.

use std::collections::HashMap;
use std::sync::Arc;

use lattice_completion::candidate::{CandidateData, CandidateKind, RawCandidate};
use lattice_completion::traits::{CandidateGenerator, GenerateContext};

/// One free-form help topic.
pub struct HelpTopic {
    pub name: String,
    pub summary: String,
    pub body: HelpTopicBody,
    /// Substring patterns matched against command names by
    /// `:describe-command`. When any pattern is a substring of the
    /// command name (case-sensitive), the describe view emits a
    /// "See also" link to this topic.
    pub related_command_patterns: Vec<String>,
}

impl std::fmt::Debug for HelpTopic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelpTopic")
            .field("name", &self.name)
            .field("summary", &self.summary)
            .field("related_command_patterns", &self.related_command_patterns)
            .field(
                "body_kind",
                &match &self.body {
                    HelpTopicBody::Static(_) => "static",
                    HelpTopicBody::Dynamic(_) => "dynamic",
                },
            )
            .finish()
    }
}

/// Where a topic's body comes from. `Static` is a compile-time
/// `&'static str` (most v1 topics); `Dynamic` is a closure invoked
/// each time the topic is opened (the seam for LSP / introspection
/// / plugin-supplied content).
pub enum HelpTopicBody {
    Static(&'static str),
    Dynamic(Box<dyn Fn() -> String + Send + Sync>),
}

impl HelpTopicBody {
    pub fn render(&self) -> String {
        match self {
            HelpTopicBody::Static(s) => s.to_string(),
            HelpTopicBody::Dynamic(f) => f(),
        }
    }
}

/// Catalogue of every registered topic, keyed by name. Plugins +
/// future LSP integrations register through `register`. Held by
/// the App as `Arc<RwLock<HelpTopicRegistry>>` would let plugins
/// add topics at runtime; v1 holds `Arc<HelpTopicRegistry>` since
/// the built-in set is fixed at startup.
#[derive(Debug, Default)]
pub struct HelpTopicRegistry {
    by_name: HashMap<String, HelpTopic>,
    /// Insertion order so the index can list topics in the order
    /// the host registered them (built-ins first).
    order: Vec<String>,
}

impl HelpTopicRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, topic: HelpTopic) {
        if !self.by_name.contains_key(&topic.name) {
            self.order.push(topic.name.clone());
        }
        self.by_name.insert(topic.name.clone(), topic);
    }

    pub fn lookup(&self, name: &str) -> Option<&HelpTopic> {
        self.by_name.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.order.iter().map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = &HelpTopic> {
        self.order.iter().filter_map(|n| self.by_name.get(n))
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Find every topic whose `related_command_patterns` contains
    /// a substring of `command_name`. Used by
    /// `:describe-command` to emit cross-links. Stable insertion
    /// order; multiple topics can match.
    pub fn topics_for_command<'a>(
        &'a self,
        command_name: &'a str,
    ) -> impl Iterator<Item = &'a HelpTopic> + 'a {
        self.iter().filter(move |t| {
            t.related_command_patterns
                .iter()
                .any(|p| command_name.contains(p))
        })
    }
}

/// `gen:help-topics`. Returns one `RawCandidate` per registered
/// help topic so `:help <Tab>` enumerates available topics. The
/// payload is `CandidateData::Plain` -- topic name alone is enough
/// for the v1 popup; future polish can introduce a richer
/// `HelpTopic` variant if summaries need to flow through the
/// matcher / annotator pipeline.
pub struct HelpTopicsGenerator {
    pub topics: Arc<HelpTopicRegistry>,
}

impl CandidateGenerator for HelpTopicsGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        self.topics
            .iter()
            .map(|t| RawCandidate {
                text: t.name.clone(),
                display: t.name.clone(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
            })
            .collect()
    }
}

// Generated by `build.rs` from `docs/user/**/*.md` (module scope so
// the emitted `static` is an item). Shape:
//   static HELP_TOPICS: &[(name, summary, related, body)]
// `README.md` maps to the `index` topic (reached by bare `:help`).
include!(concat!(env!("OUT_DIR"), "/help_topics.rs"));

/// The built-in topic set, generated at build time by `build.rs`
/// from every `docs/user/**/*.md` (recursively, skipping `tutor/`).
/// Each doc's `---` YAML frontmatter supplies `summary` + `related`
/// (see the build script); bodies are embedded as string literals
/// so the binary stays self-contained — no runtime filesystem
/// dependency. Adding a doc requires **no change here**: drop the
/// `.md` into `docs/user/` and it registers automatically.
pub fn builtin_topics() -> Arc<HelpTopicRegistry> {
    let mut r = HelpTopicRegistry::new();
    for &(name, summary, related, body) in HELP_TOPICS {
        r.register(HelpTopic {
            name: name.to_string(),
            summary: summary.to_string(),
            body: HelpTopicBody::Static(body),
            related_command_patterns: related.iter().map(|s| (*s).to_string()).collect(),
        });
    }
    Arc::new(r)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_topics_include_the_index() {
        let r = builtin_topics();
        assert!(r.lookup("index").is_some());
    }

    #[test]
    fn builtin_topics_include_folding_and_buffers() {
        let r = builtin_topics();
        assert!(r.lookup("folding").is_some());
        assert!(r.lookup("buffers").is_some());
    }

    /// Soft binary-size budget for embedded user docs. When the total
    /// size of `docs/user/*.md` exceeds this threshold, this test
    /// fails and forces a re-evaluation of the `include_str!`-based
    /// embedding model.
    ///
    /// Rationale (see `docs/dev/operations/embedded-docs-budget.md`):
    /// every user doc is currently `include_str!`'d uncompressed into
    /// the binary, which gives the editor the "works offline, no
    /// filesystem layout assumption" property (paramount goal). The
    /// cost is linear in doc volume. At 166 KB of markdown today,
    /// the cost is ~0.5-1.5% of a typical release binary — invisible.
    /// At 3× that (500 KB) the cost becomes visible; that's the
    /// trigger to switch to compressed-embed (gzip/deflate, ~5×
    /// reduction) before the bloat is real.
    ///
    /// **Action when this test fails:** pick one of the options
    /// in `docs/dev/operations/embedded-docs-budget.md` (compress,
    /// feature-gate, lazy-load) and implement it. Do NOT just bump
    /// the budget number.
    const EMBEDDED_DOCS_BUDGET_BYTES: u64 = 512 * 1024;

    #[test]
    fn embedded_user_docs_stay_under_size_budget() {
        let docs_user = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/user");
        let mut total: u64 = 0;
        let mut per_file: Vec<(String, u64)> = Vec::new();
        for entry in std::fs::read_dir(&docs_user).expect("docs/user readable") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let meta = std::fs::metadata(&path).expect("stat md file");
            total += meta.len();
            per_file.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                meta.len(),
            ));
        }
        per_file.sort_by(|a, b| b.1.cmp(&a.1));
        let detail = per_file
            .iter()
            .map(|(n, s)| format!("    {:>7} B  {}", s, n))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            total <= EMBEDDED_DOCS_BUDGET_BYTES,
            "embedded user docs total {total} B exceeds budget of \
             {budget} B. Time to switch from `include_str!` to a \
             compressed-embed scheme — see \
             `docs/dev/operations/embedded-docs-budget.md`. Per-file:\n{detail}",
            budget = EMBEDDED_DOCS_BUDGET_BYTES,
        );
    }

    #[test]
    fn every_user_doc_in_docs_user_is_registered_as_a_topic() {
        // Regression for the gap discovered post-3c.final.E.cleanup:
        // `docs/user/lsp.md` and the new `docs/user/filetree-oil.md`
        // existed on disk but weren't in the registry, so `:help lsp`
        // (or `:help filetree-oil`) failed at runtime even though the
        // user doc shipped with the source tree.
        //
        // This test walks every top-level `docs/user/*.md` and asserts
        // each is registered as a topic. README.md is the "index"
        // topic (registered under that name); every other file's
        // topic name is its stem.
        //
        // The walk uses the workspace root via CARGO_MANIFEST_DIR ↦
        // `<root>/crates/lattice-help` ↦ join `../../docs/user`.
        let docs_user = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/user");
        let r = builtin_topics();
        let names: std::collections::HashSet<&str> = r.names().collect();
        let mut missing: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&docs_user).expect("docs/user readable") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("md filename");
            let topic_name = if stem == "README" { "index" } else { stem };
            if !names.contains(topic_name) {
                missing.push(format!(
                    "{} (expected topic `{}`)",
                    path.display(),
                    topic_name
                ));
            }
        }
        assert!(
            missing.is_empty(),
            "user docs not registered as help topics — \
             add a `r.register(...)` for each in `builtin_topics()`:\n  {}",
            missing.join("\n  ")
        );
    }

    #[test]
    fn builtin_topics_include_new_foundational_topics() {
        // Documentation overhaul (workstream 4): `modal-editing`
        // covers the keymap-side; `ex-commands` covers the `:`
        // surface; `modes` is the major/minor counterpart.
        let r = builtin_topics();
        assert!(
            r.lookup("modal-editing").is_some(),
            "modal-editing topic should be registered",
        );
        assert!(
            r.lookup("ex-commands").is_some(),
            "ex-commands topic should be registered",
        );
        assert!(
            r.lookup("modes").is_some(),
            "modes topic should be registered",
        );
    }

    #[test]
    fn topics_for_command_routes_describe_to_modal_editing() {
        // `:describe-command operator:delete` surfaces a See
        // also: link to modal-editing via the `operator:` pattern.
        let r = builtin_topics();
        let hits: Vec<&str> = r
            .topics_for_command("operator:delete")
            .map(|t| t.name.as_str())
            .collect();
        assert!(hits.contains(&"modal-editing"), "got {hits:?}");
    }

    #[test]
    fn topics_for_command_routes_ex_commands_for_ex_prefixed() {
        let r = builtin_topics();
        let hits: Vec<&str> = r
            .topics_for_command("ex:write")
            .map(|t| t.name.as_str())
            .collect();
        assert!(hits.contains(&"ex-commands"), "got {hits:?}");
    }

    #[test]
    fn topics_for_command_matches_pattern_substring() {
        let r = builtin_topics();
        let hits: Vec<&str> = r
            .topics_for_command("operator:fold-create")
            .map(|t| t.name.as_str())
            .collect();
        assert!(hits.contains(&"folding"));
    }

    #[test]
    fn dynamic_body_invokes_closure_on_each_render() {
        let mut r = HelpTopicRegistry::new();
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter_clone = counter.clone();
        r.register(HelpTopic {
            name: "dynamic".into(),
            summary: "test".into(),
            body: HelpTopicBody::Dynamic(Box::new(move || {
                counter_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                "rendered".to_string()
            })),
            related_command_patterns: Vec::new(),
        });
        let t = r.lookup("dynamic").expect("dynamic topic");
        let _ = t.body.render();
        let _ = t.body.render();
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn registering_same_name_replaces_without_dup_in_order() {
        let mut r = HelpTopicRegistry::new();
        r.register(HelpTopic {
            name: "x".into(),
            summary: "one".into(),
            body: HelpTopicBody::Static(""),
            related_command_patterns: Vec::new(),
        });
        r.register(HelpTopic {
            name: "x".into(),
            summary: "two".into(),
            body: HelpTopicBody::Static(""),
            related_command_patterns: Vec::new(),
        });
        assert_eq!(r.len(), 1);
        assert_eq!(r.lookup("x").expect("x").summary, "two");
    }
}
