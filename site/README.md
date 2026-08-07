# The Lattice website

Zola site for <https://dhruvasagar.github.io/lattice/>. Landing page, user
docs, and dev docs.

## Building

`site/content/` is **generated**. Docs live in `docs/user/` and `docs/dev/`;
the sync script copies them in, rewrites their links, and builds the navigation.
Always sync before building:

```sh
python3 site/scripts/sync-docs.sh
cd site && zola serve      # or: zola build
```

Generated content is gitignored — only the hand-written `_index.md` files,
templates, sass, and `data/nav.toml` are tracked. Skipping the sync leaves you
with no pages and a build error from the templates, not a silently empty site.

## Why the docs are structured twice

`docs/user/*.md` is a **flat** corpus on purpose: it is the offline `:help`
text embedded in the binary, where `:help <topic>` resolves a bare topic name.
Flat is right for `:help` and wrong for a website, which needs a reading order
and progressive disclosure.

`site/data/nav.toml` is the navigation layer over that flat corpus. It decides
three things and nothing else:

| It decides | Which produces |
|---|---|
| which section a topic belongs to | its URL, `/docs/<section>/<topic>/` |
| what order topics appear in | sidebar and section-landing order |
| what the sidebar calls it | `[labels]`, only where the doc's own H1 reads badly out of context |

Two tiers, and the distinction is load-bearing. **Guide** sections hold topics
you read to learn a capability; they carry the main sidebar. **Reference** holds
per-surface keymap tables — one buffer kind, one minor mode, one language
grammar. 74 of the 113 topics live there, and keeping them out of the main
sidebar is what makes the nav navigable.

## Adding a doc

1. Write `docs/user/<topic>.md` with a `summary:` line in its frontmatter —
   that summary becomes the page description, the section-landing table cell,
   and the search-result snippet.
2. Add `"<topic>"` to the right section in `site/data/nav.toml`.
3. Register it with the in-editor `:help` topic registry as usual.

Skipping step 2 **fails the build**, by design:

```
sync-docs: ERROR: docs/user/ topics absent from site/data/nav.toml — add them
to a section so they get a place in the nav:
    my-new-mode
```

The reverse is caught too: a nav entry naming a deleted or renamed doc fails
the same way. This is deliberate — the hand-maintained topic table that used to
live in `content/docs/_index.md` silently drifted to listing 29 of 113 topics,
which is exactly the failure this replaces.

## Cross-references

Inside `docs/user/`, link topics with `help:<topic>` — the same form `:help`
uses in the editor:

```markdown
See [folding](help:folding) and [fold operators](help:folding#operators).
```

The sync resolves these to Zola internal links, so **`zola build` fails on a
broken cross-reference or a broken anchor** instead of shipping a dead link.
Links to `../dev/...` resolve into the dev tree the same way; anything outside
the synced trees (slice plans, `wit/`, source files) is rewritten to an
absolute GitHub URL.

## Search

`sync-docs.sh` emits `static/docs-search.json` (title, label, section, summary,
headings) and `static/docs-search.js` filters it client-side. It deliberately
does **not** use Zola's `build_search_index`: that emits an elasticlunr index
requiring the elasticlunr runtime vendored into `static/`, and filtering 113
rows does not warrant a library. The tradeoff is real — this searches titles,
summaries and headings, not full body text. If full-text search is wanted
later, switching to `build_search_index` is the path.

Keyboard: `/` focuses the box, `C-n`/`C-p` or arrows move, `Enter` opens,
`Esc` clears.

## Layout

```
site/
├── config.toml            Zola config
├── data/
│   ├── nav.toml           navigation manifest (tracked, hand-edited)
│   └── version.toml       generated from workspace Cargo.toml
├── content/               generated, except the _index.md files
├── templates/
│   ├── base.html          shell: header, footer, favicons
│   ├── index.html         landing page
│   ├── docs-home.html     /docs/ — learning path + section cards
│   ├── docs-section.html  /docs/<section>/
│   ├── docs-page.html     /docs/<section>/<topic>/ — TOC, breadcrumb, pager
│   ├── docs-nav.html      shared sidebar + search box
│   ├── section.html       dev docs sections
│   └── page.html          dev docs pages
├── sass/style.scss
├── static/
└── scripts/sync-docs.sh
```
