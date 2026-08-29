#![allow(clippy::unwrap_used, clippy::panic)]
//! OR.1 criterion bench for the plugin byte store.
//!
//! These three numbers bound OR.4 (the roam indexer) and OR.6 (find-node), so
//! they are measured before either is written rather than explained afterwards:
//!
//!   * `put_blob` / `get_blob` — a 90 KB value. That is the size roam's `nodes`
//!     key reaches on the reference corpus (585 nodes), and `nodes` is
//!     rewritten whenever anything in the corpus changes and read once per
//!     picker open. If `get` on it were expensive, §4.2's "one `get`, not 585"
//!     would be the wrong trade.
//!   * `put_record` / `get_record` — a 200-byte value, the size of one `n/<id>`
//!     record. `<CR>` on an `[[id:…]]` link does exactly one of these, on the
//!     keystroke path, so this is the number that has to be small.
//!   * `keys_prefix` — a prefix scan over 1000 entries, the shape the indexer
//!     uses to enumerate `f/<path>` rows when it decides what to retract.
//!
//! **What this measures and what it does not.** This is the store itself, not
//! the WASM boundary — the guest→host crossing is `boundary.rs`'s subject and is
//! measured there. Splitting them is deliberate: a regression in one should not
//! be readable as a regression in the other.
//!
//! Flushing is included where it naturally falls (the store flushes every 64
//! mutations), because that is what a caller actually pays. A bench that flushed
//! never would report a store nobody has.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_plugin_host::plugin_store::PluginStore;

/// The size roam's `nodes` blob reaches on the 585-node reference corpus.
const BLOB_BYTES: usize = 90 * 1024;
/// One `n/<id>` record — an id, a title, a handful of tags, a path and a line.
const RECORD_BYTES: usize = 200;

fn store_in(dir: &std::path::Path) -> PluginStore {
    PluginStore::open(dir)
}

fn bench_store(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();

    let blob = vec![0xAB_u8; BLOB_BYTES];
    let record = vec![0xCD_u8; RECORD_BYTES];

    c.bench_function("plugin_store/put_blob_90k", |b| {
        let mut store = store_in(dir.path());
        b.iter(|| {
            store
                .put(black_box("nodes"), black_box(blob.clone()))
                .unwrap()
        });
    });

    c.bench_function("plugin_store/get_blob_90k", |b| {
        let mut store = store_in(dir.path());
        store.put("nodes", blob.clone()).unwrap();
        b.iter(|| black_box(store.get(black_box("nodes"))));
    });

    c.bench_function("plugin_store/put_record_200b", |b| {
        let mut store = store_in(dir.path());
        b.iter(|| {
            store
                .put(black_box("n/E4F1"), black_box(record.clone()))
                .unwrap()
        });
    });

    // The keystroke-path number: `<CR>` on an `[[id:…]]` link is one of these,
    // against a store already holding a corpus-sized set of records.
    c.bench_function("plugin_store/get_record_200b_of_585", |b| {
        let mut store = store_in(dir.path());
        for i in 0..585 {
            store.put(&format!("n/{i:08X}"), record.clone()).unwrap();
        }
        b.iter(|| black_box(store.get(black_box("n/00000123"))));
    });

    c.bench_function("plugin_store/keys_prefix_of_1000", |b| {
        let mut store = store_in(dir.path());
        for i in 0..500 {
            store.put(&format!("n/{i:08X}"), vec![1]).unwrap();
        }
        for i in 0..500 {
            store.put(&format!("f/{i:08X}"), vec![1]).unwrap();
        }
        b.iter(|| black_box(store.keys(black_box("f/"))));
    });
}

criterion_group!(benches, bench_store);
criterion_main!(benches);
