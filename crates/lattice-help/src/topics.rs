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
//! "See also: [topic](help:topic)" cross-link when a primitive
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
            })
            .collect()
    }
}

/// The v1 built-in topic set. Sourced from `docs/user/*.md` via
/// `include_str!` so the binary is self-contained -- no filesystem
/// dependency at runtime.
pub fn builtin_topics() -> Arc<HelpTopicRegistry> {
    let mut r = HelpTopicRegistry::new();
    // Index lives at the conventional name `index`; `:help` with
    // no arg routes to it. The README's content is the rendered
    // index page (table of topics + brief explainer).
    r.register(HelpTopic {
        name: "index".into(),
        summary: "Topic index -- start here when you don't know what to look up.".into(),
        body: HelpTopicBody::Static(include_str!("../../../docs/user/README.md")),
        related_command_patterns: Vec::new(),
    });
    r.register(HelpTopic {
        name: "folding".into(),
        summary: "Manual + computed folds, fold operators, navigation, auto-open on search.".into(),
        body: HelpTopicBody::Static(include_str!("../../../docs/user/folding.md")),
        // `fold` covers any future fold-related command name; we
        // don't bind broader prefixes (e.g. `z`) because they
        // overlap too many unrelated chord-prefix bindings.
        related_command_patterns: vec!["fold".into(), "foldmethod".into()],
    });
    r.register(HelpTopic {
        name: "buffers".into(),
        summary: "Buffers, panes, splits, file tree, navigation, theme customization.".into(),
        body: HelpTopicBody::Static(include_str!("../../../docs/user/buffers.md")),
        related_command_patterns: vec![
            "buffer".into(),
            "tree".into(),
            "split".into(),
            "pane".into(),
            "ex:e".into(),
            "ex:b".into(),
        ],
    });
    r.register(HelpTopic {
        name: "languages".into(),
        summary: "Bundled languages, coverage roadmap, and how to add a new language \
                  (tree-sitter or otherwise)."
            .into(),
        body: HelpTopicBody::Static(include_str!("../../../docs/user/languages.md")),
        // Most language-related commands route through `:set
        // syntax=...` once that lands; for now bind the topic to
        // the foldmethod option so users land on the right doc when
        // they're investigating per-language fold behaviour.
        related_command_patterns: vec!["language".into(), "syntax".into()],
    });
    r.register(HelpTopic {
        name: "completion".into(),
        summary: "Insert-mode completion: triggers, sources, popup keymap, configuration, \
                  troubleshooting."
            .into(),
        body: HelpTopicBody::Static(include_str!("../../../docs/user/completion.md")),
        related_command_patterns: vec![
            "complete".into(),
            "completion".into(),
            "snippet".into(),
            "ex:complete".into(),
        ],
    });
    r.register(HelpTopic {
        name: "modes".into(),
        summary: "Major + minor modes: what they are, how `:<mode-name>` toggles work, \
                  major-mode swaps, auto-activation hooks, the option-resolution model."
            .into(),
        body: HelpTopicBody::Static(include_str!("../../../docs/user/modes.md")),
        // Bind to the auto-generated `:<mode-name>` commands so
        // `:describe-command rust-mode` (or any registered mode)
        // surfaces a "See also: modes" cross-link to the topic.
        related_command_patterns: vec!["mode".into()],
    });
    r.register(HelpTopic {
        name: "lsp-mode".into(),
        summary: "Per-buffer gate that controls whether LSP features run \
                  (auto-activation, toggle, what's gated, programmatic API)."
            .into(),
        body: HelpTopicBody::Static(include_str!("../../../docs/user/lsp-mode.md")),
        related_command_patterns: vec!["lsp".into()],
    });
    r.register(HelpTopic {
        name: "options".into(),
        summary: "Typed configuration: `:set` syntax, layered resolution \
                  (defaults / TOML / runtime / mode contributions / per-buffer), \
                  groups, and how to find every registered option."
            .into(),
        body: HelpTopicBody::Static(include_str!("../../../docs/user/options.md")),
        // `set` covers `:set NAME=VALUE`; the typed-options
        // surface picks up `:options` / `:describe-option` /
        // `:customize` automatically once those bind here.
        related_command_patterns: vec!["set".into(), "option".into(), "customize".into()],
    });
    r.register(HelpTopic {
        name: "modal-editing".into(),
        summary: "Modal editing: Normal / Insert / Visual / Command \
                  / Search / Replace, the vim grammar (operators + \
                  motions + text objects + counts), and registers / \
                  marks / macros."
            .into(),
        body: HelpTopicBody::Static(include_str!("../../../docs/user/modal-editing.md")),
        related_command_patterns: vec![
            // Match the operator/motion-class command names from
            // the grammar registry so :describe-command on `dd`,
            // `yw`, `gd`, etc. surfaces a "see also: modal-editing".
            "operator:".into(),
            "motion:".into(),
            "text-object:".into(),
            "register".into(),
            "mark".into(),
            "macro".into(),
        ],
    });
    r.register(HelpTopic {
        name: "ex-commands".into(),
        summary: "The `:` line: built-in commands, arg schemas, tab \
                  completion, ranges, aliases, `:g` / `:v` / `:s` \
                  with live preview."
            .into(),
        body: HelpTopicBody::Static(include_str!("../../../docs/user/ex-commands.md")),
        related_command_patterns: vec![
            // Most ex-command names start with `ex:`; bind broadly
            // so :describe-command on any of them links here.
            "ex:".into(),
            "substitute".into(),
            "global".into(),
        ],
    });
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
