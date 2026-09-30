#!/usr/bin/env python3
"""Sync docs from docs/{user,dev}/ into site/content/ for Zola.

docs/user/ is a FLAT corpus on purpose — it is the offline `:help` text
embedded in the binary, where `:help <topic>` resolves a bare topic name.
The website needs the opposite: a reading order, a guide/reference split,
and URLs that say where you are.

site/data/nav.toml is that navigation layer. This script reads it and:

  * routes each user doc to content/docs/<section>/<topic>.md
  * generates a landing page per section, tabulating its topics with the
    summary line each doc already carries in its frontmatter
  * resolves every `help:<topic>` cross-link (856 of them) to a Zola
    internal link, so a broken cross-reference fails the BUILD instead of
    shipping a dead link
  * hard-fails if docs/user/ and nav.toml disagree in either direction

Dev docs are synced flat into content/dev/<subdir>/ as before; only their
link rewriting shares code with the user path.
"""

import filecmp
import glob
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib

script_dir = os.path.dirname(os.path.abspath(__file__))
site_dir = os.path.dirname(script_dir)
repo_root = os.path.normpath(os.path.join(site_dir, '..'))

USER_SRC = os.path.join(repo_root, 'docs', 'user')
DEV_SRC = os.path.join(repo_root, 'docs', 'dev')
PLUGINS_DST = os.path.join(site_dir, 'content', 'plugins')
PLUGINS_SRC = os.path.join(repo_root, 'plugins')
PLUGIN_MANIFEST = os.path.join(site_dir, 'data', 'plugins.toml')
PLUGIN_BODIES = os.path.join(site_dir, 'data', 'plugin-pages')
XTASK_MAIN = os.path.join(repo_root, 'xtask', 'src', 'main.rs')
DOCS_DST = os.path.join(site_dir, 'content', 'docs')
DEV_DST = os.path.join(site_dir, 'content', 'dev')
DEV_SUBDIRS = ['guides', 'architecture', 'operations', 'audit', 'notes', 'reference']

# The plugin-API reference is GENERATED (crates/lattice-plugin-api, AD.2): an
# index, one page per seam, and a JSON export. Its correctness is guarded by
# that crate's tests, not by dev-nav.toml — listing 30-odd generated pages by
# hand would be a second copy of the WIT's interface list, and the first thing
# to drift. dev-nav.toml names the SECTION (`generated = "plugin-api"`); this
# script publishes the index as the section's landing page and each seam as a
# page in its sidebar.
PLUGIN_API_INDEX = 'reference/plugin-api'
PLUGIN_API_SEAMS = os.path.join(DEV_SRC, 'reference', 'plugin-api')
PLUGIN_API_JSON = os.path.join(DEV_SRC, 'reference', 'plugin-api.json')
STATIC_DST = os.path.join(site_dir, 'static')

# Launch screenshots. `assets/media/screenshots/` is the source of truth (what
# README.md links); `site/static/media/` is what the site serves and is
# GENERATED from it by sync_media(), so the two cannot drift — the failure that
# shipped the old images after a screenshot update. The publish spec is
# docs/media/screenshot-ideas.md: 1920 px wide, PNG, under 400 KB.
SHOTS_SRC = os.path.join(repo_root, 'assets', 'media', 'screenshots')
MEDIA_DST = os.path.join(site_dir, 'static', 'media')
SHOT_MAX_WIDTH = 1920
SHOT_MAX_BYTES = 400 * 1024
SHOT_COLORS = 256

GH_BLOB = 'https://github.com/dhruvasagar/lattice/blob/main'
GH_TREE = 'https://github.com/dhruvasagar/lattice/tree/main'

RE_VERSION = re.compile(r'^version\s*=\s*"([^"]+)"', re.MULTILINE)
RE_SUMMARY = re.compile(r'^summary:\s*(.*?)\s*$', re.MULTILINE)


def die(msg):
    sys.exit(f'sync-docs: ERROR: {msg}')


# --------------------------------------------------------------------------
# Version data for the header/footer badge
# --------------------------------------------------------------------------

def write_version_data():
    with open(os.path.join(repo_root, 'Cargo.toml'), encoding='utf-8') as fh:
        m = RE_VERSION.search(fh.read())
    version = m.group(1) if m else '0.0.0'
    dst = os.path.join(site_dir, 'data', 'version.toml')
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    with open(dst, 'w', encoding='utf-8') as fh:
        fh.write(f'latest = "{version}"\n')
    print(f'  version: {version}')


# --------------------------------------------------------------------------
# Navigation manifest
# --------------------------------------------------------------------------

