//! OM.A1 — the agenda-row producer seam, driven through a real guest.
//!
//! Instantiates the `agenda-guest` fixture via
//! [`PluginHost::spawn_agenda_source`], drives its `extensions` / `begin` /
//! `scan` exports through the [`WasmScannedExcerptSource`] adapter + `AgendaActor`
//! bridge, and asserts the native result — the whole seam end to end:
//!
//!   - the declared extensions cross back and are normalised (the fixture
//!     deliberately says `".ORG"`), which is what makes `claims()` work
//!     without the host knowing what an org file is;
//!   - `scan` crosses text in and `list<entry>` back;
//!   - `begin` really resets the guest's per-scan state — the fixture reports
//!     its file counter in each label, so a second scan that did NOT reset
//!     would be visible;
//!   - a malformed file returns a typed `err` the adapter surfaces, and the
//!     source is still usable afterwards (an err is not a quarantine).
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_mode::{CapabilitySet, ScannedExcerptSource};
use lattice_plugin_host::{
    PluginBudget, PluginHost, PluginManifest, TrustTier, WasmScannedExcerptSource,
    normalise_extensions,
};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("AGENDA_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Spawn the fixture and build the adapter the way the loader does — ask for
/// the extensions once, normalise, then construct.
async fn source(host: &PluginHost) -> WasmScannedExcerptSource {
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile agenda fixture");
    let manifest = PluginManifest::new("agenda-fixture", Vec::new(), CapabilitySet::empty());
    let (client, actor) = host
        .spawn_agenda_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
            None,
        )
        .await
        .expect("spawn agenda source");
    tokio::spawn(actor.run());
    let declared = client.extensions().await.expect("extensions cross back");
    let view_mode = client.view_mode().await.expect("view-mode crosses back");
    WasmScannedExcerptSource::new(client, normalise_extensions(declared), view_mode)
}

fn org(rows: &[i64]) -> String {
    let mut out = String::from("#+TITLE: notes\n");
    for r in rows {
        out.push_str(&format!("* TODO {r}\n"));
    }
    out
}

/// The declaration that keeps `.org` out of the host: the guest names the
/// extension, the host normalises it, and `claims()` answers from that alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_guest_declares_which_files_it_is_offered() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: agenda fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    assert_eq!(
        src.extensions(),
        ["org".to_string()],
        "the fixture declares `.ORG`; the host stores it normalised"
    );
    assert!(src.claims(Path::new("/p/notes.org")));
    assert!(src.claims(Path::new("/p/NOTES.ORG")));
    assert!(!src.claims(Path::new("/p/main.rs")));

    // OM.A3: the source also names the minor it wants on the agenda view, so
    // it can act on its own rows there. The host activates the name and never
    // learns what the chords do.
    assert_eq!(src.view_mode(), Some("agenda-guest-mode"));
}

/// Text crosses in, rows cross back, and the guest's own `sort_key` survives —
/// which is the datum the whole cross-file ordering is built on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_crosses_text_in_and_rows_back() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    src.begin(&[]).await.expect("begin");
    let rows = src
        .scan(PathBuf::from("/p/a.org"), org(&[30, 10]))
        .await
        .expect("scan");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].line, 1, "0-based, past the `#+TITLE:` line");
    assert_eq!(rows[0].end_line, 1);
    assert_eq!(rows[0].sort_key, 30);
    assert_eq!(rows[0].group, "day-30");
    assert_eq!(rows[1].sort_key, 10);
}

/// `begin` is not decoration. The fixture counts the files it has been given
/// and puts the count in each label, so a `begin` that did NOT clear the
/// guest's state shows up as `file 3` on the second scan's first file.
///
/// This is the property the WIT's "every scan is a fresh one" contract rests
/// on, and it is invisible to a test that only ever runs one scan.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn begin_resets_the_guests_per_scan_state() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    src.begin(&[]).await.expect("begin");
    let first = src
        .scan(PathBuf::from("/p/a.org"), org(&[1]))
        .await
        .unwrap();
    let second = src
        .scan(PathBuf::from("/p/b.org"), org(&[2]))
        .await
        .unwrap();
    assert_eq!(first[0].label, "Day 1 (file 1)");
    assert_eq!(second[0].label, "Day 2 (file 2)", "state accumulates…");

    src.begin(&[]).await.expect("second begin");
    let third = src
        .scan(PathBuf::from("/p/c.org"), org(&[3]))
        .await
        .unwrap();
    assert_eq!(
        third[0].label, "Day 3 (file 1)",
        "…and `begin` is what clears it"
    );
}

/// One bad file must not fail the agenda — `error-parser`'s rule, same
/// failure class. The guest's `err` surfaces as an `Err` the scan skips, and
/// the source keeps working: an err is not a quarantine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_file_errs_without_killing_the_source() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    src.begin(&[]).await.expect("begin");
    let err = src
        .scan(PathBuf::from("/p/bad.org"), "BROKEN\n".to_string())
        .await
        .expect_err("the guest rejects this file");
    assert!(err.contains("malformed file"), "got {err}");

    let ok = src
        .scan(PathBuf::from("/p/good.org"), org(&[7]))
        .await
        .expect("still alive after a guest err");
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0].sort_key, 7);
}

