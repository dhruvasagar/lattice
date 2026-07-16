#!/usr/bin/env python3
"""Sync docs from docs/{user,dev}/ to site/content/ for Zola.

Strips YAML frontmatter, converts markdown links (.md extension → clean
URLs for Zola), and converts help:topic links (user docs only).
"""
import os, re, glob

script_dir = os.path.dirname(os.path.abspath(__file__))
site_dir = os.path.dirname(script_dir)
repo_root = os.path.join(site_dir, '..')

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

        # Apply link modifications
        if link_mods and 'strip_md' in link_mods:
            body = strip_md_from_links(body)
        if link_mods and 'help_topic' in link_mods:
            # Zola pages are emitted as directories:
            #   content/docs/themes.md -> /docs/themes/
            # Inside a page like /docs/modal-editing/, `./themes/` would incorrectly
            # resolve to /docs/modal-editing/themes/. Use `../` to anchor at /docs/.
            body = re.sub(r'\(help:([a-zA-Z0-9_-]+)\)', r'(../\1/)', body)
            body = re.sub(r'\[([^\]]*)\]\((?:mode|event):[^)]+\)', r'`\1`', body)

        dst_file = os.path.join(dst_dir, b)
        write_doc(dst_file, title, body)
        print(f'  {os.path.relpath(dst_file, site_dir)}')

# --- User docs ---
print('Syncing user docs...')
sync_dir(
    os.path.join(repo_root, 'docs', 'user'),
    os.path.join(site_dir, 'content', 'docs'),
    link_mods={'help_topic', 'strip_md'}
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
        link_mods={'strip_md'}
    )

print('\nDone.')