def load_nav():
    """Return (sections, topic_section, labels).

    topic_section maps a bare topic name -> (section slug, group title).
    The slug is what every link resolver needs to turn `help:folding` into a
    real path; the group title is what the reference sidebar nests under.
    """
    path = os.path.join(site_dir, 'data', 'nav.toml')
    with open(path, 'rb') as fh:
        nav = tomllib.load(fh)

    sections = nav.get('section', [])
    labels = nav.get('labels', {})
    topic_section = {}

    for sec in sections:
        slug = sec['slug']
        for group in sec.get('group', []):
            for topic in group['docs']:
                if topic in topic_section:
                    die(f'{topic!r} listed twice in nav.toml')
                topic_section[topic] = (slug, group.get('title', ''))

    return sections, topic_section, labels


def validate_nav(topic_section, labels):
    """nav.toml and docs/user/ must agree exactly, in both directions."""
    on_disk = {
        os.path.basename(f)[:-3]
        for f in glob.glob(os.path.join(USER_SRC, '*.md'))
    } - {'README'}

    listed = set(topic_section)
    missing = sorted(on_disk - listed)
    phantom = sorted(listed - on_disk)
    bad_labels = sorted(set(labels) - on_disk)

    problems = []
    if missing:
        problems.append(
            'docs/user/ topics absent from site/data/nav.toml — add them to a '
            'section so they get a place in the nav:\n    '
            + '\n    '.join(missing)
        )
    if phantom:
        problems.append(
            'site/data/nav.toml lists topics with no docs/user/ file — the doc '
            'was renamed or deleted:\n    ' + '\n    '.join(phantom)
        )
    if bad_labels:
        problems.append(
            'site/data/nav.toml [labels] name unknown topics:\n    '
            + '\n    '.join(bad_labels)
        )
    if problems:
        die('\n\n  '.join(problems))


# --------------------------------------------------------------------------
# Frontmatter / body helpers
# --------------------------------------------------------------------------

def strip_frontmatter(content):
    return re.sub(r'^---\n.*?\n---\n', '', content, count=1, flags=re.DOTALL)


def read_summary(content):
    m = RE_SUMMARY.search(content.split('\n---', 1)[0]) if content.startswith('---') else None
    if not m:
        return ''
    return m.group(1).strip().strip('"').replace('--', '—')


def make_title(content, topic):
    m = re.search(r'^# (.+)$', content, re.MULTILINE)
    if m:
        return m.group(1).strip()
    return topic.replace('-', ' ').capitalize()


def toml_escape(value):
    return value.replace('\\', '\\\\').replace('"', '\\"')


# --------------------------------------------------------------------------
# Link rewriting
# --------------------------------------------------------------------------

def split_anchor(path):
    if '#' in path:
        head, anchor = path.split('#', 1)
        return head, f'#{anchor}'
    return path, ''


def resolve_help_links(body, topic_section):
    """`help:folding` -> `@/docs/code/folding.md`.

    Zola validates internal links at build time, so a cross-reference to a
    topic that has been renamed or dropped breaks the deploy loudly instead
    of shipping as a 404.
    """
    def _sub(m):
        topic, anchor = m.group(1), m.group(2) or ''
        placement = topic_section.get(topic)
        if placement is None:
            # Not a real topic (a `_planned_` placeholder, or a typo). Leave
            # the link text as plain code rather than emitting a dead link.
            return f'`{topic}`'
        return f'](@/docs/{placement[0]}/{topic}.md{anchor})'

    body = re.sub(r'\]\(help:([a-zA-Z0-9_-]+)(#[^)]*)?\)', _sub, body)
    # `mode:` / `event:` targets have no page of their own — render as code.
    return re.sub(r'\[([^\]]*)\]\((?:mode|event):[^)]+\)', r'`\1`', body)


