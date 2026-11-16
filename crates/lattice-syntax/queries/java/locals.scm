; Tree-sitter locals for Java.
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
  (class_body)
  (method_declaration)
  (constructor_declaration)
  (block)
  (lambda_expression)
  (for_statement)
  (enhanced_for_statement)
  (catch_clause)
] @local.scope

(local_variable_declaration declarator: (variable_declarator name: (identifier) @local.definition))
(formal_parameter name: (identifier) @local.definition)
(spread_parameter (variable_declarator name: (identifier) @local.definition))
(catch_formal_parameter name: (identifier) @local.definition)
(enhanced_for_statement name: (identifier) @local.definition)
(lambda_expression parameters: (identifier) @local.definition)
(inferred_parameters (identifier) @local.definition)
(resource name: (identifier) @local.definition)
(method_declaration name: (identifier) @local.definition)

(field_access field: (identifier) @local.member)
(method_invocation name: (identifier) @local.member)

(identifier) @local.reference
