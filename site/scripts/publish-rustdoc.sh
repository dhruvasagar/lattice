#!/usr/bin/env bash
# AD.8: publish the workspace rustdoc at <site>/api/.
#
# Run AFTER `zola build` (which rebuilds site/public/ from scratch) and after
# `cargo doc --no-deps --workspace`. Copies target/doc into site/public/api/
# and writes an index page, since stable rustdoc has no workspace landing page
# (`--enable-index-page` is nightly-only).
#
# The crate list on the index is every workspace crate rustdoc produced a page
# for.

set -euo pipefail
cd "$(dirname "$0")/../.."

src=target/doc
dst=site/public/api
if [ ! -d "$src" ]; then
    echo "publish-rustdoc: $src missing — run cargo doc --no-deps --workspace first" >&2
    exit 1
fi
if [ ! -d site/public ]; then
    echo "publish-rustdoc: site/public missing — run zola build first" >&2
    exit 1
fi

rm -rf "$dst"
cp -r "$src" "$dst"

# Workspace crates only (target/doc also holds rustdoc's own static files).
mapfile -t crates < <(
    for d in "$dst"/*/; do
        name=$(basename "$d")
        [ -f "$d/index.html" ] && [ -d "crates/${name//_/-}" ] && echo "$name"
    done | sort
)
if [ "${#crates[@]}" -eq 0 ]; then
    echo "publish-rustdoc: no workspace crate pages in $src" >&2
    exit 1
fi

{
    cat <<'HTML'
<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Lattice — Rust API</title>
<style>
  body { font: 16px/1.5 system-ui, sans-serif; max-width: 46rem; margin: 2rem auto; padding: 0 1rem; }
  code { font-size: .95em; }
  li { margin: .2rem 0; }
</style></head><body>
<h1>Lattice — Rust API</h1>
<p>rustdoc for every crate in the workspace, built from <code>main</code>.
What each crate is, and how they depend on one another, is on the
<a href="../dev/foundations/crates/">crate map</a>. The plugin API — the WIT
package plugins build against — has its own
<a href="../dev/plugin-api/">reference</a>.</p>
<ul>
HTML
    for c in "${crates[@]}"; do
        printf '  <li><a href="%s/index.html"><code>%s</code></a></li>\n' "$c" "${c//_/-}"
    done
    echo '</ul></body></html>'
} >"$dst/index.html"

echo "publish-rustdoc: ${#crates[@]} crates -> $dst/"
