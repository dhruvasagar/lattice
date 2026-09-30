#!/usr/bin/env bash
# AD.8: the Rust API documentation ratchet.
#
# A crate opts in by putting `#![warn(missing_docs)]` (or `deny`) at the top
# of its `src/lib.rs` — once every public item in it is documented. From then
# on this script, run in CI, fails on:
#
#   - any public item in that crate without a doc comment (`missing_docs`);
#   - any intra-doc link in it that does not resolve
#     (`rustdoc::broken_intra_doc_links`, `rustdoc::private_intra_doc_links`).
#
# The opted-in set is DISCOVERED from the attribute, not listed here, so there
# is no second list to drift: opting a crate in is one line in the crate.
#
# Not workspace-wide, deliberately: ~1,200 undocumented public items in the
# seven most-used crates alone, and a switch that floods is a switch people
# learn to ignore. See docs/dev/architecture/api-docs.md §3.1.
#
# Usage: scripts/doc-coverage.sh            # every opted-in crate
#        scripts/doc-coverage.sh <crate>...  # just these (must be opted in)

set -euo pipefail
cd "$(dirname "$0")/.."

opted_in() {
    grep -lE '^#!\[(warn|deny)\(missing_docs\)\]' crates/*/src/lib.rs 2>/dev/null |
        sed -E 's#crates/([^/]+)/src/lib.rs#\1#' | sort
}

if [ "$#" -gt 0 ]; then
    crates=("$@")
    for c in "${crates[@]}"; do
        if ! opted_in | grep -qx "$c"; then
            echo "doc-coverage: $c has not opted in (no #![warn(missing_docs)] in crates/$c/src/lib.rs)" >&2
            exit 2
        fi
    done
else
    mapfile -t crates < <(opted_in)
fi

if [ "${#crates[@]}" -eq 0 ]; then
    echo "doc-coverage: no crate has opted in yet — nothing to check."
    exit 0
fi

echo "doc-coverage: ${crates[*]}"
args=()
for c in "${crates[@]}"; do args+=(-p "$c"); done

# The lints are READ from rustdoc's diagnostics rather than promoted with
# `-D`: a crate's own `#![warn(missing_docs)]` outranks a command-line `-D`,
# so `RUSTDOCFLAGS=-D missing_docs` reports "clean" over a crate full of gaps
# (it did, the first time this ran). `--force-warn` cannot be overridden by an
# attribute, so every finding reaches the JSON stream, and this script decides.
out=$(mktemp)
trap 'rm -f "$out"' EXIT
RUSTDOCFLAGS="${RUSTDOCFLAGS:-} --force-warn missing_docs --force-warn rustdoc::broken_intra_doc_links --force-warn rustdoc::private_intra_doc_links" \
    cargo doc --no-deps --locked --message-format=json "${args[@]}" >"$out"

python3 - "$out" "${crates[@]}" <<'PY'
import json, sys
path, crates = sys.argv[1], {c.replace('-', '_') for c in sys.argv[2:]}
lints = {'missing_docs', 'rustdoc::broken_intra_doc_links', 'rustdoc::private_intra_doc_links'}
found = []
for line in open(path, encoding='utf-8'):
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    if msg.get('reason') != 'compiler-message':
        continue
    if msg.get('target', {}).get('name') not in crates:
        continue
    diag = msg['message']
    code = (diag.get('code') or {}).get('code')
    if code in lints:
        span = next((s for s in diag.get('spans', []) if s.get('is_primary')), None)
        where = f"{span['file_name']}:{span['line_start']}" if span else '?'
        found.append(f'{where}: [{code}] {diag["message"]}')
if found:
    print(f'doc-coverage: {len(found)} finding(s) in opted-in crates:')
    print('\n'.join(sorted(set(found))))
    sys.exit(1)
print('doc-coverage: clean')
PY
