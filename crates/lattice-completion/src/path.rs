//! Path-completion source (CSM.7).
//!
//! Fires when `InsertContext::path_context` is true -- the host
//! sets the flag when the cursor sits inside a tree-sitter
//! string scope. The source walks the directory referenced by
//! the partial path the user typed, emits one candidate per
//! filesystem entry (capped, ignored-name set filtered).
//!
//! No internal cache in v1 -- the cache that lived host-side
//! (`App.path_completion_cache`) retired with this slice. Each
//! popup-open / refilter pays one `read_dir` + per-entry
//! `file_type()`; profile and reintroduce a source-internal
//! `Mutex<Option<...>>` if the keystroke budget regresses.
//!
//! Mode adapter (`PathCompletionMode`) lives in
//! `lattice-mode::modes::completion` due to the
//! `lattice-mode <- lattice-completion` dep direction; the
//! adapter just wraps an `Arc<PathCompletionSource>`.

use std::path::PathBuf;

use crate::candidate::{CandidateData, CandidateKind, RawCandidate};
use crate::insert::{InsertContext, SourceId, PATH_SOURCE_ID};
use crate::source::SyncCompletionSource;

/// Hardcoded ignore set for v1 path completion. `.gitignore`
/// integration queues for a follow-up (needs the `ignore` crate
/// + workspace-root resolution).
const IGNORE_NAMES: &[&str] = &[".git", "node_modules", "target", "dist"];

/// Cap on candidates emitted per popup-open. Very large
/// directories would otherwise saturate the popup; the
/// matcher's typed prefix narrows things down anyway.
const MAX_ENTRIES: usize = 200;

/// The `SyncCompletionSource` impl for path completion.
/// Stateless beyond its newtype identity; the host threads
/// `path_context` + `buffer_dir` through `InsertContext`.
#[derive(Debug, Clone, Default)]
pub struct PathCompletionSource;