def make_relative_resolver(topic_section, dev_pages, page_section):
    """Resolve `../dev/...`, `../../user/...` and friends.

    Anything that lands inside the Zola content tree becomes an `@/` internal
    link; anything outside it (slice plans, wit/, source files) becomes an
    absolute GitHub URL so it still goes somewhere useful.

    `page_section` is load-bearing for the dev branch and its absence was a
    build-breaking bug: a dev page is PUBLISHED at `@/dev/<section-slug>/`,
    not at `@/dev/<source-subdir>/`, so emitting the source path produced a
    broken relative link for every user->dev reference. Zola reports one such
    link and stops, which is why it read as "one bad page" rather than
    fourteen.
    """
    def _resolve(m):
        raw = m.group(1)
        path, anchor = split_anchor(raw)

        # Normalise away the leading ../ hops — every doc tree we sync from is
        # addressed by its repo-relative path below docs/.
        stripped = re.sub(r'^(?:\.\./)+', '', path)

        if stripped.startswith('user/'):
            topic = os.path.basename(stripped)[:-3] if stripped.endswith('.md') else os.path.basename(stripped)
            placement = topic_section.get(topic)
            if placement:
                return f'](@/docs/{placement[0]}/{topic}.md{anchor})'

        if stripped.startswith('dev/'):
            rel = stripped[4:]
            key = rel[:-3] if rel.endswith('.md') else rel
            # The generated plugin-API reference is its own section (see
            # sync_plugin_api_reference): the index is the landing page.
            if key == PLUGIN_API_INDEX:
                return f'](@/dev/plugin-api/_index.md{anchor})'
            if key.startswith(PLUGIN_API_INDEX + '/'):
                return f'](@/dev/plugin-api/{key.rsplit("/", 1)[-1]}.md{anchor})'
            if key in dev_pages:
                # The SECTION the manifest put it in — its real URL.
                slug = page_section[key][0]
                stem = key.rsplit('/', 1)[-1]
                return f'](@/dev/{slug}/{stem}.md{anchor})'

        # Out of the content tree -> GitHub.
        base = GH_TREE if (path.endswith('/') or not re.search(r'\.\w{1,10}$', path)) else GH_BLOB
        return f']({base}/{stripped}{anchor})'

    return _resolve


RE_RELATIVE = re.compile(r'\]\(((?:\.\./)+[^)]+)\)')


def rewrite_links(body, topic_section, dev_pages, page_section, is_user_doc):
    if is_user_doc:
        body = resolve_help_links(body, topic_section)
    return RE_RELATIVE.sub(
        make_relative_resolver(topic_section, dev_pages, page_section), body
    )


# --------------------------------------------------------------------------
# Writers
# --------------------------------------------------------------------------

def reset_generated_dirs(sections, dev_sections):
    """Remove previously generated trees so renamed/moved docs cannot linger.

    This matters more than it looks: without it, a topic that changes section
    keeps a stale page at its old URL and Zola happily builds both.
    """
    for sec in sections:
        shutil.rmtree(os.path.join(DOCS_DST, sec['slug']), ignore_errors=True)
    # Flat pages from the pre-restructure layout.
    for stale in glob.glob(os.path.join(DOCS_DST, '*.md')):
        if os.path.basename(stale) != '_index.md':
            os.remove(stale)
    # The pre-manifest layout mirrored docs/dev/'s subdirs; the manifest
    # layout is by section. Remove BOTH so a re-sync after this change does
    # not leave every dev page published at two URLs.
    for sub in DEV_SUBDIRS:
        shutil.rmtree(os.path.join(DEV_DST, sub), ignore_errors=True)
    for sec in dev_sections:
        shutil.rmtree(os.path.join(DEV_DST, sec['slug']), ignore_errors=True)


# --------------------------------------------------------------------------
# Sync
# --------------------------------------------------------------------------

def sync_user_docs(topic_section, labels, dev_pages, page_section):
    meta = {}      # topic -> (title, summary)
    headings = {}  # topic -> [h2/h3 text], fed to the search index
    # Sidebar/landing order is manifest order; Zola sorts on `weight`.
    weights = {topic: i for i, topic in enumerate(topic_section)}

    for topic, (slug, group_title) in topic_section.items():
        src = os.path.join(USER_SRC, f'{topic}.md')
        with open(src, encoding='utf-8') as fh:
            raw = fh.read()

        title = make_title(raw, topic)
        summary = read_summary(raw)
        body = strip_frontmatter(raw)
        # The template renders <h1>{{ page.title }}</h1>; drop the duplicate.
        body = re.sub(rf'^# {re.escape(title)}\n?', '', body, count=1, flags=re.MULTILINE)
        body = rewrite_links(body, topic_section, dev_pages, page_section, is_user_doc=True)

        meta[topic] = (title, summary)
        headings[topic] = [
            h.strip().strip('`') for h in re.findall(r'^#{2,3} +(.+)$', body, re.MULTILINE)
        ]

        lines = ['+++', f'title = "{toml_escape(title)}"']
        if summary:
            lines.append(f'description = "{toml_escape(summary)}"')
        lines.extend([
            f'weight = {weights[topic]}',
            '[extra]',
            f'nav_label = "{toml_escape(labels.get(topic, title))}"',
            f'section_slug = "{slug}"',
            f'nav_group = "{toml_escape(group_title)}"',
            '+++',
            '',
        ])

        page = os.path.join(DOCS_DST, slug, f'{topic}.md')
        os.makedirs(os.path.dirname(page), exist_ok=True)
        with open(page, 'w', encoding='utf-8') as fh:
            fh.write('\n'.join(lines) + '\n' + body)

    return meta, headings


