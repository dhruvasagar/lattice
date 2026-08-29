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
; `///` gets its own node in this grammar, so a doc comment can carry the
; documentation style rather than the ordinary comment one.
(doc_comment) @comment.doc
(block_comment) @comment
(slash_comment) @comment

; --- Declaration names ---------------------------------------------------
; Through the `name:` field, so an identifier of the same shape elsewhere in the
; item is not swept up with it.
(world_item name: (identifier)) @type
(interface_item name: (identifier)) @type
(record_item name: (identifier)) @type
(enum_item name: (identifier)) @type
(variant_item name: (identifier)) @type
(flags_item name: (identifier)) @type
(resource_item name: (identifier)) @type
(type_item name: (identifier)) @type

(func_item name: (identifier)) @function
(static_method name: (identifier)) @function
(resource_method (func_item name: (identifier))) @function

; --- Types and parameters ------------------------------------------------
; `ty` is every type position — builtins (`u32`, `string`) and references to
; declared types alike. The grammar does not distinguish them and neither does a
; reader: both are "the type here".
(ty) @type.builtin
(named_type name: (identifier)) @variable.parameter

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

; --- Literals, attributes, punctuation -----------------------------------
(semver) @constant
(attribute) @attribute

"->" @operator
[ "," ";" "." ] @punctuation.delimiter
[ "(" ")" "{" "}" ] @punctuation.bracket
