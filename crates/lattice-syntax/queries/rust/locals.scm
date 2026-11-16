; Tree-sitter locals for Rust.
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
  (block)
  (function_item)
  (closure_expression)
  (for_expression)
  (if_expression)
  (while_expression)
  (loop_expression)
  (match_arm)
  (impl_item)
  (trait_item)
  (mod_item)
] @local.scope

(let_declaration pattern: (identifier) @local.definition)
(parameter pattern: (identifier) @local.definition)
(closure_parameters (identifier) @local.definition)
(for_expression pattern: (identifier) @local.definition)
(function_item name: (identifier) @local.definition)
(const_item name: (identifier) @local.definition)
(static_item name: (identifier) @local.definition)

; Bindings inside patterns: `let (a, b)`, `Some(x)`, `ref x`, `mut x`,
; `x @ ..`, `S { a, b: c }`.
(tuple_pattern (identifier) @local.definition)
(tuple_struct_pattern type: (_) (identifier) @local.definition)
(slice_pattern (identifier) @local.definition)
(ref_pattern (identifier) @local.definition)
(mut_pattern (identifier) @local.definition)
(captured_pattern (identifier) @local.definition)
(field_pattern name: (shorthand_field_identifier) @local.definition)
(field_pattern pattern: (identifier) @local.definition)
(or_pattern (identifier) @local.definition)
(match_pattern (identifier) @local.definition)

(identifier) @local.reference
