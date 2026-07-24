#!/usr/bin/env python3
"""Sync docs from docs/{user,dev}/ to site/content/ for Zola.

Strips YAML frontmatter, converts markdown links (.md extension → clean
URLs for Zola), and converts help:topic links (user docs only).
"""
import os, re, glob

script_dir = os.path.dirname(os.path.abspath(__file__))
site_dir = os.path.dirname(script_dir)
repo_root = os.path.join(site_dir, '..')

RE_VERSION = re.compile(r'^version\s*=\s*"([^"]+)"', re.MULTILINE)

def read_version():
    """Read version from workspace Cargo.toml."""
    cargo_path = os.path.join(repo_root, 'Cargo.toml')
    with open(cargo_path, encoding='utf-8') as fh:
        m = RE_VERSION.search(fh.read())
    return m.group(1) if m else '0.0.0'

def write_version_data():
    """Write version data for Zola's load_data."""
    version = read_version()
    dst = os.path.join(site_dir, 'data', 'version.toml')
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    with open(dst, 'w', encoding='utf-8') as fh:
        fh.write(f'latest = "{version}"\n')
    print(f'  Version: {version}')

def strip_frontmatter(content):
    return re.sub(r'^---\n.*?\n---\n', '', content, count=1, flags=re.DOTALL)

def make_title(content, filename):
    m = re.search(r'^# (.+)$', content, re.MULTILINE)
    if m:
        return m.group(1).strip()
    bname = os.path.splitext(filename)[0]
    return bname.replace('-', ' ').title()

def strip_md_from_links(content):
    """Convert markdown file links (.md) to clean Zola URLs.
    Handles: file.md, ./file.md, ../file.md, file.md#anchor, ../file.md#anchor
    Skips absolute URLs (http://, https://, mailto:).
    """
    return re.sub(
        r'\]\((?!https?://|mailto:|#)([^)]*?)\.md(#[^)]*)?\)',
        lambda m: '](' + m.group(1) + (m.group(2) or '') + ')',
        content
    )

def rewrite_root_links(content):
    """Rewrite ../../<path> and ../../../<path> links that leave the Zola
    content tree (i.e., resolve to files outside content/{docs,dev}/).
    Must run BEFORE strip_md_from_links so .md extensions are visible.

    Any link whose target doesn't start with docs/ or dev/ after removing
    the ../ prefix is out-of-tree — rewrite to an absolute GitHub URL.
    Also rewrites ../../user/ -> ../../../docs/ for dev docs (user docs synced to content/docs/).
    """

    def _resolve(m):
        prefix = m.group(1)
        path = m.group(2)
        has_anchor = ''
        if '#' in path:
            path, anchor = path.split('#', 1)
            has_anchor = f'#{anchor}'

        # ../../user/ -> ../../../docs/ (user docs synced to content/docs/)
        if prefix == '../../' and path.startswith('user/'):
            return f'](../../../docs/{path[5:]}{has_anchor})'
        if prefix == '../../../' and path.startswith('user/'):
            return f'](../../../docs/{path[5:]}{has_anchor})'

        # Links staying within the Zola content tree (docs/ or dev/)
        if path.startswith('docs/') or path.startswith('dev/'):
            return m.group(0)

        # Out-of-tree link -> rewrite to GitHub absolute URL
        if path.endswith('/') or not re.search(r'\.\w{1,10}$', path):
            base = 'https://github.com/dhruvasagar/lattice/tree/main'
        else:
            base = 'https://github.com/dhruvasagar/lattice/blob/main'
        return f']({base}/{path}{has_anchor})'

    return re.sub(
        r'\]\((\.\./\.\./(?:\.\./)?)([^)]+)\)',
        _resolve,
        content,
    )

