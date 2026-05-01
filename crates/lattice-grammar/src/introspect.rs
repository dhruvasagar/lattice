//! Generic introspection (DESIGN.md §5.11).
//!
//! Every `:describe-*` target implements [`Introspectable`]; the
//! shared [`render_introspection`] function turns one into the help
//! body the host wraps in a `HelpBuffer`. The trait gives:
//!
//! - **Uniform output**: kind + identifier + doc + sources +
//!   type-specific extras render in a consistent shape across
//!   `:describe-command`, `:describe-key`, `:describe-option`,
//!   `:describe-event`, `:describe-mode`.
//! - **One place to change**: tweaking how sources or extras render
//!   touches `render_introspection`, not every formatter.
//! - **Plug-in for new registries**: when typed options (§5.12) /
//!   events (§5.10) / modes (Phase 8) land, each adds an
//!   `impl Introspectable` and the introspection surface picks it
//!   up automatically.

use crate::source::SourceLocation;

/// A registered / bound / set thing, queryable from `:describe-*`.
pub trait Introspectable {
    /// Kind label for the help-buffer heading: `"command"`, `"key"`,
    /// `"option"`, `"event"`, `"mode"`, `"buffer"`.
    fn kind_label(&self) -> &'static str;

    /// User-facing identifier: `"ex:write"`, `"j"`,
    /// `"editor.line-numbers"`.
    fn identifier(&self) -> String;

    /// Multi-line documentation. May be empty (the renderer prints a
    /// `(no documentation)` placeholder).
    fn doc(&self) -> &str;

    /// One or more provenance entries. Empty means "no recorded
    /// origin" (common for synthesised buffers / runtime values
    /// without a trace).
    fn sources(&self) -> Vec<SourceEntry<'_>>;

    /// Type-specific blocks: args list for commands, mode-grouped
    /// bindings for keys, value/type for options, payload for events,
    /// keymap chain for modes. Default empty.
    fn extra_sections(&self) -> Vec<HelpSection> {
        Vec::new()
    }
}

/// One labeled provenance link in a help body.
pub struct SourceEntry<'a> {
    pub label: SourceLabel,
    pub source: &'a SourceLocation,
}

/// Human-readable label rendered before the link. Each variant maps
/// to a concrete prose phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLabel {
    DefinedAt,
    BoundAt,
    SubscribedAt,
    LastSetAt,
    OverriddenAt,
    ActivatedAt,
}

impl SourceLabel {
    pub fn as_prose(self) -> &'static str {
        match self {
            SourceLabel::DefinedAt => "Defined at",
            SourceLabel::BoundAt => "Bound at",
            SourceLabel::SubscribedAt => "Subscribed at",
            SourceLabel::LastSetAt => "Last set at",
            SourceLabel::OverriddenAt => "Overridden at",
            SourceLabel::ActivatedAt => "Activated at",
        }
    }
}

/// One named block of body lines. Rendered after the doc and before
/// the source links. Used by impls to surface type-specific structure
/// (e.g. `:describe-command` renders an "Arguments:" section from
/// `args_schema`).
pub struct HelpSection {
    pub heading: String,
    pub lines: Vec<String>,
}

