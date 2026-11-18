; Strings, characters and escapes
(string) @string
(cstring) @string.special
(char_literal) @string
(escape_sequence) @string.escape

; Numbers and booleans
(integer) @number
(float) @number
(boolean) @boolean

; Comments (a doc comment is narrowed after this general pattern, see the end)
(comment) @comment

; Built-in and named types
(scalar_type) @type.builtin
(vector_type) @type.builtin
(type_identifier) @type
((type_identifier) @type.builtin
 (#match? @type.builtin "^(Fn|FnMut|FnOnce|Ordering)$"))
(function_bound "Fn" @type.builtin)
(function_bound "FnMut" @type.builtin)
(function_bound "FnOnce" @type.builtin)

; Keywords. Contextual keywords (`par`, `step`, `over`, `on`, `reduce`, `grain`)
; are literal tokens only in their one position, so this list is safe.
[
  "align"
  "as"
  "bench"
  "break"
  "catch"
  "const"
  "continue"
  "copy"
  "defer"
  "dispatch"
  "dyn"
  "else"
  "enum"
  "errdefer"
  "export"
  "extern"
  "fail"
  "fn"
  "for"
  "from"
  "grain"
  "if"
  "impl"
  "import"
  "in"
  "inline"
  "interface"
  "into"
  "kernel"
  "let"
  "linear"
  "loop"
  "match"
  "mut"
  "noinline"
  "on"
  "orelse"
  "over"
  "par"
  "pkg"
  "pub"
  "reduce"
  "return"
  "scope"
  "select"
  "spawn"
  "static"
  "step"
  "struct"
  "test"
  "try"
  "type"
  "unsafe"
  "var"
  "waitable"
  "when"
  "while"
] @keyword

; Operators
(operator) @operator
(reduce_operator) @operator
[
  "?"
  "!"
  ".*"
  ".."
  "..="
  "=>"
  "->"
  "="
  "&"
  "*"
  "|"
] @operator

; Brackets and delimiters
[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
  ".{"
  ".["
] @punctuation.bracket

[
  ","
  ";"
  ":"
  "."
] @punctuation.delimiter

; `self`
(self) @variable.special

; Parameters
(parameter name: (identifier) @variable.parameter)

; Labels, defined and used
(label) @label

; Constant declarations and constant generic parameters
(const_item name: (identifier) @constant)
(static_item name: (identifier) @constant)
(const_parameter name: (identifier) @constant)
(generic_argument value: (identifier) @constant)

; Enum variants: declarations, expressions and patterns
(variant name: (identifier) @variant)
(variant_expression name: (identifier) @variant)
(variant_pattern name: (identifier) @variant)

; Fields: declarations, access, literals and patterns
(field_declaration name: (identifier) @property)
(field_expression field: (identifier) @property)
(field_initializer name: (identifier) @property)
(field_pattern name: (identifier) @property)

; Namespaced paths: import segments and module qualifiers of type paths
(import_path (identifier) @namespace)
(import_path (type_identifier) @namespace)
(module_name) @namespace

; Declarations of functions
(function_item name: (identifier) @function)
(extern_function name: (identifier) @function)

; Callees. These come after the variant and field patterns, so the more
; specific call-site capture wins (Zed applies the last matching pattern).
(call_expression
  function: (identifier) @function)
(call_expression
  function: (field_expression field: (identifier) @function))
(call_expression
  function: (variant_expression name: (identifier) @function))

; Builtins: one capture, no fallback constant colour
(builtin) @function.builtin

; Doc comments last, after @comment
((comment) @comment.doc
 (#match? @comment.doc "^///"))
