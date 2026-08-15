; Tree-sitter indent rules for C++.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1). See
; `queries/c/indents.scm` for the shared header notes — tree-sitter-cpp
; extends the C grammar, so the C node set applies unchanged and this
; file adds only the C++-specific ones.
;
; `>` is NOT captured as an `@outdent` even though it closes a template
; argument list: it is also the greater-than operator, and dedenting a
; line that merely starts with a comparison would be worse than leaving
; a template continuation under-indented.

[
  (compound_statement)
  (field_declaration_list)
  (enumerator_list)
  (initializer_list)
  (argument_list)
  (parameter_list)

  ; C++-specific.
  (template_argument_list)
  (template_parameter_list)
  (declaration_list)
] @indent

[
  "}"
  "]"
  ")"
] @outdent