def section_landing_body(sec, meta, labels):
    """Table(s) of the section's topics, using each doc's own summary line."""
    out = []
    groups = sec.get('group', [])
    multi = len(groups) > 1

    for group in groups:
        if multi:
            out.append(f"## {group['title']}")
            out.append('')
            if group.get('description'):
                out.append(group['description'])
                out.append('')
        out.append('| Topic | What it covers |')
        out.append('|---|---|')
        for topic in group['docs']:
            title, summary = meta[topic]
            label = labels.get(topic, title)
            cell = summary or ''
            # Table cells cannot contain raw pipes.
            cell = cell.replace('|', '\\|')
            out.append(f'| [{label}](@/docs/{sec["slug"]}/{topic}.md) | {cell} |')
        out.append('')

    return '\n'.join(out)


def write_section_landings(sections, meta, labels):
    for i, sec in enumerate(sections):
        path = os.path.join(DOCS_DST, sec['slug'], '_index.md')
        body = section_landing_body(sec, meta, labels)
        lines = [
            '+++',
            f'title = "{toml_escape(sec["title"])}"',
            f'description = "{toml_escape(sec["description"])}"',
            f'weight = {i + 1}',
            'sort_by = "weight"',
            'template = "docs-section.html"',
            'page_template = "docs-page.html"',
            '[extra]',
            f'section_slug = "{sec["slug"]}"',
            '+++',
            '',
        ]
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, 'w', encoding='utf-8') as fh:
            fh.write('\n'.join(lines) + '\n' + sec['description'] + '\n\n' + body)
        print(f'  docs/{sec["slug"]}/ ({sum(len(g["docs"]) for g in sec.get("group", []))} topics)')


def write_search_index(sections, meta, labels, headings):
    """A tiny title/summary/heading index for the docs search box.

    Deliberately not Zola's `build_search_index`: that emits an elasticlunr
    index which needs the elasticlunr runtime vendored into static/, and a
    strict CSP-friendly site should not pull a library to filter 113 rows.
    Matching titles, summaries and section headings covers the question the
    search box actually answers — "which page do I want?" — in ~40 lines of
    dependency-free JS. Full-text body search is the deliberate omission.
    """
    entries = []
    for sec in sections:
        for group in sec.get('group', []):
            for topic in group['docs']:
                title, summary = meta[topic]
                entries.append({
                    'l': labels.get(topic, title),
                    't': title,
                    'u': f'docs/{sec["slug"]}/{topic}/',
                    's': sec['title'],
                    'g': group.get('title', ''),
                    'd': summary,
                    'h': ' '.join(headings.get(topic, [])),
                })

    dst = os.path.join(site_dir, 'static', 'docs-search.json')
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    with open(dst, 'w', encoding='utf-8') as fh:
        json.dump(entries, fh, separators=(',', ':'), ensure_ascii=False)
    print(f'  static/docs-search.json ({len(entries)} entries)')


def load_dev_nav():
    """Return (sections, page_section, labels) for the developer docs.

    `page_section` maps `<subdir>/<stem>` -> (section slug, group title). A
    section may mix subdirs on purpose: a newcomer asking "how do plugins
    work" wants the guide, the design fragment and the audit together, and
    does not care that they live in three directories.
    """
    path = os.path.join(site_dir, 'data', 'dev-nav.toml')
    with open(path, 'rb') as fh:
        nav = tomllib.load(fh)

    sections = nav.get('section', [])
    labels = nav.get('labels', {})
    page_section = {}

    for sec in sections:
        slug = sec['slug']
        for group in sec.get('group', []):
            for page in group['docs']:
                if page in page_section:
                    die(f'{page!r} listed twice in dev-nav.toml')
                page_section[page] = (slug, group.get('title', ''))

    return sections, page_section, labels


def validate_dev_nav(page_section, labels, dev_pages):
    """dev-nav.toml and docs/dev/ must agree exactly, in both directions.

    This is the property the manifest exists for. A grouping that merely
    *describes* the docs decays the first time someone adds one; a grouping
    the build refuses to proceed without cannot.
    """
    listed = set(page_section)
    missing = sorted(dev_pages - listed)
    phantom = sorted(listed - dev_pages)
    bad_labels = sorted(set(labels) - dev_pages)

    problems = []
    if missing:
        problems.append(
            'docs/dev/ pages absent from site/data/dev-nav.toml — add each to '
            'the section a reader would look for it in:\n    '
            + '\n    '.join(missing)
        )
    if phantom:
        problems.append(
            'site/data/dev-nav.toml lists pages with no docs/dev/ file — the '
            'doc was renamed, deleted, or archived:\n    '
            + '\n    '.join(phantom)
        )
    if bad_labels:
        problems.append(
            'site/data/dev-nav.toml [labels] name unknown pages:\n    '
            + '\n    '.join(bad_labels)
        )
    if problems:
        die('\n\n  '.join(problems))


