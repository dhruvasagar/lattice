//! PH7.7c — the sync grammar trampoline, driven through a real guest.
//!
//! Instantiates the `grammar-guest` fixture (a `wasm32-wasip2` `grammar-plugin`
//! component) via [`PluginHost::instantiate_grammar_plugin`], registers its
//! contributions into a native `CommandRegistry`, and dispatches them through the
//! real `lattice_grammar` dispatcher — proving the whole seam end to end:
//!   - registration crosses (the guest's `register-grammar` → the `register-*`
//!     host funcs → 4 native specs: 3 motions + 1 text object),
//!   - provenance is host-stamped `SourceLayer::Plugin(id)` (a plugin cannot
//!     forge it),
//!   - a plugin motion dispatches through `execute_motion_only` — the **sync**
//!     trampoline fires into the guest and the `motion-result` crosses back
//!     (target = cursor line + count),
//!   - a guest-returned `err` degrades gracefully to `CommandError::Plugin` (a
//!     no-op), distinct from a host **trap**, which trips the quarantine + emits
//!     one Error trace and short-circuits later keystrokes (§8, PH7.12).
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::buffer::Buffer;
use lattice_core::buffers::BufferId;
use lattice_grammar::CancellationToken;
use lattice_grammar::command::{CommandInvocation, Count};
use lattice_grammar::dispatcher::execute_motion_only;
use lattice_grammar::error::CommandError;
use lattice_grammar::registry::{CommandRegistry, GrammarEnv};
use lattice_grammar::source::SourceLayer;
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{
    PluginHost, PluginManifest, PluginTracer, PluginTracerHandle, TraceLevel, TraceOutcome,
    TrustTier,
};
use lattice_protocol::position::Position;
use std::sync::Arc;
use tempfile::TempDir;

/// The fixture grammar component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("GRAMMAR_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Instantiate the fixture + register its grammar into a fresh registry; returns
/// `(registry, plugin_id)`.
fn load(dir: &TempDir) -> (CommandRegistry, u32) {
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds with tempdirs");
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile grammar fixture");
    let manifest = PluginManifest::new("grammar-fixture", Vec::new(), CapabilitySet::empty());
    let set = host
        .instantiate_grammar_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
            None,
            None,
        )
        .expect("instantiate + register-grammar");
    let plugin_id = set.plugin_id().0;
    // 3 motions (down-n, fails, traps) + 1 text object (to-cursor) + 4 actions
    // (read-at-cursor, AP.0.1; open-files-picker, PH7.4e; archive-to, XF.5;
    // archive-beside-me, OM.6b).
    assert_eq!(
        set.len(),
        8,
        "guest contributed down-n + to-cursor + fails + traps + read-at-cursor \
         + open-files-picker + archive-to + archive-beside-me"
    );

    let mut registry = CommandRegistry::new();
    set.register_all(&mut registry);
    (registry, plugin_id)
}

/// Like [`load`], but wires a `PluginTracer` (default `Info` gate) into the
/// grammar seam — the PO.3 traced-trampoline path. Returns `(registry,
/// plugin_id, tracer)` so a test can raise the plugin's level and inspect the
/// ring after dispatch.
fn load_traced(dir: &TempDir) -> (CommandRegistry, u32, PluginTracerHandle) {
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds with tempdirs");
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile grammar fixture");
    let manifest = PluginManifest::new("grammar-fixture", Vec::new(), CapabilitySet::empty());
    let tracer: PluginTracerHandle = Arc::new(PluginTracer::with_defaults());
    let set = host
        .instantiate_grammar_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
            Some(&tracer),
            None,
        )
        .expect("instantiate + register-grammar");
    let plugin_id = set.plugin_id().0;
    let mut registry = CommandRegistry::new();
    set.register_all(&mut registry);
    (registry, plugin_id, tracer)
}

/// Dispatch `down-n` from line 1 with count 3 (→ line 4) against `registry`.
fn dispatch_down_n(registry: &CommandRegistry) {
    let motion_id = registry.id_by_name("down-n").unwrap();
    let buffer = Buffer::from_text("l0\nl1\nl2\nl3\nl4\nl5\n");
    let cancel = CancellationToken::never();
    let target = execute_motion_only(
        registry,
        &buffer,
        BufferId(1),
        Position { line: 1, byte: 0 },
        CommandInvocation::of(motion_id).with_count(Count(3)),
        &cancel,
        GrammarEnv::default(),
    )
    .expect("plugin motion dispatches");
    assert_eq!(target, Position { line: 4, byte: 0 });
}

