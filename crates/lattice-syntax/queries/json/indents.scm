; Tree-sitter indent rules for JSON.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; Two constructs, and that is the whole language. Cheap to get exactly
; right, which is why JSON is in the brace-family slice rather than
; waiting: correct indentation in a hand-edited config file is worth
; more than the two lines it costs.

[
  (object)
  (array)
] @indent

[
  "}"
  "]"
] @outdent
