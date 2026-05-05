//! Built-in annotators (DESIGN.md §5.11.3).
//!
//! Annotators run AFTER ranking. Each appends to
//! `RenderedCandidate.annotations`; the renderer joins them with
//! two spaces.

use crate::candidate::{CandidateData, CandidateKind, RenderedCandidate};
use crate::traits::CandidateAnnotator;

/// `anno:kind-label`. Tags every candidate with `(command)`,
/// `(file)`, `(motion)`, etc. -- the kind label.
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
        c.annotations.push(format!("({label})"));
    }
}

/// `anno:doc-snippet`. Appends the first line of the candidate's
/// documentation if available.
pub struct DocSnippetAnnotator;

impl CandidateAnnotator for DocSnippetAnnotator {
    fn annotate(&self, c: &mut RenderedCandidate) {
        let snippet = match &c.raw.data {
            CandidateData::Command { doc, .. } => first_line(doc),
            CandidateData::Option { doc, .. } => first_line(doc),
            CandidateData::Chord { doc, .. } => first_line(doc),
            CandidateData::File { path, .. } => path.display().to_string(),
            CandidateData::Register { preview, .. } => preview.clone(),
            CandidateData::Mark { position, .. } => position.clone(),
            CandidateData::Plain | CandidateData::Extension { .. } => return,
        };
        if !snippet.is_empty() {
            c.annotations.push(snippet);
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
            },
            score: MatchScore::PERFECT,
            match_ranges: Vec::new(),
            annotations: Vec::new(),
        }
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
        assert_eq!(c.annotations, vec!["(motion)"]);
    }

    #[test]
    fn kind_label_falls_back_to_command_for_non_command_data() {
        let mut c = rendered("x", CandidateKind::Command, CandidateData::Plain);
        KindLabelAnnotator.annotate(&mut c);
        assert_eq!(c.annotations, vec!["(command)"]);
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
        assert_eq!(f.annotations, vec!["(file)"]);

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
        assert_eq!(d.annotations, vec!["(directory)"]);
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
        assert_eq!(c.annotations, vec!["Write the buffer."]);
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
        assert_eq!(c.annotations, vec!["/tmp/foo/file.rs"]);
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
        // marks when run in sequence, in order.
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
        assert_eq!(c.annotations, vec!["(ex-command)", "Write the buffer."]);
    }
}
