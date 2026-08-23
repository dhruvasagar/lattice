; Org highlights — per-level headlines.
;
; Org's headline marker is ONE node whose *text length* is the level:
; `(headline (stars) (item))`, where `stars` is `*`, `**`, `***`… This is
; unlike markdown, whose grammar gives each level its own marker node
; (`atx_h1_marker` … `atx_h6_marker`), so a per-level capture there is just
; six ordinary patterns.
;
; Here the level has to come from the text, which means `#eq?` predicates.
; No query bundled with lattice uses one, so this file is also what proves
; the pipeline evaluates them — it does, via tree-sitter's `QueryMatches`,
; which filters on text predicates as it advances. Nothing host-side was
; needed.
;
; Upstream's own highlights.scm cycles three levels with `#match?` regexes
; (`^(\*{3})*\*$` matches 1, 4, 7 stars). We want true per-level 1..6, and
; `#eq?` says that directly.
;
; The `stars` capture is `@punctuation.special` → `Style::Markup`, the same
; style markdown's `#` markers take. That is what keeps the stars at base
; size while the title scales: `heading_scale_split` looks for the first run
; whose resolved `scale > 1.0`, so `[stars at 1.0][title at N]` renders as
; two pieces on one baseline, exactly as `## Title` does.

(headline (stars) @punctuation.special (item) @text.title.1
  (#eq? @punctuation.special "*"))
(headline (stars) @punctuation.special (item) @text.title.2
  (#eq? @punctuation.special "**"))
(headline (stars) @punctuation.special (item) @text.title.3
  (#eq? @punctuation.special "***"))
(headline (stars) @punctuation.special (item) @text.title.4
  (#eq? @punctuation.special "****"))
(headline (stars) @punctuation.special (item) @text.title.5
  (#eq? @punctuation.special "*****"))
(headline (stars) @punctuation.special (item) @text.title.6
  (#eq? @punctuation.special "******"))

; Everything deeper than six shares level 6 rather than losing its heading
; identity — org has no depth limit, the theme's scale ramp does.
(headline (stars) @punctuation.special (item) @text.title.6
  (#match? @punctuation.special "^\\*{7,}$"))

; The rest is ordinary, node-name-for-node-name — no predicates needed.
; Every node named here exists in `nvim-orgmode/tree-sitter-org`'s
; node-types.json; queries compile at REGISTRATION, so a name that does not
; is a load-time error naming this file rather than a silently dead pattern.
(comment) @comment
(directive) @keyword
(property_drawer) @comment
(drawer) @comment
(block) @markup.raw
(listitem (bullet) @punctuation.special)
(timestamp) @constant
(tag) @attribute
(checkbox) @constant
