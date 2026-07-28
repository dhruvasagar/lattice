//! Help topic registry (DESIGN.md §5.11).
//!
//! Hand-written, free-form help docs (the `:help <topic>` surface,
//! distinct from the introspection-driven `:describe-*` views) live
//! here. The built-in set is generated from `docs/user/**/*.md` by
//! `build.rs` and embedded **deflate-compressed**, so `:help` works
//! offline with no filesystem layout assumption beyond the binary
//! itself, without the binary paying raw markdown volume.
//!
//! The registry is intentionally indirection-friendly:
//!
//! - [`HelpTopicBody::Compressed`] is what every builtin uses —
//!   inflated on first open and cached, so a session that never opens
//!   `:help` never decompresses anything.
//! - [`HelpTopicBody::Static`] embeds compile-time markdown directly.
//! - [`HelpTopicBody::Dynamic`] takes a closure that produces text
//!   on demand -- this is the seam for LSP-driven topics
//!   (`:help symbol::Foo`), in-process introspection that can't be
//!   captured at compile time, or any plugin-supplied source.
//!
//! **Known limit:** the host holds this as a plain
//! `Arc<HelpTopicRegistry>` built once at boot, so nothing can add a
//! topic at runtime — a plugin cannot ship a `:help` page today. That
//! is the gap the runtime-directory slice closes; see
//! `docs/dev/operations/embedded-docs-budget.md`.
//!
//! Topics also carry an optional list of substring patterns that
//! match command names; `:describe-command` walks these to emit a
//! `See also: [topic](help:topic)` cross-link when a primitive
//! covered by a topic is described.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

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
                    HelpTopicBody::Compressed { .. } => "compressed",
                    HelpTopicBody::Dynamic(_) => "dynamic",
                },
            )
            .finish()
    }
}

