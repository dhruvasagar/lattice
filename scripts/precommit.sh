#!/usr/bin/env bash
# The three gates every commit passes, in order. See CLAUDE.md
# ("A commit is fmt-clean, warning-clean and green").
#
#   scripts/precommit.sh                  # whole workspace (slow)
#   scripts/precommit.sh lattice-magit    # just the crates you touched
#
# Exists because `cargo build` succeeding proves none of this: warnings
# do not fail a build, and a `grep "^error"` will not show them.
set -uo pipefail
cd "$(dirname "$0")/.."

if [ "$#" -gt 0 ]; then
    PKGS=(); for p in "$@"; do PKGS+=(-p "$p"); done
    SCOPE="${*}"
    # Only warnings in THESE crates' own sources count. Building
    # `-p lattice-magit` also compiles its dependencies, and reporting
    # their pre-existing warnings as yours makes the gate cry wolf —
    # which is how a gate stops being read.
    ONLY="$*"
else
    PKGS=(--workspace); SCOPE="workspace"; ONLY=""
fi
fail=0
say() { printf '\n=== %s\n' "$1"; }

say "1/3  fmt (strict — CI gates on this)"
if cargo fmt --all -- --check >/dev/null 2>&1; then
    echo "  clean"
else
    echo "  DRIFT — run: cargo fmt --all"
    cargo fmt --all -- --check 2>&1 | grep '^Diff in' | sed 's/^/  /' | sort -u | head -20
    fail=1
fi

say "2/3  warnings in $SCOPE"
json=$(cargo clippy "${PKGS[@]}" --all-targets --message-format=json 2>/dev/null)
printf '%s' "$json" | ONLY="$ONLY" python3 -c '
import sys, json, collections, os
# Restrict to the named crates own files when a scope was given.
only = [f"crates/{p}/" for p in os.environ.get("ONLY", "").split() if p]
def mine(msg):
    if not only:
        return True
    return any(
        sp.get("file_name", "").startswith(tuple(only))
        for sp in msg.get("spans", [])
        if sp.get("is_primary")
    )
rustc = collections.Counter(); deny = collections.Counter()
# Deliberately `warn` in [workspace.lints] and overwhelmingly test code.
POLICY = {"clippy::unwrap_used", "clippy::panic", "clippy::todo"}
clippy = collections.Counter()
for line in sys.stdin:
    if not line.startswith("{"):
        continue
    try:
        m = json.loads(line)
    except Exception:
        continue
    msg = m.get("message")
    if not msg:
        continue
    code = (msg.get("code") or {}).get("code")
    if not code or not mine(msg):
        continue
    if msg.get("level") == "error":
        deny[code] += 1
    elif msg.get("level") == "warning":
        (clippy if code.startswith("clippy::") else rustc)[code] += 1
bad = 0
if deny:
    print("  DENY-level (always blocking):")
    for c, n in deny.most_common():
        print(f"    {n:4} {c}")
    bad = 1
if rustc:
    print("  rustc warnings (must be ZERO — these mean something you edited left an orphan):")
    for c, n in rustc.most_common():
        print(f"    {n:4} {c}")
    bad = 1
else:
    print("  rustc: clean")
other = {c: n for c, n in clippy.items() if c not in POLICY}
if other:
    print("  clippy (non-policy) — must be no NEW ones from your change:")
    for c, n in sorted(other.items(), key=lambda kv: -kv[1]):
        print(f"    {n:4} {c}")
else:
    print("  clippy: clean apart from the deliberate unwrap/panic policy warns")
sys.exit(bad)
' || fail=1

say "3/3  tests in $SCOPE"
if cargo test "${PKGS[@]}" >/tmp/lattice-precommit-tests.log 2>&1; then
    # Sum across every suite. `tail -1` alone lands on the doc-test line,
    # which is usually "0 passed" and reads like nothing ran.
    awk '/^test result: ok/ { n += $4 } END { printf "  %d tests passed — green\n", n }' \
        /tmp/lattice-precommit-tests.log
else
    echo "  FAILURES (full log: /tmp/lattice-precommit-tests.log):"
    grep -A5 '^failures:$' /tmp/lattice-precommit-tests.log | grep '^    ' | sort -u | sed 's/^/  /' | head
    echo "  If you believe a failure predates your work, PROVE it:"
    echo "    git stash -u && cargo test ... && git stash pop"
    fail=1
fi

printf '\n'
if [ "$fail" -eq 0 ]; then echo "OK — safe to commit."; else echo "NOT clean — do not commit yet."; fi
exit "$fail"