/// A file with nothing dated in it returns an empty list, not an error. The
/// distinction matters: the scan logs an `Err` and stays silent on `Ok([])`,
/// and a project of ordinary org notes would otherwise fill the log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_with_no_rows_returns_empty_rather_than_erring() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    src.begin(&[]).await.expect("begin");
    let rows = src
        .scan(
            PathBuf::from("/p/prose.org"),
            "just some prose\n".to_string(),
        )
        .await
        .expect("no rows is not an error");
    assert!(rows.is_empty());
}

/// OT.3: the host parses the file it already read and lends the guest a
/// borrowed `tree-snapshot`, so the file's text never crosses the boundary.
///
/// `.rs` rather than `.org` because these tests load no `language` plugin — the
/// org grammar is registered by the org plugin, not the host — so `.org` here
/// resolves to no language and takes the text arm, which every OTHER test in
/// this file covers. Rust is bundled, so it is the extension that proves the
/// tree arm without standing up a whole language seam.
///
/// The fixture reports the ROOT KIND, which no text scan could produce: seeing
/// `tree:source_file:2` is proof the parse happened host-side and the handle
/// crossed, not that the guest re-derived something from a string.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_lends_a_tree_when_the_extension_has_a_language() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    src.begin(&[]).await.expect("begin");
    let rows = src
        .scan(
            PathBuf::from("/p/a.rs"),
            "fn a() {}\nfn b() {}\n".to_string(),
        )
        .await
        .expect("scan");

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].label, "tree:source_file:2",
        "the host parsed off-buffer and lent the tree; the guest read it"
    );
}

/// The text arm is not a leftover — it is what keeps an agenda source
/// independent of the `language` seam, which `scanned-excerpt-source.wit` calls out
/// explicitly ("would make an agenda source *require* a language when the two
/// are independent contributions").
///
/// A filetype with no registered grammar must still scan, and it must scan
/// through the guest's text path rather than being skipped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_falls_back_to_text_when_the_extension_has_no_language() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    src.begin(&[]).await.expect("begin");
    let rows = src
        .scan(PathBuf::from("/p/a.unknownext"), org(&[7]))
        .await
        .expect("scan");

    assert_eq!(rows.len(), 1, "the text arm still produced the row");
    assert_eq!(rows[0].sort_key, 7);
    assert!(
        !rows[0].label.starts_with("tree:"),
        "no grammar for this extension, so the guest must have been handed text"
    );
}

/// AF.3 — a source reads its own configuration inside `roots`.
///
/// The gap this guards was invisible for the life of the seam: the agenda
/// store was the ONLY seam store never handed the option registry (`context`,
/// `event`, `transient` and `grammar` all get it; 73842466 fixed the same
/// omission for events). Every agenda test drove `extensions` / `begin` /
/// `scan`, none of which reads an option, so the config path had never been
/// called once — and `get-option` answered `none` for anything a guest asked.
///
/// The world had the matching hole: `scanned-excerpt-source-plugin` did not import
/// `config`, so it declared an export it gave no way to implement. Either half
/// alone still leaves `roots` answerable only with a compiled-in constant,
/// which is why both moved together and why this test asserts through the
/// option rather than through the fixture's fallback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_source_answers_roots_from_its_own_option() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: agenda fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();

    let config = std::sync::Arc::new(lattice_config::ConfigRegistry::new());
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile agenda fixture");
    let manifest = PluginManifest::new("agenda-fixture", Vec::new(), CapabilitySet::empty());
    let (client, actor) = host
        .spawn_agenda_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
            Some(&config),
        )
        .await
        .expect("spawn agenda source");
    tokio::spawn(actor.run());
    let src =
        WasmScannedExcerptSource::new(client, normalise_extensions(vec!["org".to_string()]), None);

    // Unset: the fixture's own fallback, which is also the pre-AF.3 answer.
    assert_eq!(
        src.roots().await.unwrap(),
        vec![
            "/agenda-guest/notes".to_string(),
            "~/agenda-guest/one.org".to_string(),
        ],
        "with nothing configured the guest answers its compiled default"
    );

    // Set it, and the guest must see the new value — this is the assertion
    // that fails against a store with no registry.
    // Registered under the plugin's namespace, which is how `get-option`
    // resolves a short name (`roots` → `agenda-fixture.roots`).
    assert!(lattice_plugin_host::config_host::register_plugin_option(
        &config,
        "agenda-fixture.roots",
        lattice_plugin_host::config_host::PluginOptionKind::String,
        "/from/the/option",
        "test: the roots this fixture reports",
    ));
    assert_eq!(
        src.roots().await.unwrap(),
        vec!["/from/the/option".to_string()],
        "`config.get-option` must answer inside an agenda call; `none` here \
         means the agenda store was never handed the option registry"
    );
}

