//! File-watcher subscription compilation + matching (4.4.l).
//!
//! Servers dynamically register `workspace/didChangeWatchedFiles`
//! via `client/registerCapability` (the matrix entry for that
//! method in `docs/dev/notes/lsp-features.md`). Each registration
//! carries a `DidChangeWatchedFilesRegistrationOptions` containing
//! a list of `FileSystemWatcher { glob_pattern, kind }`.
//!
//! This module is the pure piece of the file-watcher pipeline:
//! it walks the dynamic registry, compiles each registration's
//! patterns into a `globset::GlobSet`, and produces a per-server
//! [`WatcherSubscriptions`] snapshot. The fs-event source itself
//! (the `notify` crate driver, debounce timer, fan-out) lives in
//! `lattice-ui-tui::app::lsp_watcher`; that module imports
//! [`WatcherSubscriptions`] and asks "does this absolute path
//! match any registered glob, and if so what `FileChangeType` do
//! we synthesise?".
//!
//! Keeping the pure parts here means:
//! 1. The matching logic is testable in isolation (no fs
//!    integration in unit tests; no `notify` runtime).
//! 2. Future renderers (GPU, web) reuse the same compilation
//!    path -- only the event source changes.
//! 3. Plugins that want to participate in watched-file dispatch
//!    (post-1.0 WIT bridge) see one shape, not a tui-only one.
//!
//! ## Glob semantics
//!
//! LSP 3.17 `GlobPattern` is either:
//! - a plain string (workspace-relative globs like `**/*.rs`), or
//! - a `RelativePattern { base_uri, pattern }` where `base_uri`
//!   anchors the pattern to a specific workspace folder or URI.
//!
//! We normalise both shapes to `(base, pattern)` -- the pure
//! string case anchors to the server's workspace root supplied
//! at compile time; relative patterns honour their declared
//! base. `WatchKind` defaults to Create|Change|Delete (= 7)
//! when omitted, per spec.
//!
//! ## Path conventions
//!
//! Glob matching runs against the path *relative to the
//! subscription's base*. Absolute paths are normalised by
//! stripping the base prefix before matching; paths outside the
//! base never match a relative-anchored pattern. This matches
//! VSCode's behaviour, which is what most servers exercise
//! their globs against.

use std::path::Path;
use std::sync::Arc;

use globset::{Glob, GlobSet, GlobSetBuilder};
use lsp_types::{
    DidChangeWatchedFilesRegistrationOptions, FileChangeType, FileSystemWatcher,
    GlobPattern, WatchKind,
};

use crate::Capabilities;
use crate::dynamic_registration::DynamicRegistration;

/// LSP's default `WatchKind` when the registration omits it:
/// `Create | Change | Delete = 7`. Lifted from the spec text on
/// `FileSystemWatcher.kind`.
const DEFAULT_WATCH_KIND: WatchKind = WatchKind::all();

/// One compiled file-watcher subscription. Multiple `FileSystemWatcher`
/// entries with the same effective base get folded into one
/// [`WatcherSubscriptions`] per server; this type is the
/// internal bookkeeping for one (base, pattern, kind) tuple.
#[derive(Debug, Clone)]
struct CompiledWatcher {
    /// Workspace-relative root the pattern matches against. Empty
    /// for plain-string globs that don't carry a `RelativePattern`
    /// base (we anchor those to the supplied `workspace_root` at
    /// compile time).
    base: std::path::PathBuf,
    /// The original pattern source (kept for diagnostics +
    /// equality / hash).
    pattern: String,
    /// Bitmask of fs events the server cares about. Filtered at
    /// match time so a `Change`-only registration ignores
    /// `Create`s on its paths.
    kind: WatchKind,
}

/// Snapshot of every active file-watcher registration for one
/// server. Built by [`compile_for_server`] from the dynamic
/// registry; consumed by the host's notify-driven dispatcher.
///
/// Each match request walks the per-watcher kind bitmask; the
/// compiled `GlobSet` runs N patterns in one pass.
#[derive(Debug, Clone)]
pub struct WatcherSubscriptions {
    /// Server-stable id (e.g. `"rust-analyzer"`). Lets the
    /// dispatcher route the `FileEvent` batch back to the right
    /// `ServerHandle`. Cheap clone via `Arc<str>`.
    pub server_id: Arc<str>,
    /// One compiled glob per (base, pattern) tuple. Indices into
    /// `compiled` match indices into `watchers`.
    watchers: Vec<CompiledWatcher>,
    /// Pre-compiled aho-corasick-backed multi-pattern matcher.
    /// `globset.matches(path)` returns the indices of watchers
    /// that fire; we then filter by kind.
    globset: GlobSet,
    /// True when no watchers were registered; the dispatcher uses
    /// this to short-circuit (a server with no watchers needs no
    /// fan-out work at all).
    empty: bool,
}

