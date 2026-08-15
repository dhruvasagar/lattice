; Tree-sitter indent rules for TSX (TypeScript + JSX).
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1). The TypeScript set
; applies unchanged; this file adds the JSX nodes.
;
; See `queries/javascript/indents.scm` for the shared notes.
;
; `jsx_element` indents its children so nested markup nests visually.
; `jsx_self_closing_element` is NOT captured — it has no children to
; indent, and capturing it would add a level for a node that closes on
; the line it opens.

[
  ; Shared with JavaScript / TypeScript.
  (statement_block)
  (object)
  (array)
  (arguments)
  (formal_parameters)
  (class_body)
  (switch_body)
  (object_pattern)
  (array_pattern)
  (object_type)
  (enum_body)
  (type_arguments)
  (type_parameters)

  ; JSX.
  (jsx_element)
  (jsx_expression)
] @indent

[
  "}"
  "]"
  ")"
] @outdent