#[test]
fn traced_motion_at_the_default_gate_records_nothing() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, plugin_id, tracer) = load_traced(&dir);

    // The hot-path off state (design §4): at the default `Info` gate a *successful*
    // grammar call emits nothing — the trampoline's only cost is the gate load.
    dispatch_down_n(&registry);
    assert!(
        tracer.snapshot_plugin(plugin_id).is_empty(),
        "a successful motion at the default gate leaves the trace ring empty"
    );
}

#[test]
fn traced_motion_raised_to_debug_records_a_boundary_trace() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, plugin_id, tracer) = load_traced(&dir);

    // Raise this one plugin live — republished to the already-handed-out hot gate.
    tracer.set_plugin_level(plugin_id, TraceLevel::Debug);
    dispatch_down_n(&registry);

    let recs = tracer.snapshot_plugin(plugin_id);
    assert_eq!(recs.len(), 1, "the raised gate captures the guest call");
    assert_eq!(recs[0].call, "apply-motion");
    assert_eq!(recs[0].level, TraceLevel::Debug);
    assert!(matches!(recs[0].outcome, TraceOutcome::Ok { .. }));
}

#[test]
fn traced_guest_err_records_a_warn_even_at_the_default_gate() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, plugin_id, tracer) = load_traced(&dir);
    let fails_id = registry.id_by_name("fails").unwrap();

    // A guest `err` is user-actionable and rare → recorded at `Warn`, which the
    // default `Info` gate keeps (the sync seam sees the guest's inner err directly).
    let buffer = Buffer::from_text("l0\nl1\n");
    let cancel = CancellationToken::never();
    let _ = execute_motion_only(
        &registry,
        &buffer,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(fails_id),
        &cancel,
        GrammarEnv::default(),
    )
    .expect_err("the guest err is a typed CommandError");

    let recs = tracer.snapshot_plugin(plugin_id);
    assert_eq!(
        recs.len(),
        1,
        "the guest err is captured at the default gate"
    );
    assert_eq!(recs[0].level, TraceLevel::Warn);
}

#[test]
fn plugin_grammar_registers_with_host_stamped_plugin_provenance() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built (add the wasm32-wasip2 target)");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, plugin_id) = load(&dir);

    for name in ["down-n", "to-cursor", "fails"] {
        let id = registry
            .id_by_name(name)
            .unwrap_or_else(|| panic!("{name} registered"));
        assert_eq!(
            registry.lookup(id).unwrap().source.layer,
            SourceLayer::Plugin(plugin_id),
            "{name} stamped Plugin provenance (unforgeable, host-issued)"
        );
    }
}

#[test]
fn plugin_motion_dispatches_through_the_sync_trampoline() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, _) = load(&dir);
    let motion_id = registry.id_by_name("down-n").unwrap();

    let buffer = Buffer::from_text("l0\nl1\nl2\nl3\nl4\nl5\n");
    let cancel = CancellationToken::never();
    // `down-n` returns cursor.line + count; from line 1 with count 3 → line 4.
    let target = execute_motion_only(
        &registry,
        &buffer,
        BufferId(1),
        Position { line: 1, byte: 0 },
        CommandInvocation::of(motion_id).with_count(Count(3)),
        &cancel,
        GrammarEnv::default(),
    )
    .expect("plugin motion dispatches through the sync trampoline");
    assert_eq!(target, Position { line: 4, byte: 0 });
}

#[test]
fn plugin_action_reads_buffer_text_at_the_cursor_via_the_document_handle() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, _) = load(&dir);
    let action_id = registry.id_by_name("read-at-cursor").unwrap();

    // AP.0.1: the guest reads the byte at `ctx.cursor` through the borrowed
    // `document` handle. Cursor at line 1, byte 0 → 'w' of "world".
    let mut document = lattice_core::Document::from_text("hello\nworld\n");
    let cancel = CancellationToken::never();
    let effect = lattice_grammar::dispatcher::execute(
        &registry,
        &mut document,
        BufferId(1),
        Position { line: 1, byte: 0 },
        CommandInvocation::of(action_id),
        &cancel,
    )
    .expect("plugin action dispatches through the sync trampoline");

    // The guest echoes "<char>@<line>:<byte>" — both the document read AND the
    // cursor crossed the boundary. `Many([x])` normalises to `x`, so the single
    // effect arrives as `Echo` directly.
    match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => {
            assert_eq!(
                text, "w@1:0",
                "the guest sliced 'w' at the cursor and echoed the cursor coords"
            );
        }
        other => panic!("expected an Echo effect from the plugin action, got {other:?}"),
    }
}