def collect_dev_pages():
    """Hand-written dev pages. The generated plugin-API index is excluded —
    it is published by `sync_plugin_api_reference`, not through dev-nav."""
    pages = set()
    for sub in DEV_SUBDIRS:
        for f in glob.glob(os.path.join(DEV_SRC, sub, '*.md')):
            pages.add(f'{sub}/{os.path.basename(f)[:-3]}')
    pages.discard(PLUGIN_API_INDEX)
    return pages


def plugin_api_seams():
    """Generated page stems in sidebar order: `worlds` (what a plugin
    targets, and its entry points) first, then the seams alphabetically."""
    return sorted(
        (os.path.basename(f)[:-3]
         for f in glob.glob(os.path.join(PLUGIN_API_SEAMS, '*.md'))),
        key=lambda stem: (stem != 'worlds', stem),
    )


def sync_plugin_api_reference(slug, weight, description, topic_section,
                              dev_pages, page_section):
    """Publish the generated plugin-API reference as its own dev section.

    The index becomes the section's landing page; each seam becomes a page, so
    the sidebar lists every seam. Links between the generated files are
    same-directory relative (`types.md#record-effect`, `plugin-api/buffer.md`),
    which is right on GitHub and wrong under Zola's pretty URLs — they are
    rewritten to `@/` internal links here, which also makes Zola VALIDATE each
    one, anchor included, at build time.
    """
    index_src = os.path.join(DEV_SRC, PLUGIN_API_INDEX + '.md')
    if not os.path.isfile(index_src):
        die(f'{index_src} is missing — run '
            'UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api')
    seams = plugin_api_seams()
    if not seams:
        die(f'{PLUGIN_API_SEAMS} has no seam pages')

    def internal(body):
        body = re.sub(r'\]\(plugin-api/([a-z0-9-]+)\.md(#[^)]*)?\)',
                      lambda m: f'](@/dev/{slug}/{m.group(1)}.md{m.group(2) or ""})',
                      body)
        body = re.sub(r'\]\(([a-z0-9-]+)\.md(#[^)]*)?\)',
                      lambda m: f'](@/dev/{slug}/{m.group(1)}.md{m.group(2) or ""})'
                      if m.group(1) in seams else m.group(0),
                      body)
        return rewrite_links(body, topic_section, dev_pages, page_section,
                             is_user_doc=False)

    d = os.path.join(DEV_DST, slug)
    os.makedirs(d, exist_ok=True)

    with open(index_src, encoding='utf-8') as fh:
        body = strip_frontmatter(fh.read())
    title = make_title(body, 'plugin-api')
    body = re.sub(rf'^# {re.escape(title)}\n?', '', body, count=1, flags=re.MULTILINE)
    with open(os.path.join(d, '_index.md'), 'w', encoding='utf-8') as fh:
        fh.write(
            f'+++\ntitle = "{toml_escape(title)}"\n'
            f'description = "{toml_escape(description)}"\n'
            f'weight = {weight}\nsort_by = "weight"\n+++\n\n{internal(body)}'
        )

    for i, seam in enumerate(seams):
        with open(os.path.join(PLUGIN_API_SEAMS, seam + '.md'), encoding='utf-8') as fh:
            body = fh.read()
        body = re.sub(r'^# `[^`]+`\n?', '', body, count=1, flags=re.MULTILINE)
        with open(os.path.join(d, seam + '.md'), 'w', encoding='utf-8') as fh:
            fh.write(f'+++\ntitle = "{toml_escape(seam)}"\nweight = {i}\n+++\n\n'
                     f'{internal(body)}')

    # The machine-readable form, at a stable site-root URL.
    shutil.copyfile(PLUGIN_API_JSON, os.path.join(STATIC_DST, 'plugin-api.json'))
    print(f'  dev/{slug}/ (index + {len(seams)} seam pages, plugin-api.json)')


