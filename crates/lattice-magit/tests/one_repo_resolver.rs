//! MR.5: magit asks "which repository" in exactly one place.
//!
//! Design: `docs/dev/architecture/magit-repo-scoping.md` §4, whose
//! anti-rot rule this is: *after this change, no magit code outside the
//! resolver reads the process's repository.*
//!
//! ## Why a source-grep test
//!
//! Every behavioural test of repo scoping runs in ONE checkout, because
//! that is what a test process has. So a site that resolves from the
//! working directory passes every one of them — the two repositories are
//! the same repository — and only fails in front of a user with two
//! checkouts open. MR.3 and MR.4 each shipped believing they were
//! complete; this test is what found the remainder.
//!
//! ## Why it greps the DISCOVERY, not just the helper
//!
//! `magit_workdir()` is the obvious spelling, and grepping for it is how
//! MR.3 and MR.4 built their work lists. Both lists were short by
//! several sites, because a handful of places spelled the same question
//! out — `Repository::discover(".")` — and were therefore invisible:
//!
//! - `spawn_gitignore` wrote `.gitignore` into the process's repository.
//! - `magit-rebase-mode`'s `on_activate` — an entire VIEW — resolved from
//!   the process's repository through MR.3, which was the slice whose
//!   whole subject was views.
//! - `:magit-branch-create` created the branch there too.
//!
//! A guard that matched only the helper's name would have certified all
//! three. So this matches the question in any spelling, and the
//! exceptions are listed one by one with the reason attached.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};

/// Files allowed to ask the process where it is, and why.
///
/// Each entry is a decision, not a suppression. A new one belongs here
/// only with the same kind of reason: the working directory is genuinely
/// what that code is about.
const ALLOWED: &[(&str, &str)] = &[
    (
        "src/workdir.rs",
        "THE resolver. `magit_workdir()` lives here and is the only \
         reader of the process's repository; `repo_for_trigger` makes it \
         the last of three questions rather than the first.",
    ),
    (
        "src/repo_scope.rs",
        "The record and the trigger body, which call the resolver as \
         their documented fall-back — a fresh editor with nothing open \
         still answers `C-x g`.",
    ),
    (
        "src/picker_sources.rs",
        "MR.6 (open): the branch / commit / stash / ref pickers list the \
         process's repository. They receive `PickerContext`, which DOES \
         carry the active buffer, so the fix is mechanical — each source \
         takes the store + scopes handles at registration. Excepted \
         rather than silently passing, so the debt is visible here.",
    ),
    (
        "src/providers/project_diff.rs",
        "MR.6 (open): the opener runs behind `ProviderViewOpener`, whose \
         `ModeActivator` exposes no active buffer. Same gap \
         `TransientContext` had before MR.4 closed it, and the fix is the \
         same shape.",
    ),
    (
        "src/git_config.rs",
        "A process-wide config cache keyed by nothing. Making it \
         per-repository is a cache-shape change rather than a scoping \
         one, so it is a decision deferred rather than a site missed.",
    ),
];

/// Sites that are ABOUT the working directory, so reading it is correct.
///
/// Matched as substrings of the offending line, scoped to one file each
/// — narrow enough that an unrelated site cannot slip past by accident.
const INTENTIONAL_CWD: &[(&str, &str)] = &[
    (
        "src/magit_global_mode.rs",
        // `:magit-init` seeds its prompt with the cwd: initialising
        // *here* is the intent, and there is no repository yet to scope
        // to — that is the point of the command.
        "let cwd = std::env::current_dir()",
    ),
    (
        "src/magit_global_mode.rs",
        // The clone destination defaults to the cwd for the same reason:
        // the repository being cloned does not exist locally yet.
        "let cwd = std::env::current_dir().unwrap_or_else",
    ),
];

/// The question, in every spelling magit has used for it.
const CWD_PATTERNS: &[&str] = &[
    "magit_workdir()",
    r#"discover(".")"#,
    "std::env::current_dir()",
];

fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Everything before the crate's own test modules.
///
/// A test may legitimately probe the checkout it is running in — several
/// do, to assert the fall-back is still the old behaviour. Only
/// production code is constrained.
fn production_part(text: &str) -> &str {
    match text.find("\n#[cfg(test)]\n") {
        Some(i) => &text[..i],
        None => text,
    }
}

#[test]
fn only_the_resolver_reads_the_process_repository() {
    let root = crate_root();
    let mut files = Vec::new();
    rust_sources(&root.join("src"), &mut files);
    assert!(!files.is_empty(), "found no sources — walker is broken");

    let mut offenders: Vec<String> = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.iter().any(|(f, _)| *f == rel) {
            continue;
        }
        for (n, line) in production_part(&text).lines().enumerate() {
            let code = line.trim_start();
            // A comment naming the pattern is documentation, not a call —
            // and several of them explain this very rule.
            if code.starts_with("//") {
                continue;
            }
            if !CWD_PATTERNS.iter().any(|p| code.contains(p)) {
                continue;
            }
            if INTENTIONAL_CWD
                .iter()
                .any(|(f, snippet)| *f == rel && code.contains(snippet))
            {
                continue;
            }
            offenders.push(format!("{rel}:{}: {}", n + 1, code.trim()));
        }
    }
    offenders.sort();

    assert!(
        offenders.is_empty(),
        "magit must ask which repository ONCE, through \
         `repo_scope::active_workdir` (actions), \
         `repo_scope::view_workdir` (a view's activation) or \
         `repo_scope::repo_view_name_with` (a trigger).\n\n\
         These read the process's repository instead:\n  {}\n\n\
         A magit buffer belongs to the repository it was opened for, and \
         a chord pressed in it acts there — see \
         docs/dev/architecture/magit-repo-scoping.md §4. If a site \
         genuinely IS about the working directory (`:magit-init`, \
         `:magit-clone`), add it to `INTENTIONAL_CWD` with the reason.",
        offenders.join("\n  ")
    );
}

/// The exception lists are the argument, so they have to stay readable
/// as one — an entry with no reason is a suppression wearing the guard's
/// clothes.
#[test]
fn every_exception_carries_a_reason() {
    for (file, reason) in ALLOWED {
        assert!(
            reason.len() > 40,
            "{file} is excepted without a real reason: {reason:?}"
        );
    }
}
