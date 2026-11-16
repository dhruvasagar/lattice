; Tree-sitter locals for TypeScript and TSX (one file: TSX's grammar is a
; superset of TypeScript's).
;
; Capture contract (lattice-syntax `locals` module, refactor.md §9.1):
;   @local.scope       a node that opens a scope for the names defined in it
;   @local.definition  an identifier that introduces a local name
;   @local.reference   an identifier that may read a name (a catch-all)
;   @local.member      an identifier in a member or label position, which
;                      shares the reference node kind but never reads a local
; A node captured as @local.definition or @local.member is not a reference.
; Written for lattice against the pinned grammar, not vendored.

[
  (program)
  (statement_block)
  (function_declaration)
  (function_expression)
  (generator_function_declaration)
  (generator_function)
  (arrow_function)
  (method_definition)
  (class_body)
  (for_statement)
  (for_in_statement)
  (catch_clause)
] @local.scope

(variable_declarator name: (identifier) @local.definition)
(object_pattern (shorthand_property_identifier_pattern) @local.definition)
(object_pattern (pair_pattern value: (identifier) @local.definition))
(array_pattern (identifier) @local.definition)
(assignment_pattern left: (identifier) @local.definition)
(rest_pattern (identifier) @local.definition)
(function_declaration name: (identifier) @local.definition)
(generator_function_declaration name: (identifier) @local.definition)
(class_declaration name: (type_identifier) @local.definition)
(required_parameter pattern: (identifier) @local.definition)
(optional_parameter pattern: (identifier) @local.definition)
(arrow_function parameter: (identifier) @local.definition)
(catch_clause parameter: (identifier) @local.definition)
(for_in_statement left: (identifier) @local.definition)
(import_specifier !alias name: (identifier) @local.definition)
(import_specifier alias: (identifier) @local.definition)
(namespace_import (identifier) @local.definition)
(import_clause (identifier) @local.definition)

(shorthand_property_identifier) @local.reference
(identifier) @local.reference