def sync_dev_docs(topic_section, dev_pages, dev_sections, page_section, dev_labels):
    """Write each dev page into its dev-nav SECTION rather than its subdir.

    The source tree stays organised by kind (architecture / guides /
    operations / ...) because that is what contributors edit; the site is
    organised by question, because that is what a newcomer browses. The
    manifest is the mapping between the two, so neither has to compromise.
    """
    # Section landing pages, in manifest order — `weight` is what gives the
    # sidebar a reading order instead of an alphabet.
    for i, sec in enumerate(dev_sections, start=1):
        generated = sec.get('generated')
        if generated == 'plugin-api':
            sync_plugin_api_reference(sec['slug'], i * 10, sec.get('description', ''),
                                      topic_section, dev_pages, page_section)
            continue
        if generated:
            die(f'dev-nav.toml section {sec["slug"]!r}: unknown generator {generated!r}')
        d = os.path.join(DEV_DST, sec['slug'])
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, '_index.md'), 'w', encoding='utf-8') as fh:
            fh.write(
                f'+++\ntitle = "{toml_escape(sec["title"])}"\n'
                f'description = "{toml_escape(sec.get("description", ""))}"\n'
                f'weight = {i * 10}\nsort_by = "weight"\n+++\n'
            )

    # Per-section counters so each page's weight preserves manifest order.
    order = {}
    for sec in dev_sections:
        for group in sec.get('group', []):
            for page in group['docs']:
                order[page] = len(order)

    counts = {}
    for sub in DEV_SUBDIRS:
        src_dir = os.path.join(DEV_SRC, sub)
        if not os.path.isdir(src_dir):
            print(f'  SKIP: {src_dir} not found')
            continue
        for f in sorted(glob.glob(os.path.join(src_dir, '*.md'))):
            name = os.path.basename(f)
            key = f'{sub}/{name[:-3]}'
            if key == PLUGIN_API_INDEX:
                continue  # published by sync_plugin_api_reference
            slug, _group = page_section[key]
            with open(f, encoding='utf-8') as fh:
                raw = fh.read()
            title = make_title(raw, name[:-3])
            body = strip_frontmatter(raw)
            body = re.sub(rf'^# {re.escape(title)}\n?', '', body, count=1, flags=re.MULTILINE)
            body = rewrite_links(
                body, topic_section, dev_pages, page_section, is_user_doc=False
            )
            # The sidebar label overrides the H1 only where the manifest says
            # so; the page's own <h1> keeps the doc's real title.
            nav_title = dev_labels.get(key, title)
            dst = os.path.join(DEV_DST, slug, name)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with open(dst, 'w', encoding='utf-8') as fh:
                fh.write(
                    f'+++\ntitle = "{toml_escape(nav_title)}"\n'
                    f'weight = {order[key]}\n+++\n\n{body}'
                )
            counts[slug] = counts.get(slug, 0) + 1

    for sec in dev_sections:
        if not sec.get('generated'):  # generated sections report themselves
            print(f'  dev/{sec["slug"]}/ ({counts.get(sec["slug"], 0)} pages)')



def core_plugin_ids():
    """The plugins that ship with lattice, read from xtask's CORE_PLUGINS."""
    m = re.search(
        r'const\s+CORE_PLUGINS\s*:\s*&\[&str\]\s*=\s*&\[(.*?)\];',
        open(XTASK_MAIN, encoding='utf-8').read(),
        re.S,
    )
    if not m:
        die(f'could not find CORE_PLUGINS in {os.path.relpath(XTASK_MAIN, repo_root)} '
            '— it was renamed or reshaped, and this guard is now blind')
    return set(re.findall(r'"([^"]+)"', m.group(1)))


def plugin_at_a_glance(pid):
    """The "Getting it" block, read out of the plugin's OWN manifest.

    Enable gate, seams and capability grants are all declared in
    plugins/<id>/plugin.toml. Deriving them means the page cannot claim a
    plugin asks for a capability it no longer asks for.
    """
    manifest = os.path.join(PLUGINS_SRC, pid, 'plugin.toml')
    if not os.path.isfile(manifest):
        die(f'no plugin.toml at plugins/{pid}/ — site/data/plugins.toml lists '
            f'{pid!r} as bundled, but there is no such plugin in this tree')
    with open(manifest, 'rb') as fh:
        man = tomllib.load(fh)

    modes = man.get('default_modes') or (
        [man['default_mode']] if 'default_mode' in man else []
    )
    out = ['## Getting it', '']
    out.append('It ships with lattice and is enabled by default — nothing to '
               'install, no toolchain, works offline.')
    out.append('')
    if modes:
        out.append(f'It contributes the minor mode `{modes[0]}`, and the loader '
                   f'registers `{pid}.enabled` as the switch for it:')
        out.append('')
        out.append('```')
        out.append(f':set {pid}.enabled=false')
        out.append('```')
        out.append('')
    rows = []
    if man.get('provides'):
        rows.append(('Seams it plugs into', ', '.join(f'`{x}`' for x in man['provides'])))
    grants = list(man.get('capabilities', [])) + list(man.get('editor_capabilities', []))
    rows.append((
        'Capabilities it asks for',
        ', '.join(f'`{x}`' for x in grants) if grants else 'none',
    ))
    out.append('| | |')
    out.append('|---|---|')
    for label, value in rows:
        out.append(f'| **{label}** | {value} |')
    out.append('')
    return '\n'.join(out)


