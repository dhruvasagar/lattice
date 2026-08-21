//! PR.2 — the project resolver, wired into a real booted editor.
//!
//! `lattice-core`'s own tests cover the resolution rules. What these
//! cover is the *wiring*: that the service is registered under the alias
//! consumers look it up by, that `:cd` re-points the fallback, that the
//! `project.root-markers` option reaches the resolver, and that
//! `:project-root` reports what the resolver decided.
//!
//! The service-alias test is not ceremony. `ServiceRegistry::register::<T>`
//! keys on `TypeId::of::<T>()`, so registering the concrete
//! `Arc<MarkerResolver>` instead of the `ProjectResolverHandle` alias
//! would make every consumer's `get` return `None` — silently, with the
//! editor still working and every root quietly wrong.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_core::Document as CoreDocument;
use lattice_core::{ProjectKind, ProjectResolverHandle};
use lattice_host::editor::Editor;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

/// Per-test unique directory. The counter matters: a timestamp alone
/// collides under parallel `cargo test`.
fn tempdir() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lattice-host-project-{id}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn mkdirs(base: &Path, rel: &str) -> PathBuf {
    let p = base.join(rel);
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn the_resolver_is_registered_under_the_handle_alias() {
    // The Arc/TypeId rule. Registering the concrete type would make
    // every `get` return `None` while the editor kept working.
    let editor = boot();
    assert!(
        editor.services.get::<ProjectResolverHandle>().is_some(),
        "ProjectResolverHandle must resolve at boot"
    );
}

#[test]
fn a_pathless_buffer_resolves_rather_than_failing() {
    // A scratch buffer has no tree to walk. The contract is that this
    // is a real answer, not an error — every consumer can ask.
    let editor = boot();
    let project = editor.active_buffer_project();
    assert!(
        !project.root.as_os_str().is_empty(),
        "a pathless buffer must still resolve to a root"
    );
}

#[test]
fn cd_repoints_the_pwd_fallback() {
    // `:cd` moves the fallback for buffers with no project, and
    // `set_pwd` drops the cache so an already-answered buffer is not
    // left reporting the old directory.
    let base = tempdir();
    let lonely = mkdirs(&base, "lonely");
    let before = mkdirs(&base, "before");
    let after = mkdirs(&base, "after");

    let mut editor = boot();
    let resolver = editor.services.get::<ProjectResolverHandle>().unwrap();
    resolver.set_pwd(before.clone());
    assert_eq!(resolver.for_path(&lonely.join("a.rs")).root, before);

    // Drive the real `:cd` path rather than calling `set_pwd` again —
    // the point is that the effect arm is wired, not that the resolver
    // works.
    let mut out = lattice_host::dispatch::DispatchOutcome::default();
    editor.execute_ex_line(&format!("cd {}", after.display()), &mut out);

    let got = resolver.for_path(&lonely.join("a.rs"));
    assert_eq!(got.root, after, "`:cd` must re-point the pwd fallback");
    assert_eq!(got.kind, ProjectKind::Pwd);
    cleanup(&base);
}

#[test]
fn a_marker_rooted_buffer_ignores_cd() {
    // `:cd` changes only the fallback. A buffer that HAS a project must
    // not follow the working directory around — this is what lets
    // several projects co-exist while you move about.
    let base = tempdir();
    let repo = mkdirs(&base, "repo");
    mkdirs(&repo, ".git");
    let elsewhere = mkdirs(&base, "elsewhere");

    let mut editor = boot();
    let resolver = editor.services.get::<ProjectResolverHandle>().unwrap();
    assert_eq!(resolver.for_path(&repo.join("a.rs")).root, repo);

    let mut out = lattice_host::dispatch::DispatchOutcome::default();
    editor.execute_ex_line(&format!("cd {}", elsewhere.display()), &mut out);
    assert_eq!(
        resolver.for_path(&repo.join("a.rs")).root,
        repo,
        "a marker-rooted answer must survive a `:cd`"
    );
    cleanup(&base);
}

#[test]
fn project_root_command_resolves_by_the_name_a_user_types() {
    // Through the alias table, which is the path `:` actually takes —
    // the canonical id is `ex:project-root`, and asserting only that
    // would pass with the alias missing, i.e. with the command
    // unreachable.
    let editor = boot();
    let registry = editor.registry.load();
    assert!(
        lattice_host::excommand::resolve_command_name_or_alias(&registry, "project-root").is_some(),
        "`:project-root` must resolve from what a user types"
    );
}

#[test]
fn project_root_reports_the_marker_that_decided_it() {
    // "Where is my root" and "why is it there" are the same question:
    // a root one directory higher than expected is almost always a
    // stray manifest, and naming it is what makes that obvious.
    let base = tempdir();
    let repo = mkdirs(&base, "repo");
    mkdirs(&repo, ".git");

    let mut editor = boot();
    let resolver = editor.services.get::<ProjectResolverHandle>().unwrap();
    resolver.set_pwd(repo.clone());

    let mut out = lattice_host::dispatch::DispatchOutcome::default();
    editor.execute_ex_line("project-root", &mut out);

    let msg = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        msg.contains(&repo.display().to_string()),
        "`:project-root` should echo the root, got {msg:?}"
    );
    assert!(
        msg.contains(".git"),
        "`:project-root` should name the deciding marker, got {msg:?}"
    );
    cleanup(&base);
}

