//! Built-in candidate generators (DESIGN.md §5.11.3).
//!
//! v1 ships:
//! - [`CommandsGenerator`] -- every `CommandSpec` in the registry.
//!   Caches the full set per registry-version (commands don't
//!   change at runtime in v1, so the cache key is fixed and the
//!   cache effectively never expires).
//! - [`FilesGenerator`] -- filesystem entries for a path-shaped
//!   prefix. Caches per-directory with a 1-second soft TTL.
//!
//! Other host-state generators (chords, registers, marks, buffers)
//! live in `lattice-ui-tui` because they need App-level state.
//! Plugins register their own through the same trait.

use std::path::PathBuf;
use std::time::Duration;

use crate::candidate::{CacheKey, CandidateData, CandidateKind, RawCandidate};
use crate::traits::{CandidateGenerator, GenerateContext};

/// `gen:commands`. Walks the `CommandRegistry` and emits one
/// `RawCandidate` per registered `CommandSpec`. Filtering is
/// deferred to the matcher; the generator returns the full set
/// every time the cache misses.
///
/// Delimiter-form commands (`ex:substitute`, `ex:global` --
/// `SurfaceForm::Delimiter`) are excluded: the user types those
/// via `:s/.../.../`, `:g/.../.../`, `:v/.../.../`. Surfacing them
/// as completion candidates is misleading because typing
/// `:ex:global` would error with "use the delimiter form" -- the
/// keyword form is intentionally a hard-error redirect (DESIGN.md
/// §B.2). They remain reachable through `:describe-command` /
/// `:apropos` for introspection.
pub struct CommandsGenerator;

impl CandidateGenerator for CommandsGenerator {
    fn generate(&self, ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let mut out: Vec<RawCandidate> = ctx
            .registry
            .names()
            .filter_map(|name| {
                let id = ctx.registry.id_by_name(name)?;
                let spec = ctx.registry.lookup(id)?;
                // Filter delimiter-only commands -- they have no
                // useful keyword-form completion target.
                if let Some(ex) = ctx.registry.ex_command_spec(id)
                    && matches!(
                        ex.surface_form,
                        lattice_grammar::SurfaceForm::Delimiter { .. }
                    )
                {
                    return None;
                }
                Some(RawCandidate {
                    text: spec.name.clone(),
                    display: spec.name.clone(),
                    kind: CandidateKind::Command,
                    data: CandidateData::Command {
                        name: spec.name.clone(),
                        doc: spec.doc.clone(),
                        kind_label: spec.kind.label().to_string(),
                        source: spec.source.clone(),
                    },
                    source: None,
                    accept_action: None,
                })
            })
            .collect();
        out.sort_by(|a, b| a.text.cmp(&b.text));
        out
    }

    fn cache_key(&self, _ctx: &GenerateContext<'_>) -> Option<CacheKey> {
        // Commands don't change at runtime in v1, so a single
        // fixed key is correct. When dynamic registration lands
        // (post-WASM) the registry exposes a version counter and
        // we key on that.
        Some(CacheKey::new("gen:commands:v1"))
    }
}

/// `gen:files`. Resolves the prefix into a directory + basename
/// pattern, then lists entries of that directory matching the
/// basename prefix. Returns directories with a trailing `/` so the
/// user can keep tab-completing into nested paths.
pub struct FilesGenerator;

impl CandidateGenerator for FilesGenerator {
    fn generate(&self, ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        // Split prefix at the last `/`. Everything before is the
        // directory; everything after (or the whole prefix if no
        // `/`) is the basename. Resolve `~` to $HOME for Unix
        // paths.
        let (dir_str, basename) = match ctx.prefix.rfind('/') {
            Some(i) => (&ctx.prefix[..=i], &ctx.prefix[i + 1..]),
            None => ("", ctx.prefix),
        };
        let dir_path = expand_tilde(dir_str);

        let read_dir = match std::fs::read_dir(&dir_path) {
            Ok(rd) => rd,
            Err(_) => return Vec::new(),
        };

        let basename_lower = basename.to_ascii_lowercase();
        let mut out: Vec<RawCandidate> = Vec::new();
        for entry in read_dir.flatten() {
            let name_os = entry.file_name();
            let name = name_os.to_string_lossy();
            // Filter by basename prefix (case-insensitive when
            // ctx.case_sensitive is false). Matcher will further
            // refine; this filter just prevents the candidate set
            // from being thousands of entries on big directories.
            if !ctx.case_sensitive && !name.to_ascii_lowercase().starts_with(&basename_lower) {
                continue;
            }
            if ctx.case_sensitive && !name.starts_with(basename) {
                continue;
            }
            let metadata = entry.metadata();
            let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = metadata.as_ref().ok().map(|m| m.len());
            // Reconstruct the full text the user types: the
            // original directory part plus the basename, plus
            // a trailing `/` if it's a directory.
            let mut text = String::with_capacity(dir_str.len() + name.len() + 1);
            text.push_str(dir_str);
            text.push_str(&name);
            if is_dir {
                text.push('/');
            }
            let display = if is_dir {
                format!("{name}/")
            } else {
                name.to_string()
            };
            out.push(RawCandidate {
                text,
                display,
                kind: if is_dir {
                    CandidateKind::Directory
                } else {
                    CandidateKind::File
                },
                data: CandidateData::File {
                    path: entry.path(),
                    is_dir,
                    size,
                },
                source: None,
                accept_action: None,
            });
        }
        out.sort_by(|a, b| a.text.cmp(&b.text));
        out
    }