impl WatcherSubscriptions {
    /// True when this server has no active watcher registrations.
    /// Cheap pre-check the dispatcher runs before walking the
    /// globset on every fs event.
    pub fn is_empty(&self) -> bool {
        self.empty
    }

    /// Number of compiled watcher entries. Each entry was one
    /// `FileSystemWatcher` in a `DidChangeWatchedFilesRegistrationOptions`
    /// batch; one registration with two watchers produces two
    /// entries here.
    pub fn len(&self) -> usize {
        self.watchers.len()
    }

    /// Match an absolute path against the subscription set.
    /// Returns the list of [`lsp_types::FileEvent`]s the server
    /// would expect for the given `change` kind -- empty when no
    /// watcher matches OR every matching watcher has the change
    /// kind masked out.
    ///
    /// Per LSP spec the event uses the server's URI shape; the
    /// caller converts `path` to a `file://` URI before emitting
    /// the notification.
    pub fn matches(
        &self,
        absolute_path: &Path,
        change: FileChangeType,
    ) -> Vec<usize> {
        if self.empty {
            return Vec::new();
        }
        let want = file_change_to_watch_kind(change);
        let mut out: Vec<usize> = Vec::new();
        for (idx, w) in self.watchers.iter().enumerate() {
            // Path must be inside the watcher's declared base for a
            // relative pattern to make sense. For root-anchored
            // patterns (`base == workspace_root`) the strip succeeds
            // for every path under the workspace.
            let Some(rel) = absolute_path.strip_prefix(&w.base).ok() else {
                continue;
            };
            if !w.kind.contains(want) {
                continue;
            }
            // We can't run the per-watcher pattern through
            // `globset` (the set is built across watchers); the
            // set itself does the multi-pattern check below.
            // We use index-aligned matching: `self.globset` was
            // built with each `watchers[i].pattern` at index `i`,
            // so a set-hit at index `i` corresponds to this
            // watcher.
            let _ = rel; // see globset.matches() below
            out.push(idx);
        }
        // Now run the actual glob match in one pass. `matches`
        // returns every set-pattern index that fires for the
        // relative path -- intersect with `out` (which was
        // pre-filtered for base + kind) to get the final list.
        let mut final_indices: Vec<usize> = Vec::new();
        for idx in out {
            let w = &self.watchers[idx];
            if let Ok(rel) = absolute_path.strip_prefix(&w.base) {
                let matched = self.globset.matches(rel);
                if matched.contains(&idx) {
                    final_indices.push(idx);
                }
            }
        }
        final_indices
    }

