//! Languages registered at runtime rather than compiled in.
//!
//! Design:
//! [`plugin-languages.md`](../../../docs/dev/architecture/plugin-languages.md) §2.3.
//! Slice plan: LG.2.
//!
//! `Lang` is a closed enum, and every language lattice supports is a
//! workspace dependency. This module is the other half: a registry a
//! plugin contributes to at load and is withdrawn from at unload, so
//! "which languages exist" stops being a compile-time property of the
//! editor (paramount goal #2).
//!
//! ## Why the name is the id
//!
//! [`LanguageName`] is a `Copy` newtype over a `&'static str`, not an
//! index into a table. Two things fall out of that, and both were the
//! reason for choosing it:
//!
//! - **`Lang::name()` stays a field read.** It is the key every query
//!   lookup already uses — `registry.highlights_query(lang.name())`,
//!   six times per highlight invocation, plus folds and indents. An
//!   index would have put a process-global table read inside it
//!   (paramount goal #1).
//! - **`LangRegistry` is already keyed by `&'static str`.** A plugin
//!   language joins the map native languages live in, under its own
//!   name, rather than needing a parallel index space.
//!
//! The name is leaked, once per *distinct* name — [`LanguageName::intern`]
//! dedupes, so a plugin reloaded fifty times in a dev session leaks one
//! string, not fifty. A leaked name also means a buffer still holding
//! `Lang::Plugin(name)` after its plugin unloads keeps naming itself
//! correctly; it simply finds no grammar and renders as plain text.
//! Nothing dangles, and there is no kind-branch anywhere to express it.
//!
//! ## Why reads are process-global
//!
//! Writes go through an RCU handle with teardown by provenance, exactly
//! as [`contributable-registries.md`] prescribes. Reads do not take a
//! handle, because [`Lang::detect_from_path`] is a free function with
//! nineteen call sites across `lattice-host`, `lattice-magit` and
//! `lattice-multibuffer`. Threading a handle through them would make
//! plugin languages visible on some paths and invisible on others —
//! a two-tier language concept, and the same failure the "no
//! kind-specific logic" rule forbids for buffers. `Lang::Plugin` must be
//! interpretable wherever `Lang::Rust` is, by the same code.
//! `LangRegistry::standard` is already a process-wide memo in this crate
//! for a related reason.
//!
//! [`Lang::detect_from_path`]: crate::Lang::detect_from_path
//! [`contributable-registries.md`]: ../../../docs/dev/architecture/contributable-registries.md

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use arc_swap::ArcSwap;

/// The name of a language, interned so it outlives every buffer that
/// refers to it. Doubles as the language's identity: `Lang::Plugin`
/// carries one, and it is the key into [`crate::LangRegistry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LanguageName(&'static str);

impl LanguageName {
    /// Intern `name`, leaking at most once per distinct name.
    ///
    /// Deliberately not `pub(crate)`: LG.3's seam interns the name a
    /// guest supplies, and tests construct one directly.
    pub fn intern(name: &str) -> Self {
        static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
        let names = NAMES.get_or_init(|| Mutex::new(HashSet::new()));
        let mut names = names.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = names.get(name) {
            return Self(existing);
        }
        // The one leak in this module, bounded by the set above. A
        // language name is a handful of bytes and a session registers a
        // handful of languages.
        let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
        names.insert(leaked);
        Self(leaked)
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for LanguageName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Why a registration was refused. Every variant names the offending
/// language, because a plugin author reading a log line needs to know
/// which of their contributions failed — and per LG.3's error rule a
/// failed language must not take the plugin's other contributions with
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguageRegistrationError {
    /// The name collides with a language compiled into the editor.
    /// Refused rather than shadowed: a plugin silently replacing `rust`
    /// would be a confusing failure to debug, and deliberate override
    /// is a separate question nobody has asked for yet.
    ShadowsBuiltin { name: String },
    /// Another *plugin* already registered this name.
    AlreadyRegistered { name: String },
    /// A language with no extensions can never be selected, so it is
    /// almost certainly a mistake in the manifest rather than an
    /// intentional registration.
    NoExtensions { name: String },
}

impl std::fmt::Display for LanguageRegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ShadowsBuiltin { name } => write!(
                f,
                "language '{name}' is built into the editor and cannot be replaced by a plugin"
            ),
            Self::AlreadyRegistered { name } => {
                write!(f, "language '{name}' is already registered by a plugin")
            }
            Self::NoExtensions { name } => {
                write!(f, "language '{name}' registered no file extensions")
            }
        }
    }
}