impl SyncCompletionSource for PathCompletionSource {
    fn produce(&self, ctx: &InsertContext<'_>) -> Vec<RawCandidate> {
        // Suppression: outside a string scope, do nothing. The
        // popup's all-sources view stays free of filenames in
        // prose / code.
        if !ctx.path_context {
            return Vec::new();
        }
        let Some(buffer_dir) = ctx.buffer_dir else {
            return Vec::new();
        };

        // Walk back over path-shaped bytes (NOT stopping at `/`)
        // to recover the full partial path the user typed inside
        // the string literal. The trigger anchor in
        // `do_completion_trigger` stops at `/`; here we want the
        // full thing so we know which directory to walk.
        let line_text = ctx.buffer.line(ctx.cursor.line).unwrap_or_default();
        let line_bytes = line_text.as_bytes();
        let cursor_in_line = (ctx.cursor.byte as usize).min(line_bytes.len());
        let mut path_start = cursor_in_line;
        while path_start > 0 {
            let b = line_bytes[path_start - 1];
            if b == b'/' || is_path_byte(b) {
                path_start -= 1;
            } else {
                break;
            }
        }
        let partial: &str = &line_text[path_start..cursor_in_line];
        let dir_part = match partial.rfind('/') {
            Some(i) => &partial[..=i], // keep trailing slash
            None => "",
        };
        let base_dir: PathBuf = if dir_part.starts_with('/') {
            PathBuf::from(dir_part)
        } else if dir_part.is_empty() {
            buffer_dir.to_path_buf()
        } else {
            buffer_dir.join(dir_part)
        };

        let Ok(read) = std::fs::read_dir(&base_dir) else {
            return Vec::new();
        };
        let mut entries: Vec<(String, bool)> = read
            .flatten()
            .filter_map(|entry| {
                entry.file_name().to_str().map(|name| {
                    let is_dir = entry
                        .file_type()
                        .map(|t| t.is_dir())
                        .unwrap_or(false);
                    (name.to_string(), is_dir)
                })
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let source_id = SourceId::new(PATH_SOURCE_ID);
        let mut out: Vec<RawCandidate> = Vec::with_capacity(entries.len().min(MAX_ENTRIES));
        for (name, is_dir) in entries {
            if out.len() >= MAX_ENTRIES {
                break;
            }
            if IGNORE_NAMES.iter().any(|n| *n == name) {
                continue;
            }
            // Dotfiles are filtered by default -- they're rarely
            // the user's typed-context-correct candidate. A
            // future `completion.show_hidden` typed option could
            // surface them.
            if name.starts_with('.') {
                continue;
            }
            let display = if is_dir {
                format!("{name}/")
            } else {
                name.clone()
            };
            // For directories, the inserted text includes the
            // trailing `/` so the user can keep tab-completing
            // into nested paths (mirrors `gen:files` for the
            // cmdline).
            let text = if is_dir {
                format!("{name}/")
            } else {
                name.clone()
            };
            let mut cand = RawCandidate::plain(
                text,
                if is_dir {
                    CandidateKind::Directory
                } else {
                    CandidateKind::File
                },
            )
            .with_source(source_id.clone());
            cand.display = display;
            cand.data = CandidateData::File {
                path: base_dir.join(&name),
                is_dir,
                size: None,
            };
            out.push(cand);
        }
        out
    }
}

/// Same byte-class check as the host's `is_path_byte` --
/// duplicated here so the source doesn't reach into host
/// internals. Identifier chars plus `.` and `-` for filenames
/// like `foo.txt` and `my-script`.
fn is_path_byte(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.' | b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insert::CompletionTrigger;
    use lattice_core::Buffer;
    use lattice_protocol::Position;
    use std::fs;

    fn make_buffer(text: &str) -> Buffer {
        let mut b = Buffer::empty();
        let _ = b.apply_edit(&lattice_protocol::edit::Edit::insert(
            Position::ZERO,
            text.to_string(),
        ));
        b
    }

    #[test]
    fn returns_empty_outside_path_context() {
        let buffer = make_buffer("foo");
        let ctx = InsertContext {
            buffer: &buffer,
            cursor: Position::new(0, 3),
            anchor: Position::ZERO,
            query: "foo",
            trigger: &CompletionTrigger::Manual,
            case_sensitive: false,
            language: "",
            tree_sitter_symbols: &[],
            path_context: false,
            buffer_dir: Some(std::path::Path::new(".")),
            uri: None,
            lsp_position: None,
        };
        assert!(PathCompletionSource.produce(&ctx).is_empty());
    }

    #[test]
    fn lists_dir_entries_when_in_path_context() {
        let tmp = std::env::temp_dir().join(format!("lattice-csm7-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("alpha.txt"), "").unwrap();
        fs::create_dir(tmp.join("subdir")).unwrap();
        let buffer = make_buffer("\"a");
        let ctx = InsertContext {
            buffer: &buffer,
            cursor: Position::new(0, 2),
            anchor: Position::new(0, 1),
            query: "a",
            trigger: &CompletionTrigger::Manual,
            case_sensitive: false,
            language: "",
            tree_sitter_symbols: &[],
            path_context: true,
            buffer_dir: Some(&tmp),
            uri: None,
            lsp_position: None,
        };
        let candidates = PathCompletionSource.produce(&ctx);
        let labels: Vec<_> = candidates.iter().map(|c| c.display.clone()).collect();
        assert!(labels.iter().any(|d| d == "alpha.txt"), "got {labels:?}");
        assert!(labels.iter().any(|d| d == "subdir/"), "got {labels:?}");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ignored_names_are_skipped() {
        let tmp = std::env::temp_dir().join(format!("lattice-csm7-ign-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::create_dir(tmp.join(".git")).unwrap();
        fs::create_dir(tmp.join("node_modules")).unwrap();
        fs::write(tmp.join("readme.md"), "").unwrap();
        let buffer = make_buffer("\"");
        let ctx = InsertContext {
            buffer: &buffer,
            cursor: Position::new(0, 1),
            anchor: Position::new(0, 1),
            query: "",
            trigger: &CompletionTrigger::Manual,
            case_sensitive: false,
            language: "",
            tree_sitter_symbols: &[],
            path_context: true,
            buffer_dir: Some(&tmp),
            uri: None,
            lsp_position: None,
        };
        let candidates = PathCompletionSource.produce(&ctx);
        let labels: Vec<_> = candidates.iter().map(|c| c.display.clone()).collect();
        assert!(labels.iter().any(|d| d == "readme.md"));
        assert!(!labels.iter().any(|d| d == ".git/"));
        assert!(!labels.iter().any(|d| d == "node_modules/"));
        let _ = fs::remove_dir_all(&tmp);
    }
}
