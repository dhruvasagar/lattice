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

# Source pages point at the file on GitHub instead of being published. They
# were 60 MB and 765 files of a 310 MB, 9k-file site, and GitHub Pages must
# finish processing an upload within deploy-pages' 10-minute limit; GitHub
# has the same source, at the commit the docs were built from.
# (`--html-no-source` would skip generating them, but is nightly-only.)
cargo metadata --no-deps --format-version 1 >"$dst/.metadata.json"
GITHUB_BLOB="https://github.com/dhruvasagar/lattice/blob/${GITHUB_SHA:-$(git rev-parse HEAD)}" \
python3 - "$dst" <<'PY'
import json, os, re, sys
dst = sys.argv[1]
blob = os.environ['GITHUB_BLOB']
root = os.getcwd()
meta = json.load(open(os.path.join(dst, '.metadata.json')))
# rustdoc names a source dir after the target, and mirrors paths relative to
# the directory of the target's root file.
dirs = {}
for pkg in meta['packages']:
    for t in pkg['targets']:
        name = t['name'].replace('-', '_')
        d = os.path.relpath(os.path.dirname(t['src_path']), root).replace(os.sep, '/')
        dirs.setdefault(name, d)
link = re.compile(r'href="((?:\.\./)*)src/([A-Za-z0-9_]+)/([^"#]+)\.html(?:#(\d+)(?:-(\d+))?)?"')
def to_github(m):
    crate, path, a, b = m.group(2), m.group(3), m.group(4), m.group(5)
    base = dirs.get(crate)
    if base is None:
        return 'href="' + blob + '"'
    if not os.path.isfile(os.path.join(root, base, path)):
        # A build script's output (`include!(concat!(env!("OUT_DIR"), …))`)
        # is not in the repository: point at the crate's source instead.
        return f'href="{blob}/{base}"'
    url = f'{blob}/{base}/{path}'
    if a:
        url += f'#L{a}' + (f'-L{b}' if b else '')
    return f'href="{url}"'
rewritten = 0
for here, _, files in os.walk(dst):
    if os.path.relpath(here, dst).split(os.sep)[0] == 'src':
        continue
    for f in files:
        if not f.endswith('.html'):
            continue
        p = os.path.join(here, f)
        html = open(p, encoding='utf-8').read()
        new, n = link.subn(to_github, html)
        if n:
            rewritten += n
            open(p, 'w', encoding='utf-8').write(new)
print(f'publish-rustdoc: {rewritten} source links point at GitHub')
PY
rm -rf "$dst/src" "$dst/src-files.js" "$dst/.metadata.json"

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

# Every link INTO /api/ must land on a page that now exists. Zola cannot check
# these — /api/ is copied in after it builds, and the crate map's links are
# absolute URLs — so a renamed crate or a mistyped path would otherwise ship
# as a 404. Checks the built site's HTML (relative and absolute hrefs) and the
# two repo entry points that link here, README.md and AGENTS.md.
python3 - <<'PY'
import glob, os, re, sys, tomllib
from urllib.parse import urljoin, urlparse

with open('site/config.toml', 'rb') as fh:
    base = tomllib.load(fh)['base_url'].rstrip('/')
api = base + '/api/'

def exists(url):
    rest = urlparse(url)._replace(fragment='', query='').geturl()[len(api):]
    path = os.path.join('site/public/api', rest)
    if not rest or rest.endswith('/'):
        return os.path.isfile(os.path.join(path, 'index.html'))
    return os.path.isfile(path) or os.path.isfile(os.path.join(path, 'index.html'))

dead, checked = [], 0
for page in glob.glob('site/public/**/*.html', recursive=True):
    if page.startswith('site/public/api/'):
        continue  # rustdoc's own links are rustdoc's business
    rel = os.path.relpath(page, 'site/public').replace(os.sep, '/')
    page_url = f'{base}/{rel}'
    html = open(page, encoding='utf-8', errors='replace').read()
    for href in re.findall(r'href="?([^"\s>]+)', html):
        url = urljoin(page_url, href)
        if url.startswith(api) or url == api.rstrip('/'):
            checked += 1
            if not exists(url if url.startswith(api) else api):
                dead.append(f'{rel}: {href}')
for src in ('README.md', 'AGENTS.md'):
    if os.path.isfile(src):
        text = open(src, encoding='utf-8').read()
        for url in re.findall(re.escape(api) + r'[^)\s>"]*', text):
            checked += 1
            if not exists(url):
                dead.append(f'{src}: {url}')
if dead:
    print('publish-rustdoc: links into /api/ that do not exist:\n  '
          + '\n  '.join(sorted(set(dead))))
    sys.exit(1)
print(f'publish-rustdoc: {checked} links into /api/ resolve')
PY
