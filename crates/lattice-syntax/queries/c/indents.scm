; Tree-sitter indent rules for C.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; `@indent`  — this node's children sit one level deeper.
; `@outdent` — a line STARTING with this node sits one level shallower,
;              cancelling the enclosing `@indent` so a closing delimiter
;              aligns with its opener.
;
; Every node type here must exist in the grammar: `Query::new` rejects an
; unknown kind outright, which fails the whole registry build for this
; language rather than degrading gracefully. The registry test
; `standard_registry_includes_every_supported_language` is the guard.
;
; `case`/`default` labels are deliberately NOT captured. Whether a switch
; body indents its labels is a house-style question with no correct
; answer, and tree-sitter-c models the labels as children of the
; compound_statement either way — so leaving them uncaptured gives the
; common "labels align with the braces, statements indent" result.

[
  (compound_statement)
  (field_declaration_list)
  (enumerator_list)
  (initializer_list)
  (argument_list)
  (parameter_list)
] @indent

[
  "}"
  "]"
  ")"
] @outdent
