; Tree-sitter indent rules for JavaScript.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; `@indent`  — this node's children sit one level deeper.
; `@outdent` — a line STARTING with this node sits one level shallower.
;
; Every node type here must exist in the grammar: `Query::new` rejects an
; unknown kind outright, failing the registry build for this language.
;
; Destructuring patterns (`object_pattern` / `array_pattern`) are
; captured alongside their expression counterparts so a multi-line
; `const { a, b } = x` indents like the object literal it mirrors.
;
; `template_string` is deliberately absent: its contents are literal
; text, and indenting inside one would change the string's value.

[
  (statement_block)
  (object)
  (array)
  (arguments)
  (formal_parameters)
  (class_body)
  (switch_body)
  (object_pattern)
  (array_pattern)
] @indent

[
  "}"
  "]"
  ")"
] @outdent