// ─────────────────────────────────────────────────────────────────
// XF.5 — the cross-file write, and its capability gate, through a real guest
// ─────────────────────────────────────────────────────────────────

/// Load the fixture with an explicit capability set, so the granted and denied
/// cases differ **only** by the manifest.
///
/// That is the whole point: the guest is byte-identical between the two tests,
/// so a denial is demonstrably about the capability rather than about the
/// plugin having been written differently.
fn load_with_caps(dir: &TempDir, caps: Vec<lattice_plugin_host::Capability>) -> CommandRegistry {
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds with tempdirs");
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile grammar fixture");
    let manifest = PluginManifest::new("grammar-fixture", caps, CapabilitySet::empty());
    let set = host
        .instantiate_grammar_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
            None,
            None,
        )
        .expect("instantiate + register-grammar");
    let mut registry = CommandRegistry::new();
    set.register_all(&mut registry);
    registry
}

/// Fire `archive-to <path>` and return whatever effect came back.
fn run_archive_to(registry: &CommandRegistry, path: &std::path::Path) -> lattice_grammar::Effect {
    let action_id = registry.id_by_name("archive-to").unwrap();
    let mut document = lattice_core::Document::from_text("* Keep\n");
    let cancel = CancellationToken::never();
    lattice_grammar::dispatcher::execute(
        registry,
        &mut document,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(action_id)
            .with_args(lattice_grammar::Args::String(path.display().to_string())),
        &cancel,
    )
    .expect("the action dispatches")
}

/// OM.6b: fire `archive-beside-me` in a document backed by `path`, and return
/// whatever effect came back. Nothing about the TARGET is supplied here — the
/// guest derives it from `document.path()`, which is exactly the capability
/// under test.
fn run_archive_beside_me(
    registry: &CommandRegistry,
    path: Option<&std::path::Path>,
) -> lattice_grammar::Effect {
    let action_id = registry.id_by_name("archive-beside-me").unwrap();
    let builder = lattice_core::DocumentBuilder::default().with_text("* Keep\n");
    let mut document = match path {
        Some(p) => builder.with_path(p).build(),
        None => builder.build(),
    };
    let cancel = CancellationToken::never();
    lattice_grammar::dispatcher::execute(
        registry,
        &mut document,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(action_id),
        &cancel,
    )
    .expect("the action dispatches")
}

/// **OM.6b, the whole shape.** A guest asks which file it is editing, derives a
/// sibling path from the answer, and writes there. Before `document.path()` a
/// grammar action could read its buffer's text but not name its file, so
/// `org-archive-subtree` — whose target is `<file>_archive` by definition —
/// was inexpressible no matter what the effect vocabulary allowed.
///
/// The assertion is on the path the GUEST computed, not one the test passed in.
#[test]
fn a_guest_can_name_a_file_beside_the_one_it_is_editing() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let notes = dir.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    let mine = notes.join("today.org");

    let registry = load_with_caps(
        &dir,
        vec![lattice_plugin_host::Capability::FsWrite(notes.clone())],
    );

    match run_archive_beside_me(&registry, Some(&mine)) {
        lattice_grammar::Effect::WriteToFile { path, text, .. } => {
            assert_eq!(path, notes.join("today.org_archive"));
            assert_eq!(text, "* Archived beside me\n");
        }
        other => panic!("expected a WriteToFile, got {other:?}"),
    }
}

/// A buffer with no file answers `none` rather than inventing a path. The
/// guest is then the one that decides what to do about it — here, a typed
/// `err` — because "archive this scratch buffer" has no correct target and
/// guessing one would write somewhere the user never named.
#[test]
fn a_buffer_with_no_file_reports_no_path() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let registry = load_with_caps(
        &dir,
        vec![lattice_plugin_host::Capability::FsWrite(
            dir.path().to_path_buf(),
        )],
    );

    let action_id = registry.id_by_name("archive-beside-me").unwrap();
    let mut document = lattice_core::Document::from_text("* Keep\n");
    let cancel = CancellationToken::never();
    let err = lattice_grammar::dispatcher::execute(
        &registry,
        &mut document,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(action_id),
        &cancel,
    )
    .expect_err("a pathless buffer has no archive target");
    assert!(
        format!("{err:?}").contains("no file"),
        "the guest's own message survives: {err:?}"
    );
}