    fn cache_key(&self, ctx: &GenerateContext<'_>) -> Option<CacheKey> {
        // Cache per resolved directory. Two cmdlines for the same
        // directory share a cache entry; typing more chars in the
        // basename re-uses it (matcher does the filtering against
        // the cached candidate set).
        let dir_str = match ctx.prefix.rfind('/') {
            Some(i) => &ctx.prefix[..=i],
            None => "",
        };
        let dir = expand_tilde(dir_str);
        Some(CacheKey::new(format!(
            "gen:files:{}",
            dir.to_string_lossy()
        )))
    }

    fn cache_ttl(&self) -> Duration {
        // Files mutate. 1s is enough to dedupe rapid keystrokes
        // without hiding genuine filesystem changes.
        Duration::from_secs(1)
    }
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    if p.is_empty() {
        return PathBuf::from(".");
    }
    PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_core::{Buffer, Document};
    use lattice_grammar::CommandRegistry;

    fn ctx_for<'a>(
        prefix: &'a str,
        buffer: &'a Buffer,
        registry: &'a CommandRegistry,
    ) -> GenerateContext<'a> {
        GenerateContext {
            prefix,
            buffer,
            registry,
            case_sensitive: false,
        }
    }

    // ---- CommandsGenerator ----

    #[test]
    fn commands_generator_returns_one_candidate_per_registered_command() {
        let mut registry = CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut registry);
        let _ = lattice_grammar::ex_commands::populate(&mut registry);
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let g = CommandsGenerator;
        let candidates = g.generate(&ctx_for("", &buffer, &registry));
        assert!(candidates.len() > 50, "expected many built-in commands");
        // Spot-check: ex:write should be in there.
        assert!(candidates.iter().any(|c| c.text == "ex:write"));
        // Spot-check: motion:line-down should be in there.
        assert!(candidates.iter().any(|c| c.text == "motion:line-down"));
    }

    #[test]
    fn commands_generator_filters_delimiter_only_commands() {
        // ex:substitute and ex:global have SurfaceForm::Delimiter --
        // they're typed via :s/.../.../  and :g/.../.../  not by
        // name. The completion list must hide them so the user
        // doesn't see (and can't pick) a candidate that would error
        // when accepted.
        let mut registry = CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut registry);
        let _ = lattice_grammar::ex_commands::populate(&mut registry);
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let candidates = CommandsGenerator.generate(&ctx_for("", &buffer, &registry));
        assert!(
            !candidates.iter().any(|c| c.text == "ex:substitute"),
            "ex:substitute is delimiter-form-only and must not appear",
        );
        assert!(
            !candidates.iter().any(|c| c.text == "ex:global"),
            "ex:global is delimiter-form-only and must not appear",
        );
        // Other ex-commands stay present.
        assert!(candidates.iter().any(|c| c.text == "ex:write"));
    }

    #[test]
    fn commands_generator_emits_doc_and_kind_label_in_data() {
        let mut registry = CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut registry);
        let _ = lattice_grammar::ex_commands::populate(&mut registry);
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let candidates = CommandsGenerator.generate(&ctx_for("", &buffer, &registry));
        let write = candidates.iter().find(|c| c.text == "ex:write").unwrap();
        match &write.data {
            CandidateData::Command {
                doc, kind_label, ..
            } => {
                assert!(!doc.is_empty(), "ex:write should have a doc");
                assert_eq!(kind_label, "ex-command");
            }
            other => panic!("expected Command data, got {other:?}"),
        }
    }

    #[test]
    fn commands_generator_caches_with_fixed_key() {
        let registry = CommandRegistry::new();
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let g = CommandsGenerator;
        let key = g.cache_key(&ctx_for("anything", &buffer, &registry));
        assert_eq!(key, Some(CacheKey::new("gen:commands:v1")));
    }

    #[test]
    fn commands_generator_cache_key_independent_of_prefix() {
        // Verify cache key is the same regardless of prefix --
        // matcher does the filtering against the cached set, so
        // typing more chars shouldn't invalidate.
        let registry = CommandRegistry::new();
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let g = CommandsGenerator;
        let k1 = g.cache_key(&ctx_for("", &buffer, &registry));
        let k2 = g.cache_key(&ctx_for("ex:wri", &buffer, &registry));
        assert_eq!(k1, k2);
    }

    // ---- FilesGenerator ----

    #[test]
    fn files_generator_lists_current_directory_when_prefix_empty() {
        let registry = CommandRegistry::new();
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let g = FilesGenerator;
        // Run from the workspace root; the test harness's cwd is
        // the crate dir.
        let candidates = g.generate(&ctx_for("", &buffer, &registry));
        // Should at least produce some entries (Cargo.toml, src/, etc.).
        assert!(!candidates.is_empty(), "expected non-empty cwd listing");
    }

    #[test]
    fn files_generator_filters_by_basename_prefix() {
        let registry = CommandRegistry::new();
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let g = FilesGenerator;
        let candidates = g.generate(&ctx_for("Carg", &buffer, &registry));
        assert!(candidates.iter().any(|c| c.text.starts_with("Carg")));
    }

    #[test]
    fn files_generator_marks_directories_with_trailing_slash() {
        let registry = CommandRegistry::new();
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let g = FilesGenerator;
        let candidates = g.generate(&ctx_for("", &buffer, &registry));
        let src_entry = candidates.iter().find(|c| c.display.starts_with("src"));
        if let Some(entry) = src_entry {
            // src is a directory; should have trailing /.
            assert_eq!(entry.kind, CandidateKind::Directory);
            assert!(entry.display.ends_with('/'));
            assert!(entry.text.ends_with('/'));
        }
    }

    #[test]
    fn files_generator_completes_nested_path_with_slash() {
        // Regression guard: FilesGenerator must walk the
        // directory referenced by everything before the LAST `/`
        // in the prefix and filter remaining entries by the
        // basename suffix. Without this, `:e crates/latt<Tab>`
        // would fail to surface any candidates.
        let tmp =
            std::env::temp_dir().join(format!("lattice-files-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let sub = tmp.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        for n in ["alpha", "beta", "zeta"] {
            std::fs::write(sub.join(n), "").unwrap();
        }
        let prev_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&tmp).unwrap();

        let registry = CommandRegistry::new();
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let g = FilesGenerator;
        let candidates = g.generate(&ctx_for("sub/al", &buffer, &registry));

        std::env::set_current_dir(&prev_cwd).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);

        assert!(
            candidates.iter().any(|c| c.text == "sub/alpha"),
            "expected `sub/alpha` candidate, got {} candidates: {:?}",
            candidates.len(),
            candidates.iter().map(|c| &c.text).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn files_generator_returns_empty_for_nonexistent_directory() {
        let registry = CommandRegistry::new();
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let g = FilesGenerator;
        let candidates = g.generate(&ctx_for("/this/path/should/not/exist/", &buffer, &registry));
        assert!(candidates.is_empty());
    }

    #[test]
    fn files_generator_cache_key_is_per_directory() {
        let registry = CommandRegistry::new();
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let g = FilesGenerator;
        let k_a = g.cache_key(&ctx_for("/tmp/foo", &buffer, &registry));
        let k_b = g.cache_key(&ctx_for("/tmp/foo/", &buffer, &registry));
        let k_c = g.cache_key(&ctx_for("/var/", &buffer, &registry));
        // /tmp/foo (no slash) keys to "" because no slash before.
        // We deliberately bucket by directory; basename doesn't
        // change the key.
        assert_ne!(k_a, k_b);
        assert_ne!(k_b, k_c);
    }

    #[test]
    fn files_generator_uses_short_ttl() {
        let g = FilesGenerator;
        assert_eq!(g.cache_ttl(), Duration::from_secs(1));
    }
}
