; Tree-sitter indent rules for Java.
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
; Java's convention indents switch labels (unlike the C family default
; chosen in `queries/c/indents.scm`), but `switch_block` is captured
; here for the block itself only — the label question is left to the
; same uncaptured default, so labels align with the braces. Consistent
; with the C file rather than with `google-java-format`; `=` plus a
; formatter is the right tool for house style.

[
  (block)
  (class_body)
  (interface_body)
  (enum_body)
  (constructor_body)
  (annotation_type_body)
  (argument_list)
  (formal_parameters)
  (array_initializer)
  (switch_block)
] @indent

[
  "}"
  "]"
  ")"
] @outdent
