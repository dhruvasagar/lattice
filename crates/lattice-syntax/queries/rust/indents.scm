; Tree-sitter indent rules for Rust.
;
; Capture vocabulary follows Helix's `indents.scm` dialect
; (`@indent` / `@outdent`), written for lattice rather than vendored --
; see `docs/dev/architecture/auto-indent.md` §4.1 for why.
;
; `@indent`  — this node's children sit one level deeper than the line
;              the node starts on.
; `@outdent` — a line that STARTS with this node sits one level
;              shallower. In practice: the closing delimiters, which
;              cancel the enclosing `@indent` so `}` aligns with the
;              `if` rather than with the body.
;
; Every node type named here must exist in tree-sitter-rust's grammar:
; `Query::new` rejects an unknown node type outright, which would fail
; the whole registry build for Rust rather than degrade. The
; `indents_scm_compiles_for_every_language_that_ships_one` test is the
; guard.

[
  ; Braced bodies.
  (block)
  (declaration_list)          ; impl / mod / trait bodies
  (field_declaration_list)    ; struct fields
  (enum_variant_list)
  (field_initializer_list)    ; struct literals
  (match_block)
  (use_list)

  ; Delimited lists that wrap.
  (arguments)
  (parameters)
  (array_expression)
  (tuple_expression)
  (tuple_pattern)
  (struct_pattern)

  ; Clauses that continue onto following lines.
  (where_clause)
] @indent

[
  "}"
  "]"
  ")"
] @outdent