def prefix_sibling_links(content):
    """Prefix bare and ./ sibling links with ../ for Zola directory-style URLs.

    Zola serves pages as directories: content/docs/foo.md -> /docs/foo/.
    A bare link [text](bar) from /docs/foo/ resolves to /docs/foo/bar,
    but should resolve to /docs/bar/.  Using ../bar/ fixes this.

    Must run AFTER strip_md_from_links (names have already lost .md).
    """
    def _prefix(m):
        name = m.group(1)
        anchor = m.group(2) or ''
        return f'](../{name}/{anchor})'

    # Match ](name) or ](./name) or ](name#anchor) — no slashes, no http
    return re.sub(
        r'\]\((?:\.\/)?([a-zA-Z0-9_-]+)(#[^)]*)?\)',
        _prefix,
        content,
    )

def write_doc(dst_path, title, body):
    os.makedirs(os.path.dirname(dst_path), exist_ok=True)
    with open(dst_path, 'w', encoding='utf-8') as fh:
        fh.write(f'+++\ntitle = "{title}"\n+++\n\n{body}')

def sync_dir(src_dir, dst_dir, link_mods=None):
    if not os.path.isdir(src_dir):
        print(f'  SKIP: {src_dir} not found')
        return
    os.makedirs(dst_dir, exist_ok=True)

    for f in sorted(glob.glob(os.path.join(src_dir, '*.md'))):
        b = os.path.basename(f)
        with open(f, encoding='utf-8') as fh:
            content = fh.read()

        title = make_title(content, b)
        body = strip_frontmatter(content)

        # Strip the first H1 heading matching the title, since the template
        # already renders <h1>{{ page.title }}</h1>.
        body = re.sub(rf'^# {re.escape(title)}\n?', '', body, count=1, flags=re.MULTILINE)

        # Apply link modifications
        if link_mods and 'rewrite_root' in link_mods:
            body = rewrite_root_links(body)
        if link_mods and 'strip_md' in link_mods:
            body = strip_md_from_links(body)
        if link_mods and 'prefix_sibling' in link_mods:
            body = prefix_sibling_links(body)
        if link_mods and 'help_topic' in link_mods:
            # Zola pages are emitted as directories:
            #   content/docs/themes.md -> /docs/themes/
            # Inside a page like /docs/modal-editing/, `./themes/` would incorrectly
            # resolve to /docs/modal-editing/themes/. Use `../` to anchor at /docs/.
            body = re.sub(
                r'\(help:([a-zA-Z0-9_-]+)(#[^)]*)?\)',
                lambda m: f'(../{m.group(1)}/{m.group(2) or ""})',
                body,
            )
            body = re.sub(r'\[([^\]]*)\]\((?:mode|event):[^)]+\)', r'`\1`', body)

        dst_file = os.path.join(dst_dir, b)
        write_doc(dst_file, title, body)
        print(f'  {os.path.relpath(dst_file, site_dir)}')

# --- Version data ---
print('Updating version data...')
write_version_data()

# --- User docs ---
print('Syncing user docs...')
sync_dir(
    os.path.join(repo_root, 'docs', 'user'),
    os.path.join(site_dir, 'content', 'docs'),
    link_mods={'rewrite_root', 'prefix_sibling', 'help_topic', 'strip_md'}
)

# Fix specific anchor: Zola slugifies "3. Non-tree-sitter" as "3-non-tree-sitter-languages"
ldst = os.path.join(site_dir, 'content', 'docs', 'languages.md')
if os.path.exists(ldst):
    with open(ldst, encoding='utf-8') as fh:
        c = fh.read()
    c = c.replace('(#3-non-tree-sitter)', '(#3-non-tree-sitter-languages)')
    with open(ldst, 'w', encoding='utf-8') as fh:
        fh.write(c)
    print('  [fixed anchor in languages.md]')

# --- Dev docs ---
print('\nSyncing dev docs...')
for subdir in ['guides', 'architecture', 'operations', 'audit', 'notes']:
    sync_dir(
        os.path.join(repo_root, 'docs', 'dev', subdir),
        os.path.join(site_dir, 'content', 'dev', subdir),
        link_mods={'rewrite_root', 'prefix_sibling', 'strip_md'}
    )

print('\nDone.')
