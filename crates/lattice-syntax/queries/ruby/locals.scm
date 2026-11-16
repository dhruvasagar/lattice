; Tree-sitter locals for Ruby.
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
  (method)
  (singleton_method)
  (block)
  (do_block)
  (class)
  (module)
  (lambda)
] @local.scope

(method_parameters (identifier) @local.definition)
(lambda_parameters (identifier) @local.definition)
(block_parameters (identifier) @local.definition)
(optional_parameter name: (identifier) @local.definition)
(keyword_parameter name: (identifier) @local.definition)
(splat_parameter name: (identifier) @local.definition)
(hash_splat_parameter name: (identifier) @local.definition)
(block_parameter name: (identifier) @local.definition)
(assignment left: (identifier) @local.definition)
(left_assignment_list (identifier) @local.definition)
(for pattern: (identifier) @local.definition)
(exception_variable (identifier) @local.definition)

(call method: (identifier) @local.member)

(identifier) @local.reference