/// A plugin granted `fs:write` over a directory gets its `WriteToFile`
/// through, payload intact.
#[test]
fn a_granted_plugin_can_return_a_cross_file_write() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let notes = dir.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    let target = notes.join("archive.org");

    let registry = load_with_caps(
        &dir,
        vec![lattice_plugin_host::Capability::FsWrite(notes.clone())],
    );

    match run_archive_to(&registry, &target) {
        lattice_grammar::Effect::WriteToFile {
            path, anchor, text, ..
        } => {
            assert_eq!(path, target);
            assert_eq!(anchor, lattice_grammar::FileAnchor::End);
            assert_eq!(text, "* Archived by the fixture\n");
        }
        other => panic!("expected a WriteToFile, got {other:?}"),
    }
}

/// **The gate, wired.** The same guest, the same action, the same path — and
/// no `fs:write` grant. The effect is replaced with an `Echo` before it can
/// reach the editor.
///
/// The authorizer's own tests prove it decides correctly; this proves it is
/// actually *consulted*, which is the half a unit test cannot show and the
/// half whose absence would be an unchecked cross-file write reachable from
/// any plugin.
#[test]
fn a_plugin_without_fs_write_has_its_cross_file_write_refused() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let notes = dir.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    let target = notes.join("archive.org");

    // No capabilities at all — the only difference from the test above.
    let registry = load_with_caps(&dir, Vec::new());

    match run_archive_to(&registry, &target) {
        lattice_grammar::Effect::Echo { text, .. } => {
            assert!(
                text.contains("denied"),
                "the user is told rather than left wondering why a key did \
                 nothing: {text}"
            );
            assert!(text.contains("grammar-fixture"), "names the plugin: {text}");
        }
        lattice_grammar::Effect::WriteToFile { .. } => {
            panic!(
                "an ungranted plugin's cross-file write reached the editor — \
                 the boundary gate is not wired"
            )
        }
        other => panic!("expected an Echo, got {other:?}"),
    }
}

/// A `fs:read` grant over the same directory is still not permission to write
/// into it. The distinction matters because `host-services`' walk accepts
/// either, so "has an fs grant" is not the same question as "may write".
#[test]
fn a_read_only_grant_does_not_authorise_a_cross_file_write() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let notes = dir.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();

    let registry = load_with_caps(
        &dir,
        vec![lattice_plugin_host::Capability::FsRead(notes.clone())],
    );

    assert!(
        matches!(
            run_archive_to(&registry, &notes.join("archive.org")),
            lattice_grammar::Effect::Echo { .. }
        ),
        "read is not write"
    );
}

/// A granted plugin still cannot reach outside its prefix by spelling the path
/// with `..`. The escape test, through the real boundary rather than against
/// the authorizer directly.
#[test]
fn a_granted_plugin_cannot_escape_its_prefix_with_dotdot() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let notes = dir.path().join("notes");
    let secret = dir.path().join("secret");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::create_dir_all(&secret).unwrap();

    let registry = load_with_caps(
        &dir,
        vec![lattice_plugin_host::Capability::FsWrite(notes.clone())],
    );

    let escaping = notes.join("..").join("secret").join("stolen.org");
    assert!(
        matches!(
            run_archive_to(&registry, &escaping),
            lattice_grammar::Effect::Echo { .. }
        ),
        "`<granted>/../secret/…` resolves outside the grant and must be refused"
    );
}

#[test]
fn plugin_action_out_of_range_read_degrades_to_a_graceful_no_op() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, _) = load(&dir);
    let action_id = registry.id_by_name("read-at-cursor").unwrap();

    // Cursor past EOF: the host `get-text-range` returns a typed `err`, which the
    // guest propagates via `?` → the trampoline maps it to CommandError::Plugin
    // (a graceful no-op), NOT a trap or a panic (§8).
    let mut document = lattice_core::Document::from_text("hi\n");
    let cancel = CancellationToken::never();
    let err = lattice_grammar::dispatcher::execute(
        &registry,
        &mut document,
        BufferId(1),
        Position { line: 9, byte: 0 },
        CommandInvocation::of(action_id),
        &cancel,
    )
    .expect_err("an out-of-range read is a typed CommandError, not a success");
    assert!(
        matches!(err, CommandError::Plugin(_)),
        "out-of-range read maps to CommandError::Plugin, got {err:?}"
    );
}

