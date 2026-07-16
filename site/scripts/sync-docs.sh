#!/usr/bin/env python3
"""Sync user-facing docs from docs/user/*.md to site/content/docs/*.md.

Usage: from the site/ directory: python3 scripts/sync-docs.sh
(or equivalently: python3 scripts/sync-docs.sh)
"""
import os, re, glob, sys

script_dir = os.path.dirname(os.path.abspath(__file__))
site_dir = os.path.dirname(script_dir)
src_dir = os.path.join(site_dir, '..', 'docs', 'user')
dst_dir = os.path.join(site_dir, 'content', 'docs')

os.makedirs(dst_dir, exist_ok=True)

for f in sorted(glob.glob(os.path.join(src_dir, '*.md'))):
    b = os.path.basename(f)
    with open(f, encoding='utf-8') as fh:
        content = fh.read()

    m = re.search(r'^# (.+)$', content, re.MULTILINE)
    if m:
        title = m.group(1).strip()
    else:
        title = os.path.splitext(b)[0].replace('-', ' ').title()

    stripped = re.sub(r'^---\n.*?\n---\n', '', content, count=1, flags=re.DOTALL)

    converted = re.sub(r'\(help:([a-zA-Z0-9_-]+)\)', r'(./\1/)', stripped)
    converted = re.sub(r'\[([^\]]*)\]\((?:mode|event):[^)]+\)', r'`\1`', converted)
    # Fix anchors that Zola slugifies differently than the source
    converted = converted.replace('(#3-non-tree-sitter)', '(#3-non-tree-sitter-languages)')

    with open(os.path.join(dst_dir, b), 'w', encoding='utf-8') as fh:
        fh.write(f'+++\ntitle = "{title}"\n+++\n\n{converted}')
    print(f'  {b}')

print('OK')
