//! Dynamic capability tracking (LSP §3.18.10.3 / 4.4.n).
//!
//! After `initialize`, a server may send `client/registerCapability`
//! to announce capabilities it didn't list in the initial
//! `ServerCapabilities` blob, or to attach method-specific options
//! that the static shape can't express (the canonical example is
//! `workspace/didChangeWatchedFiles` -- the glob patterns to watch
//! depend on the active project and aren't known at handshake).
//! `client/unregisterCapability` reverses an earlier registration.
//!
//! Before 4.4.n the actor accepted both requests with `null` and
//! threw the payload away; feature dispatch saw only the static
//! capability set. This module owns the "dynamic layer" the
//! server adds on top -- the [`Capabilities`](crate::Capabilities)
//! aggregate carries one of these and every `supports_*` probe
//! that cares about dynamic-only registrations consults it
//! alongside the static `ServerCapabilities` field.
//!
//! ## Indexing
//!
//! The registry indexes registrations two ways:
//!
//! 1. `by_id` (HashMap<String, DynamicRegistration>) -- so
//!    `unregisterCapability` can find an entry by the id the
//!    server picked at register time and evict it in O(1).
//! 2. `by_method` (HashMap<String, Vec<String>>) -- so feature
//!    dispatch can ask "is `textDocument/completion` registered
//!    dynamically?" without scanning the whole table. The vec
//!    holds registration ids; the registry stays a single source
//!    of truth (the actual `DynamicRegistration` lives only in
//!    `by_id`).
//!
//! ## Snapshot model
//!
//! [`Capabilities`](crate::Capabilities) is published through an
//! `arc_swap::ArcSwap` cell on the [`ServerHandle`]; readers see
//! the union of static + dynamic atomically. Mutations are
//! per-actor (only the actor task issues the swap), so writes
//! don't race with each other. The cloning cost on update is
//! the size of the registry's two HashMaps -- typically
//! single-digit entries for the lifetime of a server, so the
//! arithmetic is cheap.
//!
//! [`ServerHandle`]: crate::ServerHandle

use std::collections::HashMap;

use serde_json::Value;

/// One server-issued capability registration. The `register_options`
/// blob's interpretation is method-specific; the registry treats it
/// as opaque JSON and hands it to the consumer when they fetch the
/// entry. For methods we wire (e.g. `workspace/didChangeWatchedFiles`)
/// the consumer parses it into the lsp_types shape on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicRegistration {
    /// Server-chosen id. Stable for the registration's lifetime;
    /// `unregisterCapability` references it.
    pub id: String,
    /// LSP method the registration applies to
    /// (e.g. `"textDocument/completion"`,
    /// `"workspace/didChangeWatchedFiles"`).
    pub method: String,
    /// Method-specific options blob the server attached. `None`
    /// when the server registers without options (the entry then
    /// just means "I support this method dynamically").
    pub register_options: Option<Value>,
}

/// In-memory index of every active dynamic registration for one
/// server actor. Empty on a fresh server; mutated by the actor on
/// `client/(un)registerCapability` and snapshotted into the
/// published [`Capabilities`](crate::Capabilities) on every
/// change.
///
/// Two-way index (`by_id` + `by_method`) keeps both register
/// (O(1) append) and unregister (O(1) lookup by id) cheap, with
/// the "is method X registered?" probe also O(1) via the
/// per-method bucket.
#[derive(Debug, Clone, Default)]
pub struct DynamicRegistry {
    by_id: HashMap<String, DynamicRegistration>,
    /// `method` → list of registration ids. The vec lets one
    /// method carry multiple simultaneous registrations
    /// (servers occasionally register the same method twice
    /// with different option blobs, e.g. one watcher set per
    /// language).
    by_method: HashMap<String, Vec<String>>,
}

impl DynamicRegistry {
    /// Construct an empty registry. Used at handshake and
    /// whenever the actor restarts (each restart starts clean;
    /// the server replays its registrations).
    pub fn new() -> Self {
        Self::default()
    }

    /// True iff the registry has no entries. Cheap shortcut for
    /// the common steady-state case -- most probes ask "is this
    /// dynamically registered?" and the registry is usually
    /// empty, so we want the false branch to be O(1).
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Number of distinct registrations currently active. Each
    /// `Registration` from a `RegistrationParams` batch counts
    /// once -- duplicate ids (server error) are deduplicated by
    /// [`Self::register`].
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Add one registration. If the id is already present, the
    /// new entry replaces the old (servers shouldn't reuse ids
    /// without unregistering first, but if they do we honour
    /// the latest). The `by_method` index is updated to remove
    /// the stale entry from the old method's bucket before
    /// inserting under the new method.
    pub fn register(&mut self, reg: DynamicRegistration) {
        // If the id was registered before, clean up the old
        // method bucket so the new entry's method is the only
        // place its id appears.
        if let Some(prev) = self.by_id.get(&reg.id) {
            if prev.method != reg.method {
                if let Some(bucket) = self.by_method.get_mut(&prev.method) {
                    bucket.retain(|id| id != &reg.id);
                    if bucket.is_empty() {
                        self.by_method.remove(&prev.method);
                    }
                }
            }
        }
        let bucket = self.by_method.entry(reg.method.clone()).or_default();
        if !bucket.contains(&reg.id) {
            bucket.push(reg.id.clone());
        }
        self.by_id.insert(reg.id.clone(), reg);
    }