// ── PR.3: the terminal ─────────────────────────────────────────────────────

/// The presenting bug. `:terminal` used to spawn at `dirname(active
/// file)`, so opening `crates/lattice-host/src/dispatch.rs` and hitting
/// `:terminal` landed you in `crates/lattice-host/src/`.
///
/// This asserts the value `do_terminal_spawn` feeds to
/// `SpawnConfig.cwd` rather than spawning a PTY: the spawn itself is
/// `Command::cwd(...)`, which is the OS's job, and a real PTY in a unit
/// test buys nothing but flake. What can regress is the resolution, and
/// the `assert_ne` is the half that fails on the old behaviour — an
/// equality-only test would pass on any code that happened to return
/// the repo root for an unrelated reason.
#[test]
fn a_terminal_roots_at_the_project_not_the_files_directory() {
    let base = tempdir();
    let repo = mkdirs(&base, "repo");
    mkdirs(&repo, ".git");
    let deep = mkdirs(&repo, "crates/thing/src");
    let file = deep.join("lib.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    let editor = Editor::boot(
        lattice_core::DocumentBuilder::default()
            .with_path(file)
            .with_text("fn main() {}\n")
            .build(),
    );

    let project = editor.active_buffer_project();
    assert_eq!(project.root, repo, "a terminal should open at the project");
    assert_ne!(
        project.root, deep,
        "the pre-PR.3 behaviour — the file's own directory"
    );
    cleanup(&base);
}

/// Two buffers in two checkouts each answer for themselves, so two
/// terminals opened from them get different roots. This is the
/// "multiple projects co-exist" property at the level a user meets it.
#[test]
fn terminals_from_different_checkouts_get_different_roots() {
    let base = tempdir();
    let a = mkdirs(&base, "a");
    mkdirs(&a, ".git");
    let b = mkdirs(&base, "b");
    mkdirs(&b, ".git");

    let mut roots = Vec::new();
    for repo in [&a, &b] {
        let file = repo.join("src/main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "\n").unwrap();
        let editor = Editor::boot(
            lattice_core::DocumentBuilder::default()
                .with_path(file)
                .with_text("\n")
                .build(),
        );
        roots.push(editor.active_buffer_project().root);
    }

    assert_eq!(roots[0], a);
    assert_eq!(roots[1], b);
    assert_ne!(
        roots[0], roots[1],
        "projects must not bleed into each other"
    );
    cleanup(&base);
}

/// PR.4: the resolver must be registered BEFORE the subsystem installs
/// that capture handles at install time.
///
/// It first landed after them, which nothing caught: every consumer so
/// far looks the handle up lazily at action time, so the editor worked
/// and the ordering was wrong only for a subsystem that had not been
/// written yet. That is precisely the class of boot-order regression
/// `boot_regression_pins` exists for — a `None` lookup that degrades
/// silently instead of failing.
///
/// Pinned by position rather than by outcome because the outcome is
/// invisible until a future subsystem captures at install time, and by
/// then the cause is far from the symptom.
#[test]
fn the_resolver_is_registered_before_subsystem_installs() {
    let src = include_str!("../src/editor_boot.rs");
    let register = src
        .find("boot.register_service::<lattice_core::ProjectResolverHandle>")
        .expect("the resolver registration must exist");
    for subsystem in [
        "lattice_ai::install(&mut boot)",
        "lattice_terminal::install(&mut boot)",
        "lattice_compilation::install(&mut boot)",
        "lattice_multibuffer::install(&mut boot)",
        "lattice_magit::install(&mut boot)",
    ] {
        let at = src
            .find(subsystem)
            .unwrap_or_else(|| panic!("{subsystem} should be in editor_boot"));
        assert!(
            register < at,
            "the project resolver must be registered before `{subsystem}`, \
             or a subsystem capturing the handle at install time gets None"
        );
    }
}
