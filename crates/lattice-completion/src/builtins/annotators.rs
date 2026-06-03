//! Built-in annotators (DESIGN.md §5.11.3).
//!
//! Annotators run AFTER ranking. Each appends typed
//! [`Annotation`] values to `RenderedCandidate.annotations`;
//! the renderer paints each one with the style its category
//! resolves to. See `docs/dev/architecture/marginalia.md`.

use std::sync::Arc;

use crate::candidate::{Annotation, CandidateData, CandidateKind, RenderedCandidate};
use crate::traits::CandidateAnnotator;

/// `anno:kind-label`. Tags every candidate with `(command)`,
/// `(file)`, `(motion)`, etc. -- the kind label. Pushes
/// [`Annotation::Kind`] so the renderer styles it with the
/// kind-annotation theme slot.
pub struct KindLabelAnnotator;

impl CandidateAnnotator for KindLabelAnnotator {
    fn annotate(&self, c: &mut RenderedCandidate) {
        let label = match (&c.raw.kind, &c.raw.data) {
            // For commands we have a per-spec kind label
            // ("motion", "operator", "ex-command", ...) -- richer
            // than the static `CandidateKind::Command`.
            (CandidateKind::Command, CandidateData::Command { kind_label, .. }) => {
                kind_label.clone()
            }
            (CandidateKind::Command, _) => "command".to_string(),
            (CandidateKind::Option, _) => "option".to_string(),
            (CandidateKind::File, CandidateData::File { is_dir: true, .. }) => {
                "directory".to_string()
            }
            (CandidateKind::File, _) => "file".to_string(),
            (CandidateKind::Directory, _) => "directory".to_string(),
            (CandidateKind::Pattern, _) => "pattern".to_string(),
            (CandidateKind::Buffer, _) => "buffer".to_string(),
            (CandidateKind::Register, _) => "register".to_string(),
            (CandidateKind::Mark, _) => "mark".to_string(),
            (CandidateKind::Chord, _) => "chord".to_string(),
            (CandidateKind::Plain, _) => return,
            (CandidateKind::Extension(_), _) => return,
        };
        c.annotations
            .push(Annotation::Kind(Arc::from(format!("({label})"))));
    }
}

/// `anno:doc-snippet`. Appends the first line of the candidate's
/// documentation if available. Pushes [`Annotation::DocSnippet`]
/// so the renderer styles it with the doc-annotation theme slot.
pub struct DocSnippetAnnotator;

