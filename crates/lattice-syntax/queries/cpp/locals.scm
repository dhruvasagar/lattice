; Tree-sitter locals for C++.
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
  (translation_unit)
  (function_definition)
  (compound_statement)
  (for_statement)
  (for_range_loop)
  (if_statement)
  (while_statement)
  (lambda_expression)
  (catch_clause)
  (namespace_definition)
] @local.scope

(init_declarator declarator: (identifier) @local.definition)
(init_declarator declarator: (pointer_declarator declarator: (identifier) @local.definition))
(init_declarator declarator: (reference_declarator (identifier) @local.definition))
(declaration declarator: (identifier) @local.definition)
(parameter_declaration declarator: (identifier) @local.definition)
(parameter_declaration declarator: (pointer_declarator declarator: (identifier) @local.definition))
(parameter_declaration declarator: (reference_declarator (identifier) @local.definition))
(optional_parameter_declaration declarator: (identifier) @local.definition)
(for_range_loop declarator: (identifier) @local.definition)
(function_declarator declarator: (identifier) @local.definition)
(structured_binding_declarator (identifier) @local.definition)

(identifier) @local.reference
