; Tree-sitter indent rules for HTML.
;
; Capture vocabulary follows Helix's `indents.scm` dialect; written for
; lattice rather than vendored (auto-indent.md §4.1).
;
; `element` spans start-tag through end-tag, so children indent and the
; closing tag dedents — the same shape as a braced block, with
; `end_tag` playing the part of `}`.
;
; **Void elements need no special handling.** `<br>` and `<img>` parse
; to an `element` (or `self_closing_element`) that begins and ends on
; one row, and the engine indents a row only when an ancestor satisfies
; `start_row < row <= end_row` — which a one-row node can never do for
; any row. The row rule excludes them structurally, the same way it
; excludes Ruby's modifier `if`. The plan listed void elements as a
; hazard for this slice; they turned out not to be one.
;
; `script` / `style` contents are left to their injected grammars.

[
  (element)
  (script_element)
  (style_element)
] @indent

[
  (end_tag)
  ">"
] @outdent