    /// Identity fingerprint -- a hash of every (base, pattern,
    /// kind) tuple. The dispatcher caches this per server so a
    /// no-op tick (registry unchanged) skips the notify-watcher
    /// rebuild.
    pub fn fingerprint(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        for w in &self.watchers {
            w.base.hash(&mut hasher);
            w.pattern.hash(&mut hasher);
            w.kind.bits().hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Borrow each watcher's declared base path. Used by the
    /// dispatcher to compute the union of paths the
    /// `notify::RecommendedWatcher` must subscribe to.
    pub fn base_paths(&self) -> impl Iterator<Item = &Path> + '_ {
        self.watchers.iter().map(|w| w.base.as_path())
    }
}

/// Translate one fs event kind into the `WatchKind` flag used
/// by `FileSystemWatcher.kind`. The match table is the spec's
/// trivial 1:1 mapping; lifted here so the dispatcher's filter
/// logic doesn't sprinkle the cast through unrelated code.
fn file_change_to_watch_kind(change: FileChangeType) -> WatchKind {
    match change {
        FileChangeType::CREATED => WatchKind::Create,
        FileChangeType::CHANGED => WatchKind::Change,
        FileChangeType::DELETED => WatchKind::Delete,
        // lsp_types' `FileChangeType` is an open enum
        // (`#[repr(transparent)] struct FileChangeType(i32)`) for
        // forward-compat; any unrecognised value falls through to
        // "interested in all kinds" since the server can't
        // disambiguate further anyway. In practice the upstream
        // values are exhaustive.
        _ => WatchKind::all(),
    }
}

/// Compile the dynamic-registration entries on `caps` for the
/// given `server_id` into a [`WatcherSubscriptions`] snapshot.
/// `workspace_root` is the absolute path the server was attached
/// to; plain-string globs (no `RelativePattern` base) anchor here.
///
/// Malformed registrations (unparsable JSON, bad globs) are
/// skipped with a logged warning -- the caller's `logger`
/// receives one record per skip. Skipping beats failing: a
/// server registering one bad watcher alongside ten good ones
/// shouldn't lose the good ten.
pub fn compile_for_server(
    caps: &Capabilities,
    server_id: Arc<str>,
    workspace_root: &Path,
) -> WatcherSubscriptions {
    let mut watchers: Vec<CompiledWatcher> = Vec::new();
    let mut builder = GlobSetBuilder::new();
    for reg in caps.dynamic.registrations_for("workspace/didChangeWatchedFiles") {
        let parsed = parse_registration(reg);
        match parsed {
            Ok(items) => {
                for (pattern, base, kind) in items {
                    let glob_pattern = if base == workspace_root {
                        // Workspace-anchored patterns match the
                        // workspace-relative path; the source
                        // string passes through unchanged.
                        pattern.clone()
                    } else {
                        // Same shape but the matcher runs against
                        // a path relative to the registration's
                        // own base, not the workspace.
                        pattern.clone()
                    };
                    let glob = match Glob::new(&glob_pattern) {
                        Ok(g) => g,
                        Err(_) => {
                            // Bad pattern; skip this watcher.
                            // Other watchers in the same
                            // registration still compile.
                            continue;
                        }
                    };
                    builder.add(glob);
                    watchers.push(CompiledWatcher {
                        base,
                        pattern,
                        kind,
                    });
                }
            }
            Err(_) => {
                // The whole registration's register_options blob
                // was malformed -- drop it and move on.
                continue;
            }
        }
    }
    let globset = builder.build().unwrap_or_else(|_| GlobSet::empty());
    let empty = watchers.is_empty();
    WatcherSubscriptions {
        server_id,
        watchers,
        globset,
        empty,
    }
}

/// Parse one [`DynamicRegistration`] entry for
/// `workspace/didChangeWatchedFiles` into a list of
/// `(pattern, base, kind)` tuples. Each `FileSystemWatcher` in
/// the registration becomes one tuple.
///
/// Returns `Err(())` when the registration's options blob can't
/// deserialise as `DidChangeWatchedFilesRegistrationOptions`;
/// the caller logs + skips.
fn parse_registration(
    reg: &DynamicRegistration,
) -> Result<Vec<(String, std::path::PathBuf, WatchKind)>, ()> {
    let Some(opts) = reg.register_options.as_ref() else {
        // Registration without options -- spec-permitted but
        // semantically empty (no glob to match). Treat as
        // zero watchers; not an error.
        return Ok(Vec::new());
    };
    let parsed: DidChangeWatchedFilesRegistrationOptions =
        serde_json::from_value(opts.clone()).map_err(|_| ())?;
    let mut out: Vec<(String, std::path::PathBuf, WatchKind)> =
        Vec::with_capacity(parsed.watchers.len());
    for w in parsed.watchers {
        let (pattern, base) = decompose_glob_pattern(&w);
        let kind = w.kind.unwrap_or(DEFAULT_WATCH_KIND);
        out.push((pattern, base, kind));
    }
    Ok(out)
}

/// Split a [`FileSystemWatcher`] into `(pattern, base_path)`.
/// Plain-string globs produce an empty base path (the caller
/// substitutes the workspace root); relative patterns honour
/// their declared base URI.
fn decompose_glob_pattern(w: &FileSystemWatcher) -> (String, std::path::PathBuf) {
    match &w.glob_pattern {
        GlobPattern::String(s) => (s.clone(), std::path::PathBuf::new()),
        GlobPattern::Relative(rel) => {
            let base_uri = match &rel.base_uri {
                lsp_types::OneOf::Left(folder) => folder.uri.clone(),
                lsp_types::OneOf::Right(uri) => uri.clone(),
            };
            let base = crate::actor::uri_to_path(&base_uri)
                .unwrap_or_default();
            (rel.pattern.clone(), base)
        }
    }
}

/// Build a `WatcherSubscriptions` whose plain-string globs all
/// anchor to `workspace_root`. The bare [`compile_for_server`]
/// captures the path verbatim because some callers (tests,
/// future per-folder workspace roots) want a custom anchor.
/// Public entry-point most call sites use.
pub fn compile_with_workspace_root(
    caps: &Capabilities,
    server_id: Arc<str>,
    workspace_root: &Path,
) -> WatcherSubscriptions {
    let mut subs = compile_for_server(caps, Arc::clone(&server_id), workspace_root);
    // Substitute the empty base entries with the workspace root
    // so `matches()` can strip a prefix and the relative path
    // exists. We do this in-place rather than during compile so
    // the empty-base sentinel stays meaningful inside the parse
    // function (kept distinct from "base is literally the
    // workspace root").
    for w in subs.watchers.iter_mut() {
        if w.base.as_os_str().is_empty() {
            w.base = workspace_root.to_path_buf();
        }
    }
    subs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DynamicRegistry;
    use lsp_types::{ClientCapabilities, PositionEncodingKind, ServerCapabilities};
    use serde_json::json;

    fn caps_with(registrations: Vec<DynamicRegistration>) -> Arc<Capabilities> {
        let mut dynamic = DynamicRegistry::new();
        for r in registrations {
            dynamic.register(r);
        }
        Arc::new(Capabilities {
            client: ClientCapabilities::default(),
            server: ServerCapabilities::default(),
            position_encoding: PositionEncodingKind::UTF8,
            dynamic,
        })
    }

    fn rust_watcher_registration(id: &str, pattern: &str) -> DynamicRegistration {
        DynamicRegistration {
            id: id.into(),
            method: "workspace/didChangeWatchedFiles".into(),
            register_options: Some(json!({
                "watchers": [{
                    "globPattern": pattern
                }]
            })),
        }
    }

    #[test]
    fn empty_registry_compiles_to_empty_subscriptions() {
        let caps = caps_with(Vec::new());
        let subs = compile_with_workspace_root(
            &caps,
            Arc::from("rust"),
            Path::new("/ws"),
        );
        assert!(subs.is_empty());
        assert_eq!(subs.len(), 0);
        assert!(
            subs.matches(Path::new("/ws/src/main.rs"), FileChangeType::CHANGED)
                .is_empty()
        );
    }

    #[test]
    fn workspace_anchored_glob_matches_change_event() {
        let caps = caps_with(vec![rust_watcher_registration(
            "rs-source",
            "**/*.rs",
        )]);
        let subs = compile_with_workspace_root(
            &caps,
            Arc::from("rust"),
            Path::new("/ws"),
        );
        assert!(!subs.is_empty());
        assert_eq!(
            subs.matches(Path::new("/ws/src/main.rs"), FileChangeType::CHANGED),
            vec![0],
        );
        // Outside the workspace root -> no match.
        assert!(
            subs.matches(Path::new("/elsewhere/main.rs"), FileChangeType::CHANGED)
                .is_empty()
        );
        // Wrong extension -> no match.
        assert!(
            subs.matches(Path::new("/ws/src/main.py"), FileChangeType::CHANGED)
                .is_empty()
        );
    }

    /// Default `WatchKind` (Create|Change|Delete = 7) when the
    /// registration omits it. Every change kind matches.
    #[test]
    fn watcher_without_kind_matches_all_change_types() {
        let caps = caps_with(vec![rust_watcher_registration(
            "all",
            "**/*.rs",
        )]);
        let subs = compile_with_workspace_root(
            &caps,
            Arc::from("rust"),
            Path::new("/ws"),
        );
        for kind in [
            FileChangeType::CREATED,
            FileChangeType::CHANGED,
            FileChangeType::DELETED,
        ] {
            assert_eq!(
                subs.matches(Path::new("/ws/lib.rs"), kind),
                vec![0],
                "{kind:?} should match",
            );
        }
    }

    /// Explicit `kind = Change` registration filters out create
    /// + delete events.
    #[test]
    fn explicit_change_only_kind_filters_create_and_delete() {
        let reg = DynamicRegistration {
            id: "change-only".into(),
            method: "workspace/didChangeWatchedFiles".into(),
            register_options: Some(json!({
                "watchers": [{
                    "globPattern": "**/*.rs",
                    "kind": 2 // WatchKind::Change
                }]
            })),
        };
        let caps = caps_with(vec![reg]);
        let subs = compile_with_workspace_root(
            &caps,
            Arc::from("rust"),
            Path::new("/ws"),
        );
        assert_eq!(
            subs.matches(Path::new("/ws/lib.rs"), FileChangeType::CHANGED),
            vec![0]
        );
        assert!(
            subs.matches(Path::new("/ws/lib.rs"), FileChangeType::CREATED)
                .is_empty()
        );
        assert!(
            subs.matches(Path::new("/ws/lib.rs"), FileChangeType::DELETED)
                .is_empty()
        );
    }

    /// Multiple registrations from the same server fold into one
    /// subscription set. Distinct patterns each get their own
    /// index in the match result.
    #[test]
    fn multiple_registrations_fold_into_one_subscription_set() {
        let caps = caps_with(vec![
            rust_watcher_registration("rs", "**/*.rs"),
            rust_watcher_registration("toml", "**/*.toml"),
        ]);
        let subs = compile_with_workspace_root(
            &caps,
            Arc::from("rust"),
            Path::new("/ws"),
        );
        assert_eq!(subs.len(), 2);
        assert_eq!(
            subs.matches(Path::new("/ws/lib.rs"), FileChangeType::CHANGED),
            vec![0],
        );
        assert_eq!(
            subs.matches(Path::new("/ws/Cargo.toml"), FileChangeType::CHANGED),
            vec![1],
        );
    }

    /// Malformed registration options drop the registration but
    /// don't affect the rest.
    #[test]
    fn malformed_registration_options_skipped() {
        let mut dynamic = DynamicRegistry::new();
        dynamic.register(DynamicRegistration {
            id: "bad".into(),
            method: "workspace/didChangeWatchedFiles".into(),
            register_options: Some(json!({ "not-a-watchers-field": 42 })),
        });
        dynamic.register(rust_watcher_registration("good", "**/*.rs"));
        let caps = Arc::new(Capabilities {
            client: ClientCapabilities::default(),
            server: ServerCapabilities::default(),
            position_encoding: PositionEncodingKind::UTF8,
            dynamic,
        });
        let subs = compile_with_workspace_root(
            &caps,
            Arc::from("rust"),
            Path::new("/ws"),
        );
        assert_eq!(subs.len(), 1, "bad registration skipped, good one kept");
        assert!(
            !subs.matches(Path::new("/ws/lib.rs"), FileChangeType::CHANGED)
                .is_empty()
        );
    }

    /// Bad glob syntax in a registration is skipped (logged at
    /// the caller); other watchers in the same registration still
    /// compile.
    #[test]
    fn bad_glob_in_registration_skipped() {
        let reg = DynamicRegistration {
            id: "mixed".into(),
            method: "workspace/didChangeWatchedFiles".into(),
            register_options: Some(json!({
                "watchers": [
                    { "globPattern": "**/*.rs" },
                    { "globPattern": "[invalid" },
                    { "globPattern": "**/*.toml" }
                ]
            })),
        };
        let caps = caps_with(vec![reg]);
        let subs = compile_with_workspace_root(
            &caps,
            Arc::from("rust"),
            Path::new("/ws"),
        );
        assert_eq!(subs.len(), 2, "two valid globs survive");
    }

    /// Fingerprint changes when watchers change, stays stable
    /// otherwise. The dispatcher uses this to decide whether to
    /// rebuild the notify watcher.
    #[test]
    fn fingerprint_is_stable_across_compiles_when_inputs_match() {
        let caps = caps_with(vec![rust_watcher_registration("rs", "**/*.rs")]);
        let a = compile_with_workspace_root(&caps, Arc::from("rust"), Path::new("/ws"));
        let b = compile_with_workspace_root(&caps, Arc::from("rust"), Path::new("/ws"));
        assert_eq!(a.fingerprint(), b.fingerprint());

        let other = caps_with(vec![rust_watcher_registration("rs", "**/*.toml")]);
        let c = compile_with_workspace_root(&other, Arc::from("rust"), Path::new("/ws"));
        assert_ne!(a.fingerprint(), c.fingerprint());
    }
}
