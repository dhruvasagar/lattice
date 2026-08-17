; TC.5 — Markdown context scopes.
;
; Headings are the whole structure of a markdown document, and a section's
; heading scrolling away is exactly the case the strip is for. `section` nests,
; so an H3 inside an H2 inside an H1 pins all three — which is the outline the
; reader wants.
(section) @context
