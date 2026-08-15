; Tree-sitter indent rules for TypeScript.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1). The JavaScript set
; applies unchanged — tree-sitter-typescript extends that grammar — so
; this file repeats it and adds the type-level nodes.
;
; See `queries/javascript/indents.scm` for the shared notes.

[
  ; Shared with JavaScript.
  (statement_block)
  (object)
  (array)
  (arguments)
  (formal_parameters)
  (class_body)
  (switch_body)
  (object_pattern)
  (array_pattern)

  ; TypeScript-specific: type-level braces and angle brackets.
  (object_type)
  (enum_body)
  (type_arguments)
  (type_parameters)
] @indent

[
  "}"
  "]"
  ")"
] @outdent
