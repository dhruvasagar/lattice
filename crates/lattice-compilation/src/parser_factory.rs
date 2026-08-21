//! CM.6b: the plugin-parser *factory* seam.
//!
//! Design: [`compilation-mode.md`](../../../docs/dev/architecture/compilation-mode.md)
//! §5 (the parser registry as an extensibility seam).
//!
//! ## Why a factory and not a parser
//!
//! A [`CompilationParser`] carries pending multi-line state behind
//! `&mut self`, and a run has **two** readers — stdout and stderr —
//! each on its own thread with its own [`ParserRegistry`]. Those
//! streams are independent: a header line on stderr must not prime a
//! diagnostic that a stdout line then completes. So a parser cannot be
//! shared between them, and for a WASM-backed parser it *could* not be
//! anyway (it owns a `wasmtime::Store`).
//!
//! What the registry therefore holds is not a parser but the means to
//! mint one: each reader calls [`CompilationParserFactories::create_all`]
//! once at the top of its loop and owns what it gets back for the run.
//!
//! ## Lifetime
//!
//! Registration is copy-on-write RCU behind an `ArcSwap` — the same
//! wait-free-read / rare-write idiom the picker registry uses. Reads
//! happen once per run (not per line), writes only on plugin load and
//! unload. Each run snapshots the handle once, so a plugin loaded
//! mid-build joins the *next* build rather than half of this one.
//!
//! [`ParserRegistry`]: crate::ParserRegistry

use std::sync::Arc;

use crate::parser::CompilationParser;

/// Mints a fresh [`CompilationParser`] for one pipe reader.
///
/// Implemented by `lattice-plugin-host` over a compiled `error-parser`
/// component; the compilation crate never learns what a plugin is.
pub trait CompilationParserFactory: Send + Sync + std::fmt::Debug {
    /// The host-issued plugin id that contributed this factory.
    /// Teardown removes by it — see
    /// [`CompilationParserFactories::unregister_plugin`].
    fn plugin_id(&self) -> u64;

    /// Mint a parser for one reader.
    ///
    /// `None` means instantiation failed. The implementor logs it; the
    /// run continues with whatever parsers it did get, because one bad
    /// plugin must cost its own entries and not the build (the same
    /// contract `WasmErrorParser`'s trap-poisoning follows).
    fn create(&self) -> Option<Box<dyn CompilationParser>>;
}

/// The registered set of plugin parser factories, in registration
/// order.
///
/// `Clone` so the handle below can RCU it: clone → mutate → store.
/// Cloning is cheap — the factories are `Arc`-shared.
#[derive(Default, Clone, Debug)]
pub struct CompilationParserFactories {
    factories: Vec<Arc<dyn CompilationParserFactory>>,
}

impl CompilationParserFactories {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a factory. Order is preserved and becomes the order the
    /// minted parsers see each line in.
    pub fn register(&mut self, factory: Arc<dyn CompilationParserFactory>) {
        self.factories.push(factory);
    }

    /// Drop every factory contributed by `plugin_id`, returning how
    /// many were removed. Idempotent: a second call reports zero, which
    /// is what the teardown contract requires of a double-unload.
    pub fn unregister_plugin(&mut self, plugin_id: u64) -> usize {
        let before = self.factories.len();
        self.factories.retain(|f| f.plugin_id() != plugin_id);
        before - self.factories.len()
    }

    /// Mint one parser per factory, skipping any that failed to
    /// instantiate.
    pub fn create_all(&self) -> Vec<Box<dyn CompilationParser>> {
        self.factories.iter().filter_map(|f| f.create()).collect()
    }

    /// How many factories are registered.
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    /// Whether no factory is registered — the overwhelmingly common
    /// case, and the one a run checks to skip the snapshot entirely.
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }

    /// A fresh empty [`CompilationParserFactoriesHandle`].
    ///
    /// Exists so consumers — the plugin host's teardown tests, the
    /// loader's harness — do not each have to name `arc_swap` just to
    /// build the handle this crate defines.
    pub fn new_handle() -> CompilationParserFactoriesHandle {
        Arc::new(arc_swap::ArcSwap::from_pointee(Self::new()))
    }
}