def sync_plugin_pages(topic_section):
    """Generate site/content/plugins/<id>.md for every plugin in the manifest.

    A bundled plugin's page body IS `plugins/<id>/doc/<id>.md` — the same
    markdown `:help <id>` serves from inside the component, so the website
    cannot drift from the manual the editor ships. That is the whole point:
    these manuals are the only place the keys, commands and options of a
    bundled plugin are written down, and before this they were invisible to
    anyone who had not already installed lattice.

    External plugins live in someone else's repository, so there is nothing
    in-tree to sync; their body is authored under site/data/plugin-pages/.
    """
    with open(PLUGIN_MANIFEST, 'rb') as fh:
        plugins = tomllib.load(fh)['plugin']

    bundled = {p['id'] for p in plugins if p['kind'] == 'bundled'}
    in_code = core_plugin_ids()
    missing = sorted(in_code - bundled)
    phantom = sorted(bundled - in_code)
    problems = []
    if missing:
        problems.append(
            'CORE_PLUGINS ships plugins absent from site/data/plugins.toml — a '
            'bundled plugin nobody can read about:\n    ' + '\n    '.join(missing)
            + '\n  Add an entry, and add its generated page to .gitignore.'
        )
    if phantom:
        problems.append(
            'site/data/plugins.toml marks these `kind = "bundled"` but '
            'CORE_PLUGINS does not ship them — the page claims lattice bundles '
            'something it does not:\n    ' + '\n    '.join(phantom)
        )
    if problems:
        die('\n\n  '.join(problems))

    # Everything here is generated; a renamed plugin must not keep a stale page.
    for stale in glob.glob(os.path.join(PLUGINS_DST, '*.md')):
        if os.path.basename(stale) != '_index.md':
            os.remove(stale)

    for p in plugins:
        pid, kind = p['id'], p['kind']
        if kind not in ('bundled', 'external', 'community'):
            die(f'site/data/plugins.toml: {pid!r} has unknown kind {kind!r}')

        if p.get('body') == 'authored':
            src = os.path.join(PLUGIN_BODIES, f'{pid}.md')
            if not os.path.isfile(src):
                die(f'{pid!r} is body = "authored" but site/data/plugin-pages/'
                    f'{pid}.md does not exist')
            body = open(src, encoding='utf-8').read()
            glance = ''
        else:
            src = os.path.join(PLUGINS_SRC, pid, 'doc', f'{pid}.md')
            if not os.path.isfile(src):
                die(f'no manual at plugins/{pid}/doc/{pid}.md — a bundled plugin '
                    'must ship the help text its page is built from')
            body = open(src, encoding='utf-8').read()
            # page.html renders the title as <h1>; drop the manual's own H1 so
            # the page does not carry two.
            body = re.sub(r'\A#[^\n]*\n+', '', body)
            glance = plugin_at_a_glance(pid)

        body = resolve_help_links(body, topic_section)

        # The glance block goes AFTER the manual's opening prose and before its
        # first section, so the page reads lead -> how to get it -> the manual.
        # Leading with "Getting it" answers a question the reader has not asked
        # yet.
        if glance:
            split = body.find('\n## ')
            if split == -1:
                body = body.rstrip() + '\n\n' + glance
            else:
                body = body[:split].rstrip() + '\n\n' + glance + body[split:]
            glance = ''

        extra = [f'kind = "{kind}"']
        if p.get('repo'):
            extra.append(f'repo = "{p["repo"]}"')

        with open(os.path.join(PLUGINS_DST, f'{pid}.md'), 'w', encoding='utf-8') as fh:
            fh.write(
                '+++\n'
                f'title = "{toml_escape(pid)}"\n'
                f'description = "{toml_escape(p["description"])}"\n'
                f'weight = {p["weight"]}\n'
                '[extra]\n' + '\n'.join(extra) + '\n'
                '+++\n\n'
                + (glance + '\n' if glance else '')
                + body.rstrip() + '\n'
            )

    print(f'  {len(plugins)} plugin pages ({len(bundled)} bundled, matching CORE_PLUGINS)')


# --------------------------------------------------------------------------
# Media: convert launch screenshots to the publish spec, generate served copies
# --------------------------------------------------------------------------

def png_width(path):
    """Width of a PNG from its IHDR header, or None if it isn't a PNG.

    Dependency-free (no `identify` call) — the "does this shot need
    processing?" decision reads the header directly; ImageMagick is only
    invoked when a conversion is actually required.
    """
    with open(path, 'rb') as fh:
        head = fh.read(24)
    if head[:8] != b'\x89PNG\r\n\x1a\n':
        return None
    return int.from_bytes(head[16:20], 'big')