impl std::error::Error for LanguageRegistrationError {}

/// One plugin-contributed language.
///
/// LG.2 carries identity and selection only. LG.3 adds the compiled
/// grammar and queries here, beside them — the shape is chosen so that
/// is an added field rather than a restructure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageRegistration {
    pub name: LanguageName,
    /// Lower-cased, without the leading dot.
    pub extensions: Vec<String>,
    /// The host-issued id of the plugin that contributed this. `u64`
    /// rather than a `PluginId` newtype so this crate does not depend
    /// on `lattice-plugin-host`; the same choice
    /// `lattice-compilation`'s parser factories made.
    pub provenance: u64,
}

/// The set of runtime-registered languages.
///
/// `Clone` is cheap by construction — registrations sit behind `Arc`, so
/// a clone is a refcount bump per language plus one small map — which is
/// what makes the copy-on-write write path affordable. It is paid on
/// plugin load and unload, not on any read.
#[derive(Debug, Default, Clone)]
pub struct PluginLanguages {
    by_extension: HashMap<String, LanguageName>,
    registrations: Vec<Arc<LanguageRegistration>>,
}

impl PluginLanguages {
    pub fn resolve_extension(&self, ext: &str) -> Option<LanguageName> {
        self.by_extension.get(&ext.to_ascii_lowercase()).copied()
    }

