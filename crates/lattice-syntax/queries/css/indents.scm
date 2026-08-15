; Tree-sitter indent rules for CSS.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; CSS has essentially one indenting construct — the rule block — which
; also covers at-rules (`@media`, `@supports`) since tree-sitter-css
; models their bodies as the same `block` node. The brevity is the
; language, not an omission.

[
  (block)
] @indent

[
  "}"
  "]"
  ")"
] @outdent
