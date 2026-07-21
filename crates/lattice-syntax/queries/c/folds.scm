; Tree-sitter fold ranges for C.
; Adapted from the Helix editor's runtime/queries/c/folds.scm
; (Mozilla Public License 2.0).

[
  (function_definition)
  (struct_specifier)
  (union_specifier)
  (enum_specifier)
  (for_statement)
  (while_statement)
  (if_statement)
  (compound_statement)
] @fold