def imagemagick_bin():
    """ImageMagick's entry point: `magick` (v7) or `convert` (v6), or None."""
    return shutil.which('magick') or shutil.which('convert')


def sync_media():
    """Convert launch screenshots to the publish spec and generate the served
    copies, so a fresh capture only has to be dropped into
    assets/media/screenshots/ and re-synced.

    For each source shot: if it is wider than SHOT_MAX_WIDTH or heavier than
    SHOT_MAX_BYTES it is normalised IN PLACE (downscale to 1920 px wide,
    quantise to 256 colours, strip metadata) — the same pass that took the
    reference hero to 1920 px / <400 KB. An already-compliant shot is left
    untouched, so re-running does not churn git. Every shot is then mirrored
    into site/static/media/ (generated, gitignored), and served copies whose
    source is gone are removed — so the two dirs can never drift.

    ImageMagick is required only when a shot actually needs converting; an
    all-compliant tree (the committed state) syncs with a plain copy and needs
    no toolchain, which is why CI does not have to install one.
    """
    if not os.path.isdir(SHOTS_SRC):
        return
    os.makedirs(MEDIA_DST, exist_ok=True)
    magick = imagemagick_bin()
    shots = sorted(glob.glob(os.path.join(SHOTS_SRC, '*.png')))
    converted = mirrored = removed = 0

    for src in shots:
        name = os.path.basename(src)
        width = png_width(src)
        too_wide = width is not None and width > SHOT_MAX_WIDTH
        oversize = os.path.getsize(src) > SHOT_MAX_BYTES
        if too_wide or oversize:
            if not magick:
                die(f'{name} is over the publish spec (>{SHOT_MAX_WIDTH}px wide '
                    f'or >{SHOT_MAX_BYTES // 1024} KB) but ImageMagick is not '
                    'installed — install it (`magick`/`convert`) or pre-size the '
                    'capture per docs/media/screenshot-ideas.md')
            tmp = f'{src}.tmp.png'
            cmd = [magick, src, '-strip']
            if too_wide:
                cmd += ['-resize', f'{SHOT_MAX_WIDTH}x']
            cmd += ['-colors', str(SHOT_COLORS),
                    '-define', 'png:compression-level=9', tmp]
            try:
                subprocess.run(cmd, check=True, capture_output=True)
            except subprocess.CalledProcessError as exc:
                if os.path.exists(tmp):
                    os.remove(tmp)
                detail = exc.stderr.decode('utf-8', 'replace').strip()
                die(f'converting {name} failed: {detail}')
            os.replace(tmp, src)
            converted += 1

        dst = os.path.join(MEDIA_DST, name)
        if not (os.path.isfile(dst) and filecmp.cmp(src, dst, shallow=False)):
            shutil.copy2(src, dst)
            mirrored += 1

    for served in glob.glob(os.path.join(MEDIA_DST, '*.png')):
        if not os.path.isfile(os.path.join(SHOTS_SRC, os.path.basename(served))):
            os.remove(served)
            removed += 1

    note = f'{len(shots)} shots ({converted} converted, {mirrored} synced'
    if removed:
        note += f', {removed} stale removed'
    print(f'  media: {note})')


def main():
    print('Reading navigation manifest...')
    sections, topic_section, labels = load_nav()
    validate_nav(topic_section, labels)
    guides = sum(
        len(g['docs']) for s in sections if s['slug'] != 'reference'
        for g in s.get('group', [])
    )
    print(f'  {len(topic_section)} topics across {len(sections)} sections '
          f'({guides} guides, {len(topic_section) - guides} reference)')

    print('Updating version data...')
    write_version_data()

    dev_pages = collect_dev_pages()
    dev_sections, page_section, dev_labels = load_dev_nav()
    validate_dev_nav(page_section, dev_labels, dev_pages)
    print(f'  {len(page_section)} dev pages across {len(dev_sections)} sections')
    reset_generated_dirs(sections, dev_sections)

    print('Syncing plugin pages...')
    sync_plugin_pages(topic_section)

    print('Syncing user docs...')
    meta, headings = sync_user_docs(topic_section, labels, dev_pages, page_section)
    write_section_landings(sections, meta, labels)
    write_search_index(sections, meta, labels, headings)

    print('Syncing dev docs...')
    sync_dev_docs(topic_section, dev_pages, dev_sections, page_section, dev_labels)

    print('Syncing media...')
    sync_media()

    print('\nDone.')


if __name__ == '__main__':
    main()
