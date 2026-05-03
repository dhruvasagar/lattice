; Tree-sitter fold ranges for Python.
; Adapted from the Helix editor's runtime/queries/python/folds.scm
; (Mozilla Public License 2.0).

[
  (function_definition)
  (class_definition)
  (decorated_definition)

  (if_statement)
  (for_statement)
  (while_statement)
  (try_statement)
  (with_statement)
  (match_statement)

  (import_from_statement)
  (parameters)
  (argument_list)
  (parenthesized_expression)
  (generator_expression)
  (list_comprehension)
  (set_comprehension)
  (dictionary_comprehension)

  (tuple)
  (list)
  (set)
  (dictionary)
] @fold