/// The runtime-mutable handle, registered as a boot service under this
/// exact alias (the `ServiceRegistry` Arc/TypeId convention).
///
/// `lattice-plugin-loader` reaches it via
/// `service::<CompilationParserFactoriesHandle>()` and RCU-registers
/// each loaded `error-parser` plugin's factory.
pub type CompilationParserFactoriesHandle = Arc<arc_swap::ArcSwap<CompilationParserFactories>>;

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A stand-in for a plugin factory: counts how many parsers it was
    /// asked for, and each parser it mints counts its own feeds — so a
    /// test can prove the two readers got *different* instances.
    #[derive(Debug)]
    struct CountingFactory {
        plugin_id: u64,
        created: Arc<AtomicUsize>,
        fail: bool,
    }

    #[derive(Debug)]
    struct CountingParser {
        fed: usize,
    }

    impl CompilationParser for CountingParser {
        fn feed(&mut self, line: &str) -> Vec<ErrorEntry> {
            self.fed += 1;
            if line == "BOOM" {
                return vec![ErrorEntry {
                    path: std::path::PathBuf::from("x.rs"),
                    // The per-instance feed count, so two instances that
                    // saw different numbers of lines are distinguishable.
                    line: self.fed as u32,
                    col: 0,
                    severity: ErrorSeverity::Error,
                    message: "boom".to_string(),
                }];
            }
            Vec::new()
        }
    }

    impl CompilationParserFactory for CountingFactory {
        fn plugin_id(&self) -> u64 {
            self.plugin_id
        }
        fn create(&self) -> Option<Box<dyn CompilationParser>> {
            self.created.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return None;
            }
            Some(Box::new(CountingParser { fed: 0 }))
        }
    }

    fn factory(plugin_id: u64, created: Arc<AtomicUsize>) -> Arc<dyn CompilationParserFactory> {
        Arc::new(CountingFactory {
            plugin_id,
            created,
            fail: false,
        })
    }

    #[test]
    fn create_all_mints_one_parser_per_factory() {
        let created = Arc::new(AtomicUsize::new(0));
        let mut set = CompilationParserFactories::new();
        set.register(factory(1, created.clone()));
        set.register(factory(2, created.clone()));

        assert_eq!(set.create_all().len(), 2);
        assert_eq!(created.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn each_call_mints_independent_instances() {
        // The property the whole factory shape exists for: two readers
        // must not share pending state. Feed the two sets a different
        // number of lines and assert their entries disagree.
        let created = Arc::new(AtomicUsize::new(0));
        let mut set = CompilationParserFactories::new();
        set.register(factory(1, created.clone()));

        let mut out_side = set.create_all();
        let mut err_side = set.create_all();
        assert_eq!(created.load(Ordering::SeqCst), 2);

        out_side[0].feed("a");
        out_side[0].feed("b");
        let out_entry = out_side[0].feed("BOOM");
        let err_entry = err_side[0].feed("BOOM");

        assert_eq!(out_entry[0].line, 3, "stdout instance saw three lines");
        assert_eq!(
            err_entry[0].line, 1,
            "stderr instance saw one — pending state is not shared"
        );
    }

    #[test]
    fn a_factory_that_fails_to_instantiate_is_skipped() {
        // A broken plugin costs its own entries, never the build: the
        // other factory still yields a parser.
        let created = Arc::new(AtomicUsize::new(0));
        let mut set = CompilationParserFactories::new();
        set.register(Arc::new(CountingFactory {
            plugin_id: 1,
            created: created.clone(),
            fail: true,
        }));
        set.register(factory(2, created.clone()));

        assert_eq!(set.create_all().len(), 1);
        assert_eq!(created.load(Ordering::SeqCst), 2, "both were asked");
    }

    #[test]
    fn unregister_plugin_removes_only_that_plugins_factories() {
        let created = Arc::new(AtomicUsize::new(0));
        let mut set = CompilationParserFactories::new();
        set.register(factory(1, created.clone()));
        set.register(factory(1, created.clone()));
        set.register(factory(2, created.clone()));

        assert_eq!(set.unregister_plugin(1), 2);
        assert_eq!(set.len(), 1);
        // Idempotent — the teardown contract's double-unload case.
        assert_eq!(set.unregister_plugin(1), 0);
        assert_eq!(set.unregister_plugin(2), 1);
        assert!(set.is_empty());
    }
}