/// Where a topic's body comes from.
///
/// - `Static` — a compile-time `&'static str`. Used by tests and by
///   any caller registering a topic from a string it already holds.
/// - `Compressed` — deflate-compressed markdown embedded at build
///   time. Every builtin topic is one of these. Decompressed on first
///   render and cached, so a session that never opens `:help` never
///   decompresses anything and the second open of a topic is free.
/// - `Dynamic` — a closure invoked on every open (the seam for LSP /
///   introspection / plugin-supplied content).
pub enum HelpTopicBody {
    Static(&'static str),
    Compressed {
        /// Raw deflate stream (no zlib/gzip wrapper).
        packed: &'static [u8],
        /// Decompressed length, used to size the output buffer exactly.
        raw_len: usize,
        /// Decompressed body, filled on first [`HelpTopicBody::render`].
        cache: OnceLock<String>,
    },
    Dynamic(Box<dyn Fn() -> String + Send + Sync>),
}

impl HelpTopicBody {
    pub fn render(&self) -> String {
        match self {
            HelpTopicBody::Static(s) => s.to_string(),
            HelpTopicBody::Compressed {
                packed,
                raw_len,
                cache,
            } => cache.get_or_init(|| inflate(packed, *raw_len)).clone(),
            HelpTopicBody::Dynamic(f) => f(),
        }
    }
}

/// Decompress an embedded topic body.
///
/// A failure here means the build script and this decoder disagree,
/// which is a build-time bug rather than anything the user did — so it
/// surfaces as visible text in the help buffer instead of a panic that
/// takes the editor down, or a silent empty page that looks like a
/// missing doc.
fn inflate(packed: &[u8], raw_len: usize) -> String {
    match miniz_oxide::inflate::decompress_to_vec_with_limit(packed, raw_len.max(1)) {
        Ok(bytes) => String::from_utf8(bytes)
            .unwrap_or_else(|e| format!("help: embedded topic is not valid UTF-8 ({e})")),
        Err(e) => format!("help: could not decompress embedded topic ({e:?})"),
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
                annotations: Vec::new(),
                display_spans: Vec::new(),
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
    for &(name, summary, related, packed, raw_len) in HELP_TOPICS {
        r.register(HelpTopic {
            name: name.to_string(),
            summary: summary.to_string(),
            body: HelpTopicBody::Compressed {
                packed,
                raw_len,
                cache: OnceLock::new(),
            },
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

    #[test]
    fn getting_started_topic_is_registered_for_the_dashboard_link() {
        // The launch dashboard's help-topics section links
        // `:help getting-started` (topic:getting-started). Pin that the
        // topic actually resolves — a dashboard link with no backing doc
        // is a dead `<CR>`.
        let r = builtin_topics();
        let topic = r
            .lookup("getting-started")
            .expect("getting-started topic must exist for the dashboard link");
        assert!(
            !topic.summary.is_empty(),
            "getting-started should carry a frontmatter summary"
        );
        assert!(
            topic.body.render().contains("modal"),
            "getting-started body should cover the modal loop"
        );
    }

    /// Soft binary-size budget for the **embedded** (compressed) user
    /// docs — the bytes that actually land in the binary.
    ///
    /// It used to measure raw markdown, because that *was* what got
    /// embedded. Docs are deflate-compressed at build time now, so raw
    /// size is no longer the cost: measuring it would fire the alarm on
    /// volume the binary never pays for.
    ///
    /// The budget is deliberately generous relative to today's usage
    /// (see the report the test prints) because the whole point of
    /// compressing was to buy room for the doc set to grow.
    ///
    /// **Action when this test fails:** do NOT just bump the number.
    /// Compression is already spent; the next lever is moving docs out
    /// of the binary entirely into a runtime directory — the model
    /// every editor in this class uses (Vim's `$VIMRUNTIME/doc`,
    /// Helix's `runtime/`, Kakoune's `share/kak/doc`), and the one that
    /// also lets plugins ship their own `:help` pages. See
    /// `docs/dev/operations/embedded-docs-budget.md`.
    const EMBEDDED_DOCS_BUDGET_BYTES: usize = 384 * 1024;

    #[test]
    fn embedded_user_docs_stay_under_size_budget() {
        let mut packed_total = 0usize;
        let mut raw_total = 0usize;
        let mut per_topic: Vec<(&str, usize, usize)> = Vec::new();
        for &(name, _summary, _related, packed, raw_len) in HELP_TOPICS {
            packed_total += packed.len();
            raw_total += raw_len;
            per_topic.push((name, packed.len(), raw_len));
        }
        per_topic.sort_by(|a, b| b.1.cmp(&a.1));
        let detail = per_topic
            .iter()
            .take(10)
            .map(|(n, p, r)| format!("    {p:>7} B packed ({r:>7} B raw)  {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let ratio = raw_total as f64 / packed_total.max(1) as f64;

        assert!(
            packed_total <= EMBEDDED_DOCS_BUDGET_BYTES,
            "embedded user docs total {packed_total} B compressed \
             ({raw_total} B raw, {ratio:.1}x) — over the {budget} B budget. \
             Compression is already spent, so the next step is a runtime \
             directory, NOT a bigger number here. See \
             `docs/dev/operations/embedded-docs-budget.md`. Largest:\n{detail}",
            budget = EMBEDDED_DOCS_BUDGET_BYTES,
        );
    }

    /// Compression has to actually be doing something. If a future
    /// change accidentally embeds bodies raw (a build-script regression,
    /// or a `Static` fallback), the budget test above would still pass
    /// while the binary quietly grew — this catches that directly.
    #[test]
    fn embedded_bodies_are_actually_compressed() {
        let (packed, raw): (usize, usize) = HELP_TOPICS
            .iter()
            .fold((0, 0), |(p, r), &(_, _, _, packed, raw_len)| {
                (p + packed.len(), r + raw_len)
            });
        assert!(
            raw > packed * 2,
            "expected embedded docs to compress at least 2x; got {raw} B raw \
             -> {packed} B packed. Are bodies being embedded uncompressed?"
        );
    }

    /// Every embedded body must round-trip. A topic that fails to
    /// inflate renders an error string rather than panicking, which is
    /// the right runtime behaviour but would be invisible without this.
    #[test]
    fn every_embedded_topic_decompresses_to_its_original_length() {
        let r = builtin_topics();
        let mut bad: Vec<String> = Vec::new();
        for &(name, _summary, _related, _packed, raw_len) in HELP_TOPICS {
            let body = r.lookup(name).expect("registered").body.render();
            if body.len() != raw_len {
                bad.push(format!(
                    "{name}: inflated to {} B, expected {raw_len} B — {}",
                    body.len(),
                    body.chars().take(80).collect::<String>()
                ));
            }
        }
        assert!(
            bad.is_empty(),
            "topics that failed to round-trip:\n  {}",
            bad.join("\n  ")
        );
    }

    /// The cache means a topic decompresses once per process, not once
    /// per `:help`. Pins the lazy half of the design.
    #[test]
    fn a_topic_body_is_decompressed_once_and_cached() {
        let r = builtin_topics();
        let topic = r.lookup("index").expect("index topic");
        let first = topic.body.render();
        let second = topic.body.render();
        assert_eq!(first, second);
        match &topic.body {
            HelpTopicBody::Compressed { cache, .. } => {
                assert!(cache.get().is_some(), "render must populate the cache");
            }
            other => panic!(
                "builtin topics must be compressed, got {other:?}",
                other = match other {
                    HelpTopicBody::Static(_) => "static",
                    HelpTopicBody::Dynamic(_) => "dynamic",
                    HelpTopicBody::Compressed { .. } => unreachable!(),
                }
            ),
        }
    }

    #[test]
    fn every_user_doc_in_docs_user_is_registered_as_a_topic() {
        // Regression for the gap discovered post-3c.final.E.cleanup:
        // `docs/user/lsp.md` and the then-new filetree/oil doc existed
        // on disk but weren't in the registry, so `:help lsp` failed at
        // runtime even though the user doc shipped with the source
        // tree. (Registration is generated from the directory now, so
        // this guards the generator rather than a hand-written list.)
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

    /// Blank out fenced blocks and inline code spans so the link
    /// scanners below see only *live* links.
    ///
    /// Docs legitimately show link syntax as an example —
    /// `buffers.md` explains the index format with a literal
    /// `` `[name](help:name)` ``. That is documentation of the form,
    /// not a link to a topic called `name`, and flagging it would
    /// push authors into not documenting the syntax at all. Replacing
    /// the spans with spaces (rather than deleting them) keeps byte
    /// offsets stable, so any future line/column reporting stays
    /// honest.
    #[cfg(test)]
    fn strip_code(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < bytes.len() {
            // Fenced block: ``` … ``` (also covers ~~~ via the same
            // shape when authors use it — checked separately).
            let fence = if text[i..].starts_with("```") {
                Some("```")
            } else if text[i..].starts_with("~~~") {
                Some("~~~")
            } else {
                None
            };
            if let Some(f) = fence {
                let end = text[i + 3..]
                    .find(f)
                    .map(|p| i + 3 + p + 3)
                    .unwrap_or(bytes.len());
                out.extend(std::iter::repeat_n(' ', end - i));
                i = end;
                continue;
            }
            if bytes[i] == b'`' {
                let end = text[i + 1..]
                    .find('`')
                    .map(|p| i + 1 + p + 1)
                    .unwrap_or(bytes.len());
                out.extend(std::iter::repeat_n(' ', end - i));
                i = end;
                continue;
            }
            out.push(text[i..].chars().next().unwrap());
            i += text[i..].chars().next().unwrap().len_utf8();
        }
        out
    }

    /// HD.1 — every `](help:topic)` link in a user doc must resolve.
    ///
    /// This is the test whose absence let 210 dead links accumulate.
    /// Cross-doc links used to be written `](magit-status.md)`, which
    /// `classify_link_url` classifies as `Unresolved` — pressing
    /// `<CR>` on one echoed ``no handler for `magit-status.md` ``.
    /// They rendered correctly on GitHub, so nothing surfaced it. The
    /// docs now use `](help:topic)` throughout, and this pins that
    /// every target is a real topic so a rename cannot silently
    /// orphan a link again.
    #[test]
    fn every_help_link_in_a_user_doc_resolves_to_a_registered_topic() {
        let docs_user = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/user");
        let r = builtin_topics();
        let mut dangling: Vec<String> = Vec::new();
        let mut checked = 0usize;

        for entry in std::fs::read_dir(&docs_user).expect("docs/user readable") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let text = strip_code(&std::fs::read_to_string(&path).expect("read md"));
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            for (idx, _) in text.match_indices("](help:") {
                let rest = &text[idx + "](help:".len()..];
                let Some(end) = rest.find(')') else { continue };
                let target = &rest[..end];
                // `help:topic#anchor` — the topic is the part before
                // the anchor (`do_open_help_topic` splits the same way).
                let name = target.split('#').next().unwrap_or(target);
                checked += 1;
                if r.lookup(name).is_none() {
                    dangling.push(format!("{file}: `help:{target}` — no such topic"));
                }
            }
        }

        assert!(
            checked > 50,
            "expected the docs to be densely cross-linked; only found {checked} help: links — did the link form change?"
        );
        assert!(
            dangling.is_empty(),
            "dangling help links ({} of {checked}):\n  {}",
            dangling.len(),
            dangling.join("\n  ")
        );
    }

    /// HD.2 — an anchored link (`help:magit#options`) must name a
    /// heading that exists in the target topic.
    ///
    /// `do_open_help_topic` treats a missing anchor as "open the topic
    /// unscrolled" rather than an error — the page is still the right
    /// answer, so failing loudly at the user would be worse than
    /// landing at the top. That leniency is what makes this test
    /// necessary: a renamed heading silently degrades every link
    /// pointing at it, and nothing at runtime would say so.
    ///
    /// Slugs follow `generate_heading_anchors` (GitHub-style:
    /// lowercase, non-alphanumeric runs collapsed to `-`, edges
    /// trimmed), with inline code backticks stripped first so a
    /// heading like `` ## `C-c g` — dispatch `` slugs the way its
    /// rendered text reads.
    #[test]
    fn every_anchored_help_link_names_a_heading_that_exists() {
        fn slug(heading: &str) -> String {
            let plain = heading.replace('`', "");
            let mut out = String::new();
            let mut pending_dash = false;
            for c in plain.chars() {
                if c.is_ascii_alphanumeric() {
                    if pending_dash && !out.is_empty() {
                        out.push('-');
                    }
                    pending_dash = false;
                    out.extend(c.to_lowercase());
                } else {
                    pending_dash = true;
                }
            }
            out
        }

        let docs_user = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/user");
        let mut headings: std::collections::HashMap<String, std::collections::HashSet<String>> =
            std::collections::HashMap::new();
        let mut bodies: Vec<(String, String)> = Vec::new();

        for entry in std::fs::read_dir(&docs_user).expect("docs/user readable") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
            let topic = if stem == "README" { "index" } else { stem }.to_string();
            let text = std::fs::read_to_string(&path).expect("read md");
            let set: std::collections::HashSet<String> = text
                .lines()
                .filter_map(|l| l.strip_prefix('#'))
                .map(|l| slug(l.trim_start_matches('#').trim()))
                .collect();
            headings.insert(topic.clone(), set);
            bodies.push((topic, strip_code(&text)));
        }

        let mut broken: Vec<String> = Vec::new();
        for (from, text) in &bodies {
            for (idx, _) in text.match_indices("](help:") {
                let rest = &text[idx + "](help:".len()..];
                let Some(end) = rest.find(')') else { continue };
                let Some((topic, anchor)) = rest[..end].split_once('#') else {
                    continue;
                };
                if let Some(known) = headings.get(topic)
                    && !known.contains(anchor)
                {
                    broken.push(format!(
                        "{from}.md: `help:{topic}#{anchor}` — `{topic}` has no such heading"
                    ));
                }
            }
        }

        assert!(
            broken.is_empty(),
            "anchored help links pointing at headings that no longer exist \
             (they open the topic unscrolled, so nothing reports this at \
             runtime):\n  {}",
            broken.join("\n  ")
        );
    }

    /// HD.1 — the index lists every topic.
    ///
    /// `README.md` is the `index` topic, the page bare `:help` opens
    /// and the only browsable catalogue of what documentation exists.
    /// A topic missing from it is discoverable only by already knowing
    /// its name. `surround-mode` and `terminal-mode` were both absent
    /// when this test was written.
    #[test]
    fn the_index_lists_every_registered_topic() {
        let docs_user = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/user");
        let index = std::fs::read_to_string(docs_user.join("README.md")).expect("read index");
        let index = strip_code(&index);

        let mut listed: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (idx, _) in index.match_indices("](help:") {
            let rest = &index[idx + "](help:".len()..];
            if let Some(end) = rest.find(')') {
                listed.insert(rest[..end].split('#').next().unwrap_or("").to_string());
            }
        }

        let mut missing: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&docs_user).expect("docs/user readable") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).expect("stem");
            // README is the index itself.
            if stem == "README" || listed.contains(stem) {
                continue;
            }
            missing.push(stem.to_string());
        }
        missing.sort();
        assert!(
            missing.is_empty(),
            "topics missing from the `:help` index (docs/user/README.md) — \
             they exist but nothing links to them:\n  {}",
            missing.join("\n  ")
        );
    }

    /// The other half: a *sibling* markdown link (`](compilation-mode.md)`)
    /// is dead inside `:help` because nothing resolves a bare `.md`
    /// path to a topic. Links to `../dev/**` are exempt — those are
    /// developer docs that are deliberately not help topics, and they
    /// stay plain markdown so they still resolve on disk and on
    /// GitHub.
    #[test]
    fn no_user_doc_links_to_a_sibling_doc_by_filename() {
        let docs_user = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/user");
        let mut offenders: Vec<String> = Vec::new();

        for entry in std::fs::read_dir(&docs_user).expect("docs/user readable") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let text = strip_code(&std::fs::read_to_string(&path).expect("read md"));
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            for (idx, _) in text.match_indices("](") {
                let rest = &text[idx + 2..];
                let Some(end) = rest.find(')') else { continue };
                let url = &rest[..end];
                // Only bare siblings: no scheme, no parent-dir escape.
                if url.ends_with(".md") && !url.contains('/') && !url.contains(':') {
                    offenders.push(format!(
                        "{file}: `{url}` — use `help:{}`",
                        &url[..url.len() - 3]
                    ));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "sibling `.md` links are dead inside `:help` (they classify as \
             `Unresolved`); use the `help:` form:\n  {}",
            offenders.join("\n  ")
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
