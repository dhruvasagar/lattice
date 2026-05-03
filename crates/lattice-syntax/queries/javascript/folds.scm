; Tree-sitter fold ranges for JavaScript.
; Adapted from the Helix editor's runtime/queries/javascript/folds.scm
; (Mozilla Public License 2.0).

[
  (function_declaration)
  (function_expression)
  (arrow_function)
  (generator_function_declaration)
  (method_definition)
  (class_declaration)
  (class_body)

  (statement_block)
  (object)
  (object_pattern)
  (array)
  (array_pattern)
  (formal_parameters)
  (arguments)
  (switch_body)

  (export_clause)
  (named_imports)
] @fold