/// Render an [`Introspectable`] into help-body lines. The generic
/// shape every `:describe-*` produces:
///
/// ```text
/// {identifier}  ({kind})
///
/// {doc}
///
/// {extra_section_heading}
///   {extra_section_lines}
///
/// {label}: {source.as_link()}  ({source.layer.label()})
/// ```
pub fn render_introspection(item: &dyn Introspectable) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("{}  ({})", item.identifier(), item.kind_label()));
    lines.push(String::new());
    let doc = item.doc();
    if doc.is_empty() {
        lines.push("(no documentation)".to_string());
    } else {
        for l in doc.lines() {
            lines.push(l.to_string());
        }
    }
    for section in item.extra_sections() {
        lines.push(String::new());
        lines.push(section.heading);
        for l in section.lines {
            lines.push(l);
        }
    }
    let sources = item.sources();
    if !sources.is_empty() {
        lines.push(String::new());
        for SourceEntry { label, source } in sources {
            lines.push(format!(
                "{}: {}  ({})",
                label.as_prose(),
                source.as_link(),
                source.layer.label(),
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::source::{SourceKind, SourceLayer};

    struct StubItem {
        ident: String,
        doc: String,
        source: SourceLocation,
    }

    impl Introspectable for StubItem {
        fn kind_label(&self) -> &'static str {
            "stub"
        }
        fn identifier(&self) -> String {
            self.ident.clone()
        }
        fn doc(&self) -> &str {
            &self.doc
        }
        fn sources(&self) -> Vec<SourceEntry<'_>> {
            vec![SourceEntry {
                label: SourceLabel::DefinedAt,
                source: &self.source,
            }]
        }
    }

    #[test]
    fn rendered_output_starts_with_identifier_and_kind() {
        let item = StubItem {
            ident: "ex:write".into(),
            doc: "Write the buffer.".into(),
            source: SourceLocation::builtin_file("foo.rs", 7),
        };
        let lines = render_introspection(&item);
        assert_eq!(lines[0], "ex:write  (stub)");
        assert!(lines.iter().any(|l| l.contains("Write the buffer.")));
    }

    #[test]
    fn rendered_output_emits_source_link_with_layer_label() {
        let item = StubItem {
            ident: "x".into(),
            doc: "doc".into(),
            source: SourceLocation::builtin_file("a/b.rs", 99),
        };
        let lines = render_introspection(&item);
        let last = lines.last().unwrap();
        assert!(last.contains("Defined at:"));
        assert!(last.contains("[[file:a/b.rs:99]]"));
        assert!(last.contains("(built-in)"));
    }

    #[test]
    fn empty_doc_renders_placeholder() {
        let item = StubItem {
            ident: "x".into(),
            doc: String::new(),
            source: SourceLocation::builtin_file("a.rs", 1),
        };
        let lines = render_introspection(&item);
        assert!(lines.iter().any(|l| l == "(no documentation)"));
    }

    #[test]
    fn no_sources_omits_the_block() {
        struct NoSources;
        impl Introspectable for NoSources {
            fn kind_label(&self) -> &'static str {
                "stub"
            }
            fn identifier(&self) -> String {
                "x".into()
            }
            fn doc(&self) -> &str {
                "doc"
            }
            fn sources(&self) -> Vec<SourceEntry<'_>> {
                Vec::new()
            }
        }
        let lines = render_introspection(&NoSources);
        assert!(lines.iter().all(|l| !l.contains("Defined at")));
    }

    #[test]
    fn extra_sections_render_after_doc() {
        struct WithSection {
            source: SourceLocation,
        }
        impl Introspectable for WithSection {
            fn kind_label(&self) -> &'static str {
                "stub"
            }
            fn identifier(&self) -> String {
                "x".into()
            }
            fn doc(&self) -> &str {
                "the doc"
            }
            fn sources(&self) -> Vec<SourceEntry<'_>> {
                vec![SourceEntry {
                    label: SourceLabel::DefinedAt,
                    source: &self.source,
                }]
            }
            fn extra_sections(&self) -> Vec<HelpSection> {
                vec![HelpSection {
                    heading: "Arguments:".into(),
                    lines: vec!["  1. path".into()],
                }]
            }
        }
        let item = WithSection {
            source: SourceLocation::builtin_file("a.rs", 1),
        };
        let lines = render_introspection(&item);
        let heading_idx = lines.iter().position(|l| l == "Arguments:").unwrap();
        let arg_idx = lines.iter().position(|l| l == "  1. path").unwrap();
        let source_idx = lines.iter().position(|l| l.contains("Defined at:")).unwrap();
        assert!(heading_idx < arg_idx);
        assert!(arg_idx < source_idx);
    }

    #[test]
    fn dot_repeat_chained_source_serialises_as_link() {
        let inner = SourceLocation::builtin_file("a.rs", 5);
        let s = SourceLocation {
            layer: SourceLayer::Runtime,
            kind: SourceKind::DotRepeat(Box::new(inner)),
        };
        assert!(s.as_link().contains("dot-repeat-of"));
        assert!(s.as_link().contains("a.rs:5"));
    }
}
