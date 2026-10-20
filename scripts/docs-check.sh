#!/usr/bin/env bash
# The docs gates CI runs, runnable before a push. `scripts/precommit.sh`
# runs this as its fourth gate; run it alone after a docs-only change.
#
#   scripts/docs-check.sh                  # rustdoc for the whole workspace
#   scripts/docs-check.sh lattice-magit    # rustdoc for just these crates
#
#   1. Generated references and help — `cargo test -p xtask
#      -p lattice-plugin-api -p lattice-help`: the crate map, the plugin-API
#      reference and guides, help topics, anchors and the `:help` index.
#   2. Rustdoc — `cargo doc --no-deps` for the crates in scope (CI's `doc`
#      job), and the doc-coverage ratchet for those that opted in.
#   3. The site — `site/scripts/sync-docs.sh`, then `zola build`, which fails
#      on any broken internal link or anchor (the deploy-docs build).
#
# Every step runs every time: no "only if docs/ changed" filter. On the
# plugin-manifest branch the crate map went stale from a `Cargo.toml`
# dependency and the site sync from a plugin's manifest moving into its
# source. A path filter would have had to predict both, and a guard that
# guesses where its failures come from is blind exactly where it guessed
# wrong. All three reached CI.
#
# For the same reason a missing tool FAILS the gate rather than skipping
# it: a check that switches itself off without a word is no guard.
# `DOCS_CHECK_SKIP_SITE=1` opts out of step 3 on purpose — the gate then
# passes, and says in capitals that the site was not checked.
set -uo pipefail
unset CDPATH  # or `cd` echoes the directory it lands in
cd "$(dirname "$0")/.."

if [ "$#" -gt 0 ]; then
    PKGS=(); for p in "$@"; do PKGS+=(-p "$p"); done
    SCOPE=("$@")
else
    PKGS=(--workspace); SCOPE=()
fi
fail=0
log=$(mktemp -d)
trap 'rm -rf "$log"' EXIT
step() { printf '  %s\n' "$1"; }

# 1. Generated references and help. `--no-fail-fast`: cargo otherwise stops
# at the first failing test binary, and a stale crate map would hide behind
# a broken help anchor until the next run.
if cargo test -p xtask -p lattice-plugin-api -p lattice-help --locked --no-fail-fast >"$log/ref.log" 2>&1; then
    step "references + help: clean"
else
    step "references + help: FAILED"
    grep -A8 '^---- ' "$log/ref.log" | grep -vE '^(--|note:|failures:|\s*$)' | sed 's/^/    /' | head -30
    fail=1
fi

# 2. Rustdoc, then the coverage ratchet for the opted-in crates in scope —
# with the toolchain CI documents with (`stable`), not the pinned one.
# rustdoc 1.94.0 (rust-toolchain.toml) crashes on lattice-ui-tui ("no
# resolution for lattice_host::messages::MESSAGES_BUFFER_NAME", then "no
# resolutions for a doc link"); stable 1.97 documents it unchanged. This
# gate exists to predict CI, so it uses CI's compiler, in its own target dir
# so neither toolchain evicts the other's cache.
DOC_TOOLCHAIN="${DOCS_CHECK_TOOLCHAIN:-stable}"
doc_env=(env RUSTUP_TOOLCHAIN="$DOC_TOOLCHAIN" CARGO_TARGET_DIR="${DOCS_CHECK_TARGET_DIR:-target/stable-doc}")
if ! "${doc_env[@]}" rustc --version >/dev/null 2>&1; then
    step "rustdoc: the \`$DOC_TOOLCHAIN\` toolchain is not installed — rustup toolchain install $DOC_TOOLCHAIN"
    fail=1
elif "${doc_env[@]}" cargo doc --no-deps --locked "${PKGS[@]}" >"$log/doc.log" 2>&1; then
    step "rustdoc: clean"
else
    step "rustdoc: FAILED"
    grep -E '^error' "$log/doc.log" | sort -u | sed 's/^/    /' | head -10
    fail=1
fi
opted=$(grep -lE '^#!\[(warn|deny)\(missing_docs\)\]' crates/*/src/lib.rs 2>/dev/null |
    sed -E 's#crates/([^/]+)/src/lib.rs#\1#')
cover=()
if [ "${#SCOPE[@]}" -gt 0 ]; then
    for c in "${SCOPE[@]}"; do
        if printf '%s\n' "$opted" | grep -qx "$c"; then cover+=("$c"); fi
    done
fi
if [ "${#SCOPE[@]}" -gt 0 ] && [ "${#cover[@]}" -eq 0 ]; then
    step "doc coverage: none of these crates has opted in"
elif "${doc_env[@]}" scripts/doc-coverage.sh ${cover[@]+"${cover[@]}"} >"$log/cov.log" 2>&1; then
    step "doc coverage: clean"
else
    step "doc coverage: FAILED"
    grep -E ': \[(missing_docs|rustdoc::)' "$log/cov.log" | sed 's/^/    /' | head -20
    fail=1
fi

# 3. The site.
if [ "${DOCS_CHECK_SKIP_SITE:-0}" = "1" ]; then
    step "site: NOT CHECKED (DOCS_CHECK_SKIP_SITE=1) — CI will be the first to build it"
elif ! command -v zola >/dev/null; then
    step "site: zola is not installed — CI uses zola 0.22.1"
    step "  install it, or DOCS_CHECK_SKIP_SITE=1 to commit unchecked"
    fail=1
elif ! python3 site/scripts/sync-docs.sh >"$log/sync.log" 2>&1; then
    step "site: sync-docs FAILED"
    grep -iE 'error|die' "$log/sync.log" | sed 's/^/    /' | head -10
    fail=1
elif ! (cd site && zola build) >"$log/zola.log" 2>&1; then
    step "site: zola build FAILED"
    grep -E '^(ERROR|\s+[0-9]+\.)' "$log/zola.log" | sed 's/^/    /' | head -12
    fail=1
else
    step "site: clean"
fi

exit "$fail"