    pub fn get(&self, name: LanguageName) -> Option<&Arc<LanguageRegistration>> {
        self.registrations.iter().find(|r| r.name == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<LanguageRegistration>> {
        self.registrations.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }
}

/// Copy-on-write RCU, the idiom `contributable-registries.md` §2
/// establishes and the tree already carries five times over.
pub type PluginLanguagesHandle = Arc<ArcSwap<PluginLanguages>>;

/// Set the moment anything is registered and cleared when the last
/// registration goes away.
///
/// This is what makes "native resolution is unchanged when the registry
/// is empty" true *by construction* rather than by measurement:
/// [`Lang::detect_from_path`] does one relaxed atomic load and returns,
/// never touching the `ArcSwap`. `detect_from_path` is called per hunk
/// in magit's diff highlighting, so "cheap" is not good enough — the
/// empty case has to be free.
///
/// [`Lang::detect_from_path`]: crate::Lang::detect_from_path
static ANY_REGISTERED: AtomicBool = AtomicBool::new(false);

/// The process-wide handle. See the module docs for why reads do not
/// take one explicitly.
pub fn handle() -> &'static PluginLanguagesHandle {
    static HANDLE: OnceLock<PluginLanguagesHandle> = OnceLock::new();
    HANDLE.get_or_init(|| Arc::new(ArcSwap::from_pointee(PluginLanguages::default())))
}

/// Resolve a file extension to a plugin language.
///
/// Consulted **after** the native table (see [`crate::Lang::detect_from_path`]),
/// so a plugin cannot shadow a built-in language by accident. Whether it
/// should be able to, deliberately, is a separate question — deferred
/// until someone asks for it.
pub fn resolve_extension(ext: &str) -> Option<LanguageName> {
    if !ANY_REGISTERED.load(Ordering::Relaxed) {
        return None;
    }
    handle().load().resolve_extension(ext)
}

/// Snapshot the live set. Wait-free; callers hold the `Arc` for as long
/// as they need a coherent view.
pub fn snapshot() -> Arc<PluginLanguages> {
    handle().load_full()
}

/// Register a language.
///
/// `extensions` are matched case-insensitively and may be given with or
/// without a leading dot.
pub fn register(
    name: &str,
    extensions: &[&str],
    provenance: u64,
) -> Result<LanguageName, LanguageRegistrationError> {
    if crate::Lang::builtin_by_name(name).is_some() {
        return Err(LanguageRegistrationError::ShadowsBuiltin {
            name: name.to_owned(),
        });
    }
    let normalised: Vec<String> = extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    if normalised.is_empty() {
        return Err(LanguageRegistrationError::NoExtensions {
            name: name.to_owned(),
        });
    }

    let interned = LanguageName::intern(name);
    let registration = Arc::new(LanguageRegistration {
        name: interned,
        extensions: normalised,
        provenance,
    });

    // `rcu` retries its closure under contention, so the closure must
    // stay pure — it is, and the duplicate check inside it is what makes
    // two plugins racing to claim one name resolve deterministically
    // rather than by whoever stored last.
    let mut refused = false;
    handle().rcu(|current| {
        refused = current.registrations.iter().any(|r| r.name == interned);
        if refused {
            return PluginLanguages::clone(current);
        }
        let mut next = PluginLanguages::clone(current);
        for ext in &registration.extensions {
            // Last writer wins *within* the plugin set; the native table
            // still wins over all of it at resolution time.
            next.by_extension.insert(ext.clone(), interned);
        }
        next.registrations.push(Arc::clone(&registration));
        next
    });
    if refused {
        return Err(LanguageRegistrationError::AlreadyRegistered {
            name: name.to_owned(),
        });
    }
    ANY_REGISTERED.store(true, Ordering::Relaxed);
    Ok(interned)
}

/// Withdraw every language contributed by `provenance`, returning how
/// many were removed.
///
/// Teardown is **by provenance, not by token**: there is no list of
/// handles for the caller to record and therefore none to forget. A
/// plugin that registers three languages and crashes still has all three
/// withdrawn.
pub fn unregister_plugin(provenance: u64) -> usize {
    let mut removed = 0;
    handle().rcu(|current| {
        let mut next = PluginLanguages::clone(current);
        let before = next.registrations.len();
        next.registrations.retain(|r| r.provenance != provenance);
        removed = before - next.registrations.len();
        // Rebuild the extension map from what survives rather than
        // deleting the departing language's keys: another plugin may
        // have claimed the same extension after it, and deleting by key
        // would withdraw *their* mapping too.
        next.by_extension.clear();
        for reg in &next.registrations {
            for ext in &reg.extensions {
                next.by_extension.insert(ext.clone(), reg.name);
            }
        }
        next
    });
    if removed > 0 && handle().load().registrations.is_empty() {
        ANY_REGISTERED.store(false, Ordering::Relaxed);
    }
    removed
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::Lang;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    /// The registry is process-wide and `cargo test` runs these in
    /// parallel in one process, so every test must claim names and
    /// extensions no other test uses. A shared counter is the same fix
    /// the tempdir helpers needed for the same reason.
    fn unique(tag: &str) -> (String, String, u64) {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        (
            format!("lg2{tag}{n}"),
            format!("lg2x{tag}{n}"),
            9_000_000 + n,
        )
    }

    #[test]
    fn registered_language_resolves_by_extension() {
        let (name, ext, plugin) = unique("res");
        let interned = register(&name, &[&ext], plugin).unwrap();

        let path = PathBuf::from(format!("notes.{ext}"));
        assert_eq!(
            Lang::detect_from_path(Some(&path)),
            Lang::Plugin(interned),
            "a registered extension should resolve to its plugin language"
        );
        assert_eq!(Lang::Plugin(interned).name(), name);
        assert_eq!(Lang::Plugin(interned).label(), name);

        unregister_plugin(plugin);
    }

    #[test]
    fn unload_withdraws_the_language() {
        let (name, ext, plugin) = unique("unload");
        register(&name, &[&ext], plugin).unwrap();
        let path = PathBuf::from(format!("notes.{ext}"));
        assert!(matches!(
            Lang::detect_from_path(Some(&path)),
            Lang::Plugin(_)
        ));

        assert_eq!(unregister_plugin(plugin), 1);
        assert_eq!(
            Lang::detect_from_path(Some(&path)),
            Lang::Plain,
            "after unload the extension must fall back to plain"
        );
    }

    #[test]
    fn unload_withdraws_every_language_from_that_plugin() {
        let (a, ext_a, plugin) = unique("multia");
        let (b, ext_b, _) = unique("multib");
        register(&a, &[&ext_a], plugin).unwrap();
        register(&b, &[&ext_b], plugin).unwrap();

        assert_eq!(unregister_plugin(plugin), 2, "both, without a token list");
        for ext in [&ext_a, &ext_b] {
            assert_eq!(
                Lang::detect_from_path(Some(&PathBuf::from(format!("f.{ext}")))),
                Lang::Plain
            );
        }
    }

    #[test]
    fn unloading_one_plugin_leaves_anothers_language_alone() {
        let (a, ext_a, plugin_a) = unique("isoa");
        let (b, ext_b, plugin_b) = unique("isob");
        let name_b = register(&b, &[&ext_b], plugin_b).unwrap();
        register(&a, &[&ext_a], plugin_a).unwrap();

        unregister_plugin(plugin_a);
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from(format!("f.{ext_b}")))),
            Lang::Plugin(name_b),
            "withdrawing one plugin must not rebuild away another's extensions"
        );
        unregister_plugin(plugin_b);
    }

    #[test]
    fn plugin_and_native_languages_coexist() {
        let (name, ext, plugin) = unique("coexist");
        let interned = register(&name, &[&ext], plugin).unwrap();

        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("main.rs"))),
            Lang::Rust
        );
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from(format!("a.{ext}")))),
            Lang::Plugin(interned)
        );
        unregister_plugin(plugin);
    }

    #[test]
    fn native_resolution_wins_over_a_plugin_claiming_the_same_extension() {
        let (name, _, plugin) = unique("shadowext");
        // Claiming `rs` is allowed — the native table is consulted first,
        // so the claim simply never wins. That is the design's "a plugin
        // cannot shadow a built-in language by accident".
        register(&name, &["rs"], plugin).unwrap();
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("main.rs"))),
            Lang::Rust
        );
        unregister_plugin(plugin);
    }

    #[test]
    fn native_resolution_is_unchanged_when_the_registry_is_empty() {
        // Deliberately does not touch the registry: other tests run in
        // parallel in this process, so the invariant under test is
        // "native extensions resolve natively", not "the registry is
        // empty right now".
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("a.py"))),
            Lang::Python
        );
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("a.md"))),
            Lang::Markdown
        );
        assert_eq!(Lang::detect_from_path(None), Lang::Plain);
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("a.lg2-never-registered"))),
            Lang::Plain
        );
    }

    #[test]
    fn a_plugin_may_not_shadow_a_builtin_name() {
        let err = register("rust", &["myrust"], 9_999_001).unwrap_err();
        assert_eq!(
            err,
            LanguageRegistrationError::ShadowsBuiltin {
                name: "rust".into()
            }
        );
        // And the refusal is total — no extension leaked into the map.
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("a.myrust"))),
            Lang::Plain
        );
    }

    #[test]
    fn a_name_may_not_be_registered_twice() {
        let (name, ext, plugin) = unique("dup");
        register(&name, &[&ext], plugin).unwrap();
        let err = register(&name, &["otherext"], plugin + 1).unwrap_err();
        assert_eq!(
            err,
            LanguageRegistrationError::AlreadyRegistered { name: name.clone() }
        );
        unregister_plugin(plugin);
    }

    #[test]
    fn a_language_with_no_extensions_is_refused() {
        let (name, _, plugin) = unique("noext");
        assert_eq!(
            register(&name, &[], plugin).unwrap_err(),
            LanguageRegistrationError::NoExtensions { name: name.clone() }
        );
        // A dot alone normalises to empty and is refused the same way.
        assert_eq!(
            register(&name, &["."], plugin).unwrap_err(),
            LanguageRegistrationError::NoExtensions { name }
        );
    }

    #[test]
    fn extensions_are_matched_case_insensitively_and_dots_are_optional() {
        let (name, ext, plugin) = unique("case");
        let interned = register(&name, &[&format!(".{}", ext.to_uppercase())], plugin).unwrap();
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from(format!("F.{}", ext.to_uppercase())))),
            Lang::Plugin(interned)
        );
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from(format!("f.{ext}")))),
            Lang::Plugin(interned)
        );
        unregister_plugin(plugin);
    }

    #[test]
    fn interning_the_same_name_twice_yields_one_pointer() {
        let a = LanguageName::intern("lg2-intern-probe");
        let b = LanguageName::intern("lg2-intern-probe");
        assert_eq!(a, b);
        assert!(
            std::ptr::eq(a.as_str(), b.as_str()),
            "re-interning must not leak a second copy"
        );
    }

    #[test]
    fn unregistering_an_unknown_plugin_is_a_no_op() {
        assert_eq!(unregister_plugin(9_999_999), 0);
    }

    #[test]
    fn a_stale_lang_still_names_itself_after_unload() {
        let (name, ext, plugin) = unique("stale");
        let interned = register(&name, &[&ext], plugin).unwrap();
        let held = Lang::Plugin(interned);
        unregister_plugin(plugin);

        // The buffer that was open when the plugin went away keeps a
        // usable `Lang`: it names itself, finds no grammar, and renders
        // as plain text. No dangle, and no kind-branch to express it.
        assert_eq!(held.name(), name);
        assert_eq!(held.comment_syntax().line, None);
        assert_eq!(crate::major_mode_id_for_lang(held), None);
    }
}
