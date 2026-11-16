; Tree-sitter locals for Go.
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
  (source_file)
  (function_declaration)
  (method_declaration)
  (func_literal)
  (block)
  (if_statement)
  (for_statement)
  (expression_switch_statement)
  (type_switch_statement)
  (select_statement)
  (expression_case)
  (type_case)
  (communication_case)
  (default_case)
] @local.scope

(function_declaration name: (identifier) @local.definition)
(parameter_declaration name: (identifier) @local.definition)
(variadic_parameter_declaration name: (identifier) @local.definition)
(short_var_declaration left: (expression_list (identifier) @local.definition))
(var_spec name: (identifier) @local.definition)
(const_spec name: (identifier) @local.definition)
(range_clause left: (expression_list (identifier) @local.definition))
(type_switch_statement alias: (expression_list (identifier) @local.definition))
(receive_statement left: (expression_list (identifier) @local.definition))

; `Point{X: 1}`: the key names a field.
(keyed_element . (literal_element (identifier) @local.member))

(identifier) @local.reference
