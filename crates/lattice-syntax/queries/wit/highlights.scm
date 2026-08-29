; WIT — WebAssembly Interface Types.
;
; Written here rather than using `tree_sitter_wit::HIGHLIGHTS_QUERY`, which
; exists but names its captures in the TextMate vocabulary
; (`entity.name.type.interface`, `storage.modifier`, `meta.attribute`).
; `style::name_to_style` keys on the FIRST dot-segment, so those resolve to
; `Style::Default` — leaving every `interface`, `record`, `world`, `enum`,
; `variant` and `resource` NAME unstyled, i.e. exactly the tokens a reader scans
; a `.wit` file for.
;
; The node and token set below is the one the upstream query proves exists; the
; grammar has no anonymous tokens for `list` / `option` / `result` and friends
; (they parse as `ty`), so there is nothing to capture for them separately.

; --- Comments ------------------------------------------------------------
; Captured with the SAME names Rust's query uses, so `///` in a `.wit` file and
; `///` in a `.rs` file resolve to the same style. `name_to_style` keys on the
; first dot-segment, so `comment.documentation` and `comment` both land on
; `Style::Comment` — there is no separate doc style to reach for, and inventing
; a capture name for one would only have looked like it worked.
(doc_comment) @comment.documentation
(block_comment) @comment
(slash_comment) @comment

; --- Declaration names ---------------------------------------------------
; The capture closes INSIDE the parens, on the identifier. Written the other way
; — `(interface_item name: (identifier)) @type` — the capture attaches to the
; whole `interface_item`, so the item's entire span takes the style: doc comment,
; body and all. That is what the upstream query does, and it is why a `.wit` file
; rendered as one long block of `type` with its comments swallowed.
(world_item name: (identifier) @type)
(interface_item name: (identifier) @type)
(record_item name: (identifier) @type)
(enum_item name: (identifier) @type)
(variant_item name: (identifier) @type)
(flags_item name: (identifier) @type)
(resource_item name: (identifier) @type)
(type_item name: (identifier) @type)

(func_item name: (identifier) @function)
(static_method name: (identifier) @function)

; --- Types and parameters ------------------------------------------------
; `ty` is every type position — builtins (`u32`, `string`) and references to
; declared types alike. The grammar does not distinguish them and neither does a
; reader: both are "the type here".
(ty) @type.builtin
(named_type name: (identifier) @variable.parameter)

; --- Keywords ------------------------------------------------------------
[
  "package"
  "world"
  "interface"
  "import"
  "export"
  "include"
  "use"
  "type"
  "record"
  "enum"
  "flags"
  "variant"
  "resource"
  "func"
  "as"
  "with"
] @keyword

; `static` and `constructor` shape a resource's surface rather than declaring a
; new item, so they read as modifiers on the function line.
"static" @keyword
"constructor" @function

; --- Literals and punctuation --------------------------------------------
(semver) @constant

; `(attribute)` is deliberately NOT captured. In this grammar an attribute node
; SPANS the doc comment attached to the same item, so capturing it paints the
; `///` line as an attribute — which is how `///` in a `.wit` file came to look
; nothing like `///` in a `.rs` file. Its constituent tokens still style through
; the keyword and punctuation rules; a real `@since(...)` capture needs the
; attribute's inner nodes, and belongs with whoever needs it.

"->" @operator
[ "," ";" "." ] @punctuation.delimiter
[ "(" ")" "{" "}" ] @punctuation.bracket
