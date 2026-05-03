; Tree-sitter fold ranges for Rust.
; Adapted from the Helix editor's runtime/queries/rust/folds.scm
; (Mozilla Public License 2.0). Each `@fold` capture marks an AST
; node whose byte range becomes a fold; the Lattice runner converts
; that range to a [start_line, end_line] fold and runs identity
; matching against the previous fold set so closed-state survives
; reparses.

[
  (function_item)
  (struct_item)
  (enum_item)
  (impl_item)
  (trait_item)
  (mod_item)
  (union_item)
  (use_declaration)
  (extern_crate_declaration)
  (macro_definition)
  (macro_invocation)

  (block)
  (match_block)
  (declaration_list)
  (enum_variant_list)
  (field_declaration_list)
  (field_initializer_list)
  (parameters)
  (token_tree)
  (use_list)
  (arguments)
  (array_expression)
  (tuple_expression)
  (struct_expression)
  (closure_expression)
  (closure_parameters)
  (where_clause)
  (type_arguments)
  (type_parameters)

  (block_comment)
] @fold
