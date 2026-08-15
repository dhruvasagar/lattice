; Tree-sitter indent rules for TOML.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; **`table` is deliberately NOT captured.** A `[table]` node spans its
; whole section, so capturing it would indent every key underneath —
; and TOML convention is flush-left keys under a flush-left header.
; That is the one judgement call this file makes, and it is the reason
; it is three lines rather than one.
;
; What remains genuinely wraps: multi-line arrays and inline tables,
; both of which include their brackets in the node, so the ordinary
; brace-family behaviour applies.

[
  (array)
  (inline_table)
] @indent

[
  "}"
  "]"
] @outdent
