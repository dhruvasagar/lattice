; Tree-sitter locals for Python.
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
  (module)
  (function_definition)
  (lambda)
  (class_definition)
  (list_comprehension)
  (set_comprehension)
  (dictionary_comprehension)
  (generator_expression)
] @local.scope

(function_definition name: (identifier) @local.definition)
(class_definition name: (identifier) @local.definition)
(parameters (identifier) @local.definition)
(lambda_parameters (identifier) @local.definition)
(default_parameter name: (identifier) @local.definition)
(typed_parameter (identifier) @local.definition)
(typed_default_parameter name: (identifier) @local.definition)
(list_splat_pattern (identifier) @local.definition)
(dictionary_splat_pattern (identifier) @local.definition)
(assignment left: (identifier) @local.definition)
(assignment left: (pattern_list (identifier) @local.definition))
(assignment left: (tuple_pattern (identifier) @local.definition))
(for_statement left: (identifier) @local.definition)
(for_statement left: (pattern_list (identifier) @local.definition))
(for_in_clause left: (identifier) @local.definition)
(for_in_clause left: (pattern_list (identifier) @local.definition))
(as_pattern alias: (as_pattern_target (identifier) @local.definition))
(named_expression name: (identifier) @local.definition)
(import_statement name: (dotted_name . (identifier) @local.definition))
(import_from_statement name: (dotted_name (identifier) @local.definition))
(aliased_import alias: (identifier) @local.definition)

(attribute attribute: (identifier) @local.member)
(keyword_argument name: (identifier) @local.member)

(identifier) @local.reference
