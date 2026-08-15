; Tree-sitter indent rules for YAML.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; **The hazard here is block scalars, and it is a data-corruption one,
; not a cosmetic one.** In
;
;     script: |
;       line one
;         line two
;
; the leading whitespace inside the `|` block is the VALUE. Applying the
; enclosing mapping's structural indent would change what the YAML
; means. That is handled outside this file: `block_scalar` is in the
; engine's string-scope set, so it refuses to answer inside one and the
; lexical bridge copies the previous line instead.
;
; `string_scalar` is deliberately NOT in that set — YAML wraps every
; plain scalar in one, so including it would disable indentation for
; essentially all YAML.
;
; `block_mapping_pair` spans from the key's row to the end of its
; nested value, so a nested mapping indents by the ordinary row rule. A
; pair whose value sits on the same row is one row tall and contributes
; nothing, which is correct.

[
  (block_mapping_pair)
  (block_sequence)
  (flow_mapping)
  (flow_sequence)
] @indent

[
  "}"
  "]"
] @outdent