/// AF.1 — what the guest names as its roots reaches the host verbatim.
///
/// The regression this guards is the one OT.4 spent a slice on: a seam wired
/// through the WIT, the trampoline and the guest that delivers nothing, because
/// nothing in production actually calls it. So this asserts on the ADAPTER —
/// `ScannedExcerptSource::roots`, the method the scan calls — rather than on the
/// `AgendaClient` beneath it, and it asserts the exact strings rather than
/// merely "not empty".
///
/// Unexpanded on this side deliberately. `~` expansion is the host's, and a
/// fixture that pre-expanded would hide whether it happens at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_source_names_the_paths_it_wants_scanned() {
    let Some(_) = guest_wasm() else {
        eprintln!("SKIP: agenda fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    let roots = src.roots().await.expect("roots never fails the scan");
    assert_eq!(
        roots,
        vec![
            "/agenda-guest/notes".to_string(),
            "~/agenda-guest/one.org".to_string(),
        ],
        "the guest's list crosses back in order and unmodified"
    );

    // Per scan, not once at load: a second call must reach the guest again,
    // because the answer comes from user config and has to follow a `:set`.
    let again = src.roots().await.expect("a second scan asks again");
    assert_eq!(
        again, roots,
        "and answers consistently while config is stable"
    );
}

/// OA.5: a guest's per-row style spans cross the boundary and survive
/// validation.
///
/// The seam exists so an agenda row is coloured by the SOURCE's semantics —
/// which word is a TODO keyword, a priority, a tag — rather than by the file's
/// tree-sitter grammar, which is all the host has on its own.
///
/// Offsets stay relative to the row's own line: the guest cannot know where
/// its row lands until every other file's rows have been interleaved by the
/// host's sort, so rebasing them here would be rebasing against nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guests_row_spans_cross_the_boundary() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    src.begin(&[]).await.expect("begin");
    let rows = src
        .scan(PathBuf::from("/p/a.org"), org(&[1]))
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]
            .spans
            .iter()
            .map(|s| (s.start, s.end, s.slot.as_str()))
            .collect::<Vec<_>>(),
        vec![(2, 6, "keyword")],
        "the guest's span arrives with its own line's offsets and its slot NAME \
         — a name, because a `Style` is a closed Rust enum plus an interned id \
         and neither crosses an ABI"
    );
}

/// OA.11a: the view's scan arguments reach the guest, uninterpreted.
///
/// The fixture stashes them in `begin` and rides them in every row's label,
/// so this asserts the whole path — `AgendaClient::begin` → the actor → the
/// WIT call → the guest's own state → back out through `scan`. The seam's
/// recurring failure is the one OT.4 spent a slice on: wired end to end and
/// delivering nothing. A test that only checked `begin` returned `Ok` would
/// pass against a host that dropped the args on the floor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_views_scan_args_reach_the_guest() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let src = source(&host).await;

    src.begin(&["waiting".to_string(), "extra".to_string()])
        .await
        .expect("begin");
    let rows = src
        .scan(PathBuf::from("/p/a.org"), org(&[1]))
        .await
        .unwrap();
    assert_eq!(
        rows[0].label, "Day 1 (file 1) [waiting,extra]",
        "both args cross, in order"
    );

    // …and a scan opened for nothing is byte-for-byte what it was before this
    // slice, which is what keeps every existing trigger unchanged.
    src.begin(&[]).await.expect("begin");
    let rows = src
        .scan(PathBuf::from("/p/a.org"), org(&[1]))
        .await
        .unwrap();
    assert_eq!(rows[0].label, "Day 1 (file 1)");
}

/// OA.11a: a differently-parameterised scan is a different generation.
///
/// The guest folds its args into the key `begin` returns, and it must: two
/// custom commands ask two different questions of the same unchanged files, so
/// a cache keyed only on the files would serve the first command's rows under
/// the second command's name. Every one of those rows would look plausible,
/// which is what makes this worth pinning rather than trusting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_args_change_the_generation_key() {
    let Some(_) = guest_wasm() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data")).unwrap();
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile agenda fixture");
    let manifest = PluginManifest::new("agenda-fixture", Vec::new(), CapabilitySet::empty());
    let (client, actor) = host
        .spawn_agenda_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
            None,
        )
        .await
        .expect("spawn agenda source");
    tokio::spawn(actor.run());

    let default = client.begin(Vec::new()).await.expect("begin");
    let waiting = client
        .begin(vec!["waiting".to_string()])
        .await
        .expect("begin");
    let refile = client
        .begin(vec!["refile".to_string()])
        .await
        .expect("begin");

    assert_ne!(
        default, waiting,
        "the default scan and a named command are not the same question"
    );
    assert_ne!(waiting, refile, "…and neither are two different commands");
    assert_eq!(
        waiting,
        client
            .begin(vec!["waiting".to_string()])
            .await
            .expect("begin"),
        "the same args are the same generation, or nothing would ever cache"
    );
}
