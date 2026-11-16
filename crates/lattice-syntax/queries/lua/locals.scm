; Tree-sitter locals for Lua.
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
  (chunk)
  (block)
  (function_declaration)
  (function_definition)
  (for_statement)
] @local.scope

(variable_declaration (assignment_statement (variable_list name: (identifier) @local.definition)))
(variable_declaration (variable_list name: (identifier) @local.definition))
(function_declaration name: (identifier) @local.definition)
(parameters name: (identifier) @local.definition)
(for_numeric_clause name: (identifier) @local.definition)
(for_generic_clause (variable_list name: (identifier) @local.definition))

(dot_index_expression field: (identifier) @local.member)
(method_index_expression method: (identifier) @local.member)
(field name: (identifier) @local.member)

(identifier) @local.reference