#[test]
fn plugin_motion_guest_err_degrades_to_a_graceful_no_op() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, _) = load(&dir);
    let fails_id = registry.id_by_name("fails").unwrap();

    let buffer = Buffer::from_text("l0\nl1\n");
    let cancel = CancellationToken::never();
    // The guest has no `apply-motion` arm for this callback → a WIT `err`, which
    // the trampoline maps to `CommandError::Plugin` (the dispatcher commits no
    // effect — a graceful no-op, §8), NOT a panic or a host trap.
    let err = execute_motion_only(
        &registry,
        &buffer,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(fails_id),
        &cancel,
        GrammarEnv::default(),
    )
    .expect_err("a guest err is a typed CommandError, not a success");
    assert!(
        matches!(err, CommandError::Plugin(_)),
        "guest err maps to CommandError::Plugin, got {err:?}"
    );
}

#[test]
fn a_trapping_motion_quarantines_and_short_circuits() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, plugin_id, tracer) = load_traced(&dir);
    let traps_id = registry.id_by_name("traps").unwrap();
    let buffer = Buffer::from_text("l0\nl1\nl2\n");
    let cancel = CancellationToken::never();

    // First dispatch: the guest panics → a host trap. The trampoline classifies
    // it, trips the quarantine, and returns CommandError::Plugin — a graceful
    // no-op on the keystroke path, never a panic (§8).
    let err1 = execute_motion_only(
        &registry,
        &buffer,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(traps_id),
        &cancel,
        GrammarEnv::default(),
    )
    .expect_err("a trapping motion is a typed CommandError, not a success/panic");
    assert!(matches!(err1, CommandError::Plugin(_)), "got {err1:?}");

    // Second dispatch: the quarantine short-circuits at the top of run_callback,
    // BEFORE re-entering the dead Store — still a typed error, never a re-trap.
    let err2 = execute_motion_only(
        &registry,
        &buffer,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(traps_id),
        &cancel,
        GrammarEnv::default(),
    )
    .expect_err("a quarantined plugin short-circuits to a no-op");
    assert!(matches!(err2, CommandError::Plugin(_)), "got {err2:?}");

    // Exactly ONE Error/Trap trace record: the trip emits once; the re-trip
    // returns before the emit, so the second dispatch adds nothing (no
    // per-keystroke Error flood on a dead plugin).
    let traps: Vec<_> = tracer
        .snapshot_plugin(plugin_id)
        .into_iter()
        .filter(|r| matches!(r.outcome, TraceOutcome::Trap { .. }))
        .collect();
    assert_eq!(
        traps.len(),
        1,
        "one trap record from the trip, none from the re-trip"
    );
    assert_eq!(traps[0].level, TraceLevel::Error);
}

/// PH7.4e — "utilize an existing picker": a plugin opens a picker source
/// it does not own.
///
/// The conversion layer was already unit-tested in both directions
/// (`boundary_effect`'s round-trip), and `Effect::OpenPicker` was already
/// expressible. What was unproven is the thing the slice is named for:
/// that a **real guest** can produce it and the host receives it intact.
///
/// The args are non-empty and ordered on purpose — an assertion that
/// merely saw `OpenPicker` would pass on a payload that arrived empty or
/// reversed.
#[test]
fn a_plugin_can_open_a_picker_source_it_does_not_own() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, _) = load(&dir);
    let action_id = registry
        .id_by_name("open-files-picker")
        .expect("the guest registered the action");

    let mut document = lattice_core::Document::from_text("hello\n");
    let cancel = CancellationToken::never();
    let effect = lattice_grammar::dispatcher::execute(
        &registry,
        &mut document,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(action_id),
        &cancel,
    )
    .expect("the plugin action dispatches through the sync trampoline");

    match effect {
        lattice_grammar::effect::Effect::OpenPicker { source, args } => {
            assert_eq!(source, "files", "the guest names a HOST-owned source");
            assert_eq!(
                args,
                vec!["src".to_string(), "*.rs".to_string()],
                "the payload crossed intact and in order"
            );
        }
        other => panic!("expected OpenPicker from the plugin action, got {other:?}"),
    }
}
