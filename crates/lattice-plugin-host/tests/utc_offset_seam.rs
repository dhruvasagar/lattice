//! OC.4 end-to-end — the host answers the one clock question a guest cannot.
//!
//! A component's `SystemTime::now()` resolves through `wasi:clocks`, which is
//! UTC, and the host builds each plugin's `WasiCtxBuilder` with **no environment
//! inheritance** — so there is no `TZ` either. A guest therefore has a correct
//! instant and no way to render it as the wall-clock time the user sees. Org
//! writes `CLOCK: [2026-08-28 Fri 16:02]` in local time by definition, so
//! without this seam every clock line, every `%U` / `%T` / `%t` capture stamp
//! and the agenda's "today" anchor is wrong by the user's offset — and near
//! midnight, wrong by a day.
//!
//! **On the honesty of these assertions.** The strong one — guest value equals
//! the host's own `chrono::Local` answer — is degenerate on a machine configured
//! to UTC, where both sides are `0` and a stubbed seam would pass. It is not
//! degenerate anywhere else, and the shape-assertions below (a real offset is
//! whole minutes and inside the range zones actually occupy) hold everywhere.
//! Pretending otherwise by pinning `TZ` would mean mutating process-global env
//! to test a function whose entire job is to read process-global env.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::{CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::position::Position;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// The widest range any real zone occupies: UTC-12 (Baker Island) to UTC+14
/// (Line Islands). Anything outside is not an offset, it is a bug.
const MIN_OFFSET: i32 = -12 * 3600;
const MAX_OFFSET: i32 = 14 * 3600;

#[test]
fn the_host_answers_the_guests_utc_offset() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    }
    let path = guest_wasm().unwrap();
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    // NO capabilities: the seam is deliberately ungated, because a plugin with
    // no filesystem grant still has to render timestamps in the right zone.
    let manifest = PluginManifest::new("multiseam", Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let grammar_set = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect("grammar drain instantiates");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    let id = commands.id_by_name("multiseam-utc-offset").unwrap();

    let mut document = lattice_core::Document::from_text("x\n");
    let cancel = CancellationToken::never();
    let effect = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(id),
        &cancel,
        GrammarEnv::default(),
    )
    .expect("the offset action dispatches through the sync trampoline");

    let text = match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => text,
        other => panic!("expected an Echo carrying the offset, got {other:?}"),
    };
    let (offset, guest_utc) = text.split_once(':').expect("<offset>:<guest-utc-secs>");
    let offset: i32 = offset.parse().expect("the offset crossed as a number");
    let guest_utc: i64 = guest_utc.parse().expect("the guest's clock crossed");

    // The guest's own clock still works and is still UTC — this seam adds a
    // second, independent source rather than replacing the first.
    let host_utc = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(
        (host_utc - guest_utc).abs() < 60,
        "the guest's wasi:clocks reading should track the host's UTC clock"
    );

    // A real offset, not a placeholder.
    assert!(
        (MIN_OFFSET..=MAX_OFFSET).contains(&offset),
        "{offset} is outside every zone that exists"
    );
    assert_eq!(
        offset % 60,
        0,
        "no zone has a sub-minute offset; {offset} is not a wall-clock offset"
    );

    // …and it is the HOST's offset, which is the part that cannot be faked
    // anywhere but a UTC-configured machine (see the module note).
    assert_eq!(
        offset,
        chrono::Local::now().offset().local_minus_utc(),
        "the value crossed from the host, not from the guest's UTC-only clock"
    );
}