impl CandidateAnnotator for DocSnippetAnnotator {
    fn annotate(&self, c: &mut RenderedCandidate) {
        let snippet = match &c.raw.data {
            CandidateData::Command { doc, .. } => first_line(doc),
            CandidateData::Option { doc, .. } => first_line(doc),
            // Slice `3c.unify.option-doc-annotator`: per-value
            // doc surfaces in the marginalia column. Empty doc
            // (when the type hasn't overridden
            // `enumerate_with_docs`) annotates nothing.
            CandidateData::OptionValue { doc, .. } => first_line(doc),
            CandidateData::Chord { doc, .. } => first_line(doc),
            CandidateData::File { path, .. } => path.display().to_string(),
            CandidateData::Register { preview, .. } => preview.clone(),
            CandidateData::Mark { position, .. } => position.clone(),
            CandidateData::Plain | CandidateData::Extension { .. } => return,
        };
        if !snippet.is_empty() {
            c.annotations
                .push(Annotation::DocSnippet(Arc::from(snippet)));
        }
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::candidate::{CandidateData, CandidateKind, MatchScore, RawCandidate};
    use lattice_grammar::source::SourceLocation;

    fn rendered(text: &str, kind: CandidateKind, data: CandidateData) -> RenderedCandidate {
        RenderedCandidate {
            raw: RawCandidate {
                text: text.into(),
                display: text.into(),
                kind,
                data,
                source: None,
                accept_action: None,
            },
            score: MatchScore::PERFECT,
            match_ranges: Vec::new(),
            annotations: Vec::new(),
        }
    }

    /// Lift annotation text out of the typed enum for ergonomic
    /// assertion: every test in this module wants to compare
    /// against `Vec<&str>` of display text. Category is asserted
    /// separately in the `*_categorizes_as_*` tests.
    fn display_texts(c: &RenderedCandidate) -> Vec<String> {
        c.annotations
            .iter()
            .map(|a| a.display_text().into_owned())
            .collect()
    }

    #[test]
    fn kind_label_uses_command_kind_label_when_present() {
        let mut c = rendered(
            "motion:line-down",
            CandidateKind::Command,
            CandidateData::Command {
                name: "motion:line-down".into(),
                doc: "".into(),
                kind_label: "motion".into(),
                source: SourceLocation::synthetic("test"),
            },
        );
        KindLabelAnnotator.annotate(&mut c);
        assert_eq!(display_texts(&c), vec!["(motion)"]);
    }

    #[test]
    fn kind_label_categorizes_as_kind() {
        // MARG.1: typed annotation carries its category; the
        // renderer pattern-matches on this to pick the theme
        // slot. Asserts the variant tag, not the text.
        let mut c = rendered(
            "x",
            CandidateKind::Command,
            CandidateData::Command {
                name: "x".into(),
                doc: "".into(),
                kind_label: "motion".into(),
                source: SourceLocation::synthetic("test"),
            },
        );
        KindLabelAnnotator.annotate(&mut c);
        assert_eq!(c.annotations[0].category(), "kind");
        assert!(matches!(c.annotations[0], Annotation::Kind(_)));
    }

    #[test]
    fn kind_label_falls_back_to_command_for_non_command_data() {
        let mut c = rendered("x", CandidateKind::Command, CandidateData::Plain);
        KindLabelAnnotator.annotate(&mut c);
        assert_eq!(display_texts(&c), vec!["(command)"]);
    }

    #[test]
    fn kind_label_distinguishes_files_from_directories() {
        let mut f = rendered(
            "file.rs",
            CandidateKind::File,
            CandidateData::File {
                path: "/tmp/file.rs".into(),
                is_dir: false,
                size: None,
            },
        );
        KindLabelAnnotator.annotate(&mut f);
        assert_eq!(display_texts(&f), vec!["(file)"]);

        let mut d = rendered(
            "Documents",
            CandidateKind::File,
            CandidateData::File {
                path: "/tmp/Documents".into(),
                is_dir: true,
                size: None,
            },
        );
        KindLabelAnnotator.annotate(&mut d);
        assert_eq!(display_texts(&d), vec!["(directory)"]);
    }

    #[test]
    fn kind_label_skips_plain_kind() {
        let mut c = rendered("x", CandidateKind::Plain, CandidateData::Plain);
        KindLabelAnnotator.annotate(&mut c);
        assert!(c.annotations.is_empty());
    }

    #[test]
    fn doc_snippet_appends_first_line_of_command_doc() {
        let mut c = rendered(
            "ex:write",
            CandidateKind::Command,
            CandidateData::Command {
                name: "ex:write".into(),
                doc: "Write the buffer.\nMore detail follows.".into(),
                kind_label: "ex-command".into(),
                source: SourceLocation::synthetic("test"),
            },
        );
        DocSnippetAnnotator.annotate(&mut c);
        assert_eq!(display_texts(&c), vec!["Write the buffer."]);
    }

    #[test]
    fn doc_snippet_categorizes_as_doc() {
        let mut c = rendered(
            "ex:write",
            CandidateKind::Command,
            CandidateData::Command {
                name: "ex:write".into(),
                doc: "Write the buffer.".into(),
                kind_label: "ex-command".into(),
                source: SourceLocation::synthetic("test"),
            },
        );
        DocSnippetAnnotator.annotate(&mut c);
        assert_eq!(c.annotations[0].category(), "doc");
        assert!(matches!(c.annotations[0], Annotation::DocSnippet(_)));
    }

    #[test]
    fn doc_snippet_uses_path_for_files() {
        let mut c = rendered(
            "file.rs",
            CandidateKind::File,
            CandidateData::File {
                path: "/tmp/foo/file.rs".into(),
                is_dir: false,
                size: None,
            },
        );
        DocSnippetAnnotator.annotate(&mut c);
        assert_eq!(display_texts(&c), vec!["/tmp/foo/file.rs"]);
    }

    #[test]
    fn doc_snippet_skips_when_doc_is_empty() {
        let mut c = rendered(
            "x",
            CandidateKind::Command,
            CandidateData::Command {
                name: "x".into(),
                doc: "".into(),
                kind_label: "ex-command".into(),
                source: SourceLocation::synthetic("test"),
            },
        );
        DocSnippetAnnotator.annotate(&mut c);
        assert!(c.annotations.is_empty());
    }

    #[test]
    fn doc_snippet_skips_extension_data() {
        let mut c = rendered(
            "x",
            CandidateKind::Extension(7),
            CandidateData::Extension {
                kind_id: 7,
                payload: vec![],
            },
        );
        DocSnippetAnnotator.annotate(&mut c);
        assert!(c.annotations.is_empty());
    }

    #[test]
    fn annotators_chain_in_registration_order() {
        // Pure trait-level test: verify both annotators leave their
        // marks when run in sequence, in order. Asserts both the
        // category tags AND the display text are preserved so the
        // renderer's per-variant style lookup downstream gets the
        // right slot.
        let mut c = rendered(
            "ex:write",
            CandidateKind::Command,
            CandidateData::Command {
                name: "ex:write".into(),
                doc: "Write the buffer.".into(),
                kind_label: "ex-command".into(),
                source: SourceLocation::synthetic("test"),
            },
        );
        KindLabelAnnotator.annotate(&mut c);
        DocSnippetAnnotator.annotate(&mut c);
        assert_eq!(display_texts(&c), vec!["(ex-command)", "Write the buffer."]);
        assert_eq!(c.annotations[0].category(), "kind");
        assert_eq!(c.annotations[1].category(), "doc");
    }

    #[test]
    fn keybinding_display_text_formats_chords_space_separated() {
        // MARG.1 sets up the Keybinding variant; MARG.2 wires the
        // annotator. Display-text formatting belongs to the
        // variant — assert it here so the contract is locked in
        // before the annotator lands.
        use lattice_protocol::KeyChord;
        let ann = Annotation::Keybinding(vec![
            KeyChord::ctrl('w'),
            KeyChord::char('v'),
        ]);
        assert_eq!(ann.display_text(), "<C-w> v");
        assert_eq!(ann.category(), "keybinding");
    }

    #[test]
    fn keybinding_display_text_handles_single_chord() {
        use lattice_protocol::KeyChord;
        let ann = Annotation::Keybinding(vec![KeyChord::char('j')]);
        assert_eq!(ann.display_text(), "j");
    }

    #[test]
    fn keybinding_display_text_is_empty_for_empty_chord_list() {
        // Annotators should not emit Keybinding with an empty
        // list (the contract is documented on the variant); but
        // if they do, display falls back to empty so we never
        // panic at paint time.
        let ann = Annotation::Keybinding(vec![]);
        assert_eq!(ann.display_text(), "");
    }

    #[test]
    fn custom_annotation_passes_slot_through() {
        let ann = Annotation::Custom {
            text: "[lsp]".into(),
            slot: "annotation_lsp".into(),
        };
        assert_eq!(ann.display_text(), "[lsp]");
        assert_eq!(ann.category(), "annotation_lsp");
    }
}
