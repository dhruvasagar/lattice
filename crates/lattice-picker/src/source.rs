//! Picker source registry (metadata layer).
//!
//! Each picker source — `files`, `recent`, `lines`, `marks`,
//! `lsp-references`, ... — registers a [`PickerSourceSpec`]
//! into a [`PickerRegistry`] at boot. The registry powers
//! three things:
//!
//! 1. **`:picker <Tab>` completion.** The cmdline source-id
//!    completion mode iterates the registry and surfaces every
//!    registered source as a candidate.
//! 2. **Source-arg completion.** Once the source id is resolved,
//!    arg-2+ completion consults the source's
//!    [`ArgSpec::completion`] hooks — same `gen:*` completion
//!    sources every other ex-command uses.
//! 3. **`:describe-picker` introspection.** Walks the registry
//!    to render `:describe-picker` (and `:describe-picker <id>`
//!    for per-source detail).
//!
//! The registry only holds **metadata** at this stage. The
//! `PickerSourceGenerator` trait (slice 4 in
//! `docs/dev/architecture/picker.md`) elevates the registry to
//! hold generator trait objects so source dispatch is registry-
//! driven end-to-end. Today the App still owns the
//! `source_id → method` dispatch table; the registry just
//! supplies the names + arg schemas the grammar needs.
//!
//! ## WIT mirror (Phase 7)
//!
//! When the plugin host lands, WIT-imported sources register
//! their spec record into the same `PickerRegistry`. Plugin
//! sources are indistinguishable from first-party at the
//! registry level — both appear under `:picker <Tab>` and
//! flow through the same dispatch.
//!
//! The registry interface is therefore deliberately small:
//! `register`, `get`, `iter`. Nothing host-specific leaks in.

use std::collections::HashMap;

use lattice_grammar::args::ArgSpec;

/// Static metadata describing one picker source.
///
/// `id` is the stable name the user types after `:picker`
/// (e.g. `files`, `lsp-references`). `doc` is one line shown
/// in `:describe-picker` and next to the id in cmdline
/// completion. `args_schema` describes positional args after
/// the source id — same `ArgSpec` machinery the rest of the
/// grammar uses, so `:picker grep <pat> <Tab>` completes
/// through the existing `gen:*` source plumbing.
#[derive(Debug, Clone)]
pub struct PickerSourceSpec {
    pub id: &'static str,
    pub doc: &'static str,
    pub args_schema: Vec<ArgSpec>,
    /// Parameter-hint line shown while the user is typing args
    /// after the source id. Empty string = no hint (the
    /// cmdline falls back to per-arg `ArgSpec::doc`).
    pub args_hint: &'static str,
}

impl PickerSourceSpec {
    /// Sugar for declaring a no-arg picker source (`files`,
    /// `recent`, `buffers`, etc.).
    pub fn no_args(id: &'static str, doc: &'static str) -> Self {
        Self {
            id,
            doc,
            args_schema: Vec::new(),
            args_hint: "",
        }
    }
}

/// Registry of every picker source the `:picker <id>` ex-command
/// can dispatch to. Populated at boot by each feature crate's
/// `register_picker_sources` entry point.
///
/// Re-registering an id overwrites the previous entry — last
/// writer wins. In practice each id is registered exactly once
/// at boot; the overwrite semantics make tests trivial to write
/// (`register` twice with different specs to assert the second
/// wins).
#[derive(Debug, Default)]
pub struct PickerRegistry {
    sources: HashMap<&'static str, PickerSourceSpec>,
}

impl PickerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: PickerSourceSpec) {
        self.sources.insert(spec.id, spec);
    }

    pub fn get(&self, id: &str) -> Option<&PickerSourceSpec> {
        self.sources.get(id)
    }

    /// Walk every registered source in id-sorted order.
    /// Deterministic for tab-completion and `:describe-picker`
    /// listings; tests can rely on the order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &PickerSourceSpec)> + '_ {
        let mut ids: Vec<&'static str> = self.sources.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter().map(move |id| (id, &self.sources[id]))
    }

    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        let mut ids: Vec<&'static str> = self.sources.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn spec(id: &'static str) -> PickerSourceSpec {
        PickerSourceSpec::no_args(id, "test source")
    }

    #[test]
    fn new_registry_is_empty() {
        let reg = PickerRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.get("files").is_none());
    }

    #[test]
    fn register_and_get_round_trip() {
        let mut reg = PickerRegistry::new();
        reg.register(spec("files"));
        assert_eq!(reg.len(), 1);
        let got = reg.get("files").unwrap();
        assert_eq!(got.id, "files");
        assert_eq!(got.doc, "test source");
    }

    #[test]
    fn iter_yields_sources_in_id_order() {
        let mut reg = PickerRegistry::new();
        reg.register(spec("recent"));
        reg.register(spec("buffers"));
        reg.register(spec("files"));
        reg.register(spec("lines"));
        let ids: Vec<&'static str> = reg.iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec!["buffers", "files", "lines", "recent"]);
    }

    #[test]
    fn re_registering_same_id_overwrites_previous_entry() {
        let mut reg = PickerRegistry::new();
        reg.register(PickerSourceSpec::no_args("files", "first"));
        reg.register(PickerSourceSpec::no_args("files", "second"));
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("files").unwrap().doc, "second");
    }

    #[test]
    fn ids_iterator_matches_iter_keys() {
        let mut reg = PickerRegistry::new();
        reg.register(spec("zeta"));
        reg.register(spec("alpha"));
        reg.register(spec("mu"));
        let ids: Vec<&'static str> = reg.ids().collect();
        assert_eq!(ids, vec!["alpha", "mu", "zeta"]);
    }
}
