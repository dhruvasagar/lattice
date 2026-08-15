; Tree-sitter indent rules for Go.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; `@indent`  — this node's children sit one level deeper.
; `@outdent` — a line STARTING with this node sits one level shallower.
;
; Every node type here must exist in the grammar: `Query::new` rejects an
; unknown kind outright, failing the registry build for this language.
;
; Go indents with TABS by convention. That is not expressed here — the
; indent UNIT (tabs vs spaces, and how wide) is `expandtab` /
; `shiftwidth`, contributed per-language through `Mode::options()` in
; IN.11. This file only says WHERE the levels are; the unit says what
; one level is made of. Keeping the two apart is why `tabstop` and
; `shiftwidth` are separate options (auto-indent.md §3).
;
; `case` / `default` clauses in a switch are not captured, matching the
; C family's choice: gofmt aligns them with the `switch`, which is what
; leaving them uncaptured produces.

[
  (block)
  (literal_value)
  (argument_list)
  (parameter_list)
  (field_declaration_list)
  (interface_type)
  (expression_switch_statement)
  (type_switch_statement)
  (select_statement)
  (const_declaration)
  (var_declaration)
  (import_spec_list)
] @indent

[
  "}"
  "]"
  ")"
] @outdent
