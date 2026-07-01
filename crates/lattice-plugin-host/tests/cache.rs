//! PH7.1b: the on-disk module cache. A component's AOT compile is cached, so a
//! second launch reuses the cached module instead of recompiling. Also proves
//! the cache discriminates distinct components (no key collision) and that
//! loading is lazy (compiling never runs guest code).

use lattice_plugin_host::PluginHost;
use tempfile::TempDir;

const NOOP_WAT: &str = include_str!("fixtures/noop.wat");
const BUSY_WAT: &str = include_str!("fixtures/busy.wat");
const SPIN_WAT: &str = include_str!("fixtures/spin.wat");

fn bytes(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).expect("fixture WAT assembles to component bytes")
}

#[test]
fn second_launch_reuses_the_cached_module() {
    // A shared cache dir standing in for `<XDG_CACHE_HOME>/lattice/plugin-cache`,
    // isolated per test so the hit/miss counts are hermetic.
    let dir = TempDir::new().expect("tempdir");
    let noop = bytes(NOOP_WAT);

    // First launch: cold. Compiling the component is a cache miss; the artifact
    // is written to the cache dir.
    let host1 = PluginHost::with_cache_dir(dir.path()).expect("host1 builds");
    let _c1 = host1.compile(&noop).expect("first compile");
    assert_eq!(host1.cache_misses(), 1, "first compile should be a miss");
    assert_eq!(host1.cache_hits(), 0);

    // Second launch: a fresh host over the same cache dir. Compiling the same
    // component reuses the cached module — a hit, no recompile.
    let host2 = PluginHost::with_cache_dir(dir.path()).expect("host2 builds");
    let _c2 = host2.compile(&noop).expect("second compile");
    assert_eq!(
        host2.cache_hits(),
        1,
        "second launch should reuse the cached module",
    );
    assert_eq!(
        host2.cache_misses(),
        0,
        "second launch should not recompile"
    );
}

#[test]
fn distinct_components_do_not_collide_in_the_cache() {
    let dir = TempDir::new().expect("tempdir");
    let host = PluginHost::with_cache_dir(dir.path()).expect("host builds");

    // Two different components → two misses (the key discriminates content).
    host.compile(&bytes(NOOP_WAT)).expect("noop compiles");
    host.compile(&bytes(BUSY_WAT)).expect("busy compiles");
    assert_eq!(host.cache_misses(), 2);
    assert_eq!(host.cache_hits(), 0);

    // Recompiling the first component now hits.
    host.compile(&bytes(NOOP_WAT)).expect("noop recompiles");
    assert_eq!(host.cache_hits(), 1);
    assert_eq!(host.cache_misses(), 2);
}

#[test]
fn compiling_does_not_instantiate_or_run_guest_code() {
    // Lazy instantiation: loading a component must not instantiate it or run
    // `activate`. If it did, this infinite-loop `spin` component would hang the
    // test. Reaching the end proves load ≠ instantiate ≠ run.
    let dir = TempDir::new().expect("tempdir");
    let host = PluginHost::with_cache_dir(dir.path()).expect("host builds");
    let _component = host
        .compile(&bytes(SPIN_WAT))
        .expect("spin compiles without running");
}
