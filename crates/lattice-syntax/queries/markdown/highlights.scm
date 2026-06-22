; Lattice custom markdown (block) highlights.
;
; Based on tree-sitter-md's bundled query (itself from
; nvim-treesitter), with ONE deliberate change: heading titles are
; captured PER LEVEL (`@text.title.1` … `@text.title.6`) instead of the
; bundled level-less `@text.title`. The atx marker nodes
; (`atx_h1_marker` … `atx_h6_marker`) are distinct, so a pattern that
; requires a given marker captures that heading's inline title at its
; level. `crates/lattice-syntax/src/style.rs` maps `text.title.N` →
; `Style::HeadingN`, which the theme resolves to per-level size (`scale`)
; and per-level colour (`fg`) — so distinguishing levels here is what
; lights up both the variable heading size (Thread F) and the per-level
; heading colours. The level-less `text.title` stays mapped (→ Heading1)
; as a fallback for setext headings, which carry no level marker.
(atx_heading (atx_h1_marker) (inline) @text.title.1)
(atx_heading (atx_h2_marker) (inline) @text.title.2)
(atx_heading (atx_h3_marker) (inline) @text.title.3)
(atx_heading (atx_h4_marker) (inline) @text.title.4)
(atx_heading (atx_h5_marker) (inline) @text.title.5)
(atx_heading (atx_h6_marker) (inline) @text.title.6)
(setext_heading (paragraph) @text.title)

[
  (atx_h1_marker)
  (atx_h2_marker)
  (atx_h3_marker)
  (atx_h4_marker)
  (atx_h5_marker)
  (atx_h6_marker)
  (setext_h1_underline)
  (setext_h2_underline)
] @punctuation.special

[
  (link_title)
  (indented_code_block)
  (fenced_code_block)
] @text.literal

[
  (fenced_code_block_delimiter)
] @punctuation.delimiter

(code_fence_content) @none

[
  (link_destination)
] @text.uri

[
  (link_label)
] @text.reference

[
  (list_marker_plus)
  (list_marker_minus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
  (thematic_break)
] @punctuation.special

[
  (block_continuation)
  (block_quote_marker)
] @punctuation.special

[
  (backslash_escape)
] @string.escape
