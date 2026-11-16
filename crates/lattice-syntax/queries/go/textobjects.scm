(function_declaration) @function.outer
(function_declaration
  body: (block) @function.inner)
(method_declaration) @function.outer
(method_declaration
  body: (block) @function.inner)
(type_declaration) @class.outer
(if_statement) @block.outer
(if_statement
  consequence: (block) @block.inner)
(for_statement) @block.outer
(for_statement
  body: (block) @block.inner)
(expression_switch_statement) @block.outer
(type_switch_statement) @block.outer
(comment) @comment.outer
(comment) @comment.inner
(parameter_declaration) @parameter.outer
(parameter_declaration) @parameter.inner

; RF.4b′: a call's arguments are `aa` / `ia` too, as the help promises
; and nvim-treesitter-textobjects does: `daa` in `f(a, b)` reaches `b`.
(argument_list (_) @parameter.outer)
(argument_list (_) @parameter.inner)