    /// Evict the registration matching `id`. Silently no-op when
    /// the id is absent (server may unregister speculatively
    /// after a restart; rejecting would just spam the log).
    pub fn unregister(&mut self, id: &str) {
        let Some(reg) = self.by_id.remove(id) else {
            return;
        };
        if let Some(bucket) = self.by_method.get_mut(&reg.method) {
            bucket.retain(|x| x != id);
            if bucket.is_empty() {
                self.by_method.remove(&reg.method);
            }
        }
    }

    /// True iff at least one active registration targets the
    /// given LSP method. The probe `supports_*` family calls
    /// here to OR into their static `ServerCapabilities` check.
    pub fn has(&self, method: &str) -> bool {
        self.by_method
            .get(method)
            .is_some_and(|v| !v.is_empty())
    }

    /// Borrow every registration for the given method in
    /// insertion order. Used by feature consumers that need
    /// the `register_options` blob -- e.g. the file-watcher
    /// pump (4.4.l) reads each `DidChangeWatchedFilesRegistrationOptions`
    /// to know which glob patterns to subscribe to.
    pub fn registrations_for<'a>(
        &'a self,
        method: &str,
    ) -> impl Iterator<Item = &'a DynamicRegistration> + 'a {
        self.by_method
            .get(method)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.by_id.get(id))
    }

    /// Borrow one registration by id. Used by tests and the
    /// log surface (4.4.n's `:lsp-status` extension will dump
    /// every active dynamic registration for diagnosis).
    pub fn get(&self, id: &str) -> Option<&DynamicRegistration> {
        self.by_id.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reg(id: &str, method: &str) -> DynamicRegistration {
        DynamicRegistration {
            id: id.into(),
            method: method.into(),
            register_options: None,
        }
    }

    fn reg_opts(id: &str, method: &str, opts: Value) -> DynamicRegistration {
        DynamicRegistration {
            id: id.into(),
            method: method.into(),
            register_options: Some(opts),
        }
    }

    #[test]
    fn empty_registry_has_nothing() {
        let r = DynamicRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert!(!r.has("textDocument/completion"));
        assert_eq!(r.registrations_for("textDocument/completion").count(), 0);
    }

    #[test]
    fn register_and_query_by_method() {
        let mut r = DynamicRegistry::new();
        r.register(reg("a", "textDocument/completion"));
        r.register(reg("b", "textDocument/completion"));
        r.register(reg("c", "workspace/didChangeWatchedFiles"));
        assert_eq!(r.len(), 3);
        assert!(r.has("textDocument/completion"));
        assert!(r.has("workspace/didChangeWatchedFiles"));
        assert!(!r.has("textDocument/hover"));
        let comp: Vec<&str> = r
            .registrations_for("textDocument/completion")
            .map(|x| x.id.as_str())
            .collect();
        assert_eq!(comp, vec!["a", "b"]);
    }

    #[test]
    fn unregister_evicts_from_both_indexes() {
        let mut r = DynamicRegistry::new();
        r.register(reg("a", "textDocument/completion"));
        r.register(reg("b", "textDocument/completion"));
        r.unregister("a");
        assert_eq!(r.len(), 1);
        assert!(r.has("textDocument/completion"));
        let comp: Vec<&str> = r
            .registrations_for("textDocument/completion")
            .map(|x| x.id.as_str())
            .collect();
        assert_eq!(comp, vec!["b"]);
        r.unregister("b");
        assert!(!r.has("textDocument/completion"));
        assert!(r.is_empty());
    }

    #[test]
    fn unregister_unknown_id_is_noop() {
        let mut r = DynamicRegistry::new();
        r.register(reg("a", "textDocument/completion"));
        r.unregister("does-not-exist");
        assert_eq!(r.len(), 1);
        assert!(r.has("textDocument/completion"));
    }

    /// Some servers re-register an id with a new method without
    /// an intermediate unregister (sloppy but legal per spec).
    /// The registry honours the latest entry and clears the
    /// stale method's bucket so probes don't claim phantom
    /// support.
    #[test]
    fn re_register_same_id_moves_methods() {
        let mut r = DynamicRegistry::new();
        r.register(reg("a", "textDocument/completion"));
        r.register(reg("a", "textDocument/hover"));
        assert_eq!(r.len(), 1);
        assert!(!r.has("textDocument/completion"));
        assert!(r.has("textDocument/hover"));
    }

    #[test]
    fn register_options_round_trip() {
        let mut r = DynamicRegistry::new();
        let opts = json!({ "watchers": [{ "globPattern": "**/*.rs" }] });
        r.register(reg_opts(
            "watch-rs",
            "workspace/didChangeWatchedFiles",
            opts.clone(),
        ));
        let got = r.get("watch-rs").expect("registered");
        assert_eq!(got.register_options, Some(opts));
    }
}
