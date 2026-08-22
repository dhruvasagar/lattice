//! CM.6 — the plugin-contributed compilation-parser seam, through a real guest.
//!
//! The fixture recognises a two-line format no native parser knows:
//!
//! ```text
//! ERR something broke
//!   at src/thing.q:12:5
//! ```
//!
//! Two lines on purpose. A single-line format would be satisfied by a regex
//! and would not exercise what the seam has to support: pending state carried
//! across `feed` calls, and `reset` dropping it.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_protocol::error_list::ErrorSeverity;
use tempfile::TempDir;

const PLUGIN_ID: &str = "error-parser-fixture";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("ERROR_PARSER_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

fn parser(dir: &TempDir) -> Option<lattice_plugin_host::error_parser_host::WasmErrorParser> {
    let wasm = guest_wasm()?;
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile error-parser fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());
    Some(
        host.spawn_error_parser(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::grammar(),
        )
        .expect("spawn error parser"),
    )
}

#[test]
fn a_plugin_parses_a_format_no_native_parser_knows() {
    let dir = TempDir::new().unwrap();
    let Some(mut p) = parser(&dir) else {
        eprintln!("SKIP: error-parser fixture guest not built");
        return;
    };

    // The header completes nothing — it primes the guest's pending state.
    assert!(
        p.feed("ERR something broke").is_empty(),
        "a header alone is not a diagnostic"
    );

    // The locator completes it.
    let entries = p.feed("  at src/thing.q:12:5");
    assert_eq!(entries.len(), 1, "got {entries:?}");
    let e = &entries[0];
    assert_eq!(e.path, std::path::PathBuf::from("src/thing.q"));
    assert_eq!(e.line, 11, "the guest converts 1-based → 0-based");
    assert_eq!(e.col, 4);
    assert_eq!(e.severity, ErrorSeverity::Error);
    assert_eq!(e.message, "something broke");
}

#[test]
fn severity_crosses_the_boundary() {
    let dir = TempDir::new().unwrap();
    let Some(mut p) = parser(&dir) else {
        eprintln!("SKIP: error-parser fixture guest not built");
        return;
    };
    p.feed("WARN mind the gap");
    let entries = p.feed("  at a/b.q:1:1");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].severity, ErrorSeverity::Warning);
    assert_eq!((entries[0].line, entries[0].col), (0, 0));
}

/// `reset` drops pending state, which is what stops a build interrupted
/// mid-diagnostic from leaking a half-parsed entry into the next run.
#[test]
fn reset_drops_pending_state_between_runs() {
    let dir = TempDir::new().unwrap();
    let Some(mut p) = parser(&dir) else {
        eprintln!("SKIP: error-parser fixture guest not built");
        return;
    };
    p.feed("ERR interrupted");
    p.reset();
    assert!(
        p.feed("  at leaked.q:1:1").is_empty(),
        "a locator with no header after a reset must complete nothing"
    );
}

/// Lines the plugin does not recognise are simply not its business — it
/// returns nothing and the native parsers still see the same line.
#[test]
fn unrecognised_lines_yield_nothing() {
    let dir = TempDir::new().unwrap();
    let Some(mut p) = parser(&dir) else {
        eprintln!("SKIP: error-parser fixture guest not built");
        return;
    };
    for line in [
        "   Compiling foo v0.1.0",
        "warning: unused variable `x`",
        "",
        "at malformed",
    ] {
        assert!(
            p.feed(line).is_empty(),
            "line {line:?} should match nothing"
        );
    }
}

/// Two instances keep separate pending state.
///
/// The guest holds it in a `thread_local!`, so this is really asserting that
/// the host gives each parser its own `Store` — if they shared one, one
/// build's half-diagnostic would complete against another's locator.
#[test]
fn two_parsers_do_not_share_pending_state() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let (Some(mut a), Some(mut b)) = (parser(&dir_a), parser(&dir_b)) else {
        eprintln!("SKIP: error-parser fixture guest not built");
        return;
    };
    a.feed("ERR belongs to a");
    // `b` never saw a header, so a locator completes nothing for it.
    assert!(
        b.feed("  at b.q:1:1").is_empty(),
        "state leaked between parser instances"
    );
    // …and `a`'s pending diagnostic is still its own.
    let entries = a.feed("  at a.q:2:2");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message, "belongs to a");
}

/// CM.6b — the factory, over the same real guest.
///
/// One compiled component, two parsers, from the two calls a compilation
/// run's stdout and stderr readers make. This is the assertion the factory
/// shape exists for; `two_parsers_do_not_share_pending_state` above proves
/// the same property for two separately *spawned* parsers, and this proves
/// the factory does not quietly hand back the same one twice.
#[test]
fn a_factory_mints_independent_parsers_from_one_component() {
    use lattice_compilation::{CompilationParser, CompilationParserFactory};

    let dir = TempDir::new().unwrap();
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: error-parser fixture guest not built");
        return;
    };
    let host = std::sync::Arc::new(
        PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
            .expect("host builds"),
    );
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile error-parser fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());
    let factory = lattice_plugin_host::error_parser_host::WasmErrorParserFactory::new(
        host,
        component,
        manifest,
        TrustTier::Bundled,
        PluginBudget::grammar(),
        7,
    );

    assert_eq!(factory.plugin_id(), 7, "provenance is the teardown token");

    let mut out_side: Box<dyn CompilationParser> =
        factory.create().expect("stdout reader's parser");
    let mut err_side: Box<dyn CompilationParser> =
        factory.create().expect("stderr reader's parser");

    // The stdout reader primes a diagnostic; the stderr reader must not be
    // able to complete it.
    out_side.feed("ERR on stdout");
    assert!(
        err_side.feed("  at stderr.q:1:1").is_empty(),
        "the two readers' parsers share a Store"
    );
    let entries = out_side.feed("  at stdout.q:2:2");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message, "on stdout");
}

/// A parser is fed EVERY captured line of a build. Fuel is a per-call budget,
/// so it must be re-armed per `feed` — arming once at instantiate makes the
/// parser work for the first ~1000 lines of a build and then poison itself,
/// silently dropping every diagnostic after that point.
///
/// A real `cargo build` emits far more than 1000 lines, so this is the common
/// case rather than an edge one. Found while fixing the same bug in the CR.4
/// dashboard seam, which shares the arm-once shape.
#[test]
fn a_parser_survives_a_long_build() {
    let dir = TempDir::new().unwrap();
    let Some(mut p) = parser(&dir) else {
        eprintln!("SKIP: error-parser fixture guest not built");
        return;
    };

    // Noise lines, as a long build is mostly noise.
    for _ in 0..150_000 {
        let _ = p.feed("   Compiling something v0.1.0");
    }

    // The parser must still recognise its format after all that.
    assert!(p.feed("ERR something broke").is_empty());
    let entries = p.feed("  at src/thing.q:12:5");
    assert_eq!(
        entries.len(),
        1,
        "the parser stopped recognising diagnostics partway through the build \
         — fuel is not being re-armed per feed"
    );
}
