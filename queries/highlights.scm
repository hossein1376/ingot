; Comments
((comment) @comment.doc
 (#match? @comment.doc "^///"))
(comment) @comment

; Strings and escapes
(string) @string
(cstring) @string.special
(char_literal) @string
(escape_sequence) @string.escape

; Numbers and literals
(integer) @number
(float) @number
(boolean) @boolean

; Built-in and named types
(scalar_type) @type.builtin
(vector_type) @type.builtin
(type_identifier) @type

; Declaration names
(fn_decl name: (identifier) @function)
(extern_function name: (identifier) @function)
(const_decl name: (identifier) @constant)
(static_decl name: (identifier) @constant)
(variant name: (identifier) @variant)
(struct_decl name: (type_identifier) @type)
(enum_decl name: (type_identifier) @type)
(interface_decl name: (type_identifier) @type)
(type_decl name: (type_identifier) @type)

; Parameters and labels
(parameter name: (identifier) @variable.parameter)
(for_statement label: (identifier) @label)
(while_statement label: (identifier) @label)
(loop_statement label: (identifier) @label)

; Struct-literal keys and field access
(field_init (identifier) @property)
(postfix_expression "." (identifier) @property)

; Builtins (@assert, @load, ...)
(builtin) @function

; Keywords
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
  "orelse"
  "over"
  "par"
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
  "grain"
  "on"
] @keyword

; Operators
[
  "+"
  "-"
  "*"
  "/"
  "%"
  "+%"
  "-%"
  "*%"
  "+|"
  "-|"
  "*|"
  "+%="
  "-%="
  "*%="
  "+|="
  "-|="
  "*|="
  "<<"
  ">>"
  "<<="
  ">>="
  "&"
  "|"
  "^"
  "&="
  "|="
  "^="
  "&&"
  "||"
  "!"
  "~"
  "=="
  "!="
  "<"
  "<="
  ">"
  ">="
  "="
  "+="
  "-="
  "*="
  "/="
  "%="
  "->"
  "=>"
  ".."
  "..="
  "..."
] @operator

; Delimiters
[
  ","
  ";"
  ":"
  "."
] @punctuation.delimiter

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

; Dot-brace / dot-bracket literals: closing token matches the opening one
(struct_literal ".{" @punctuation.special "}" @punctuation.special)
(array_literal ".{" @punctuation.special "}" @punctuation.special)
(expected_type_literal ".{" @punctuation.special "}" @punctuation.special)
(struct_pattern ".{" @punctuation.special "}" @punctuation.special)
(explicit_type_arguments ".[" @punctuation.special "]" @punctuation.special)

[
  ".{"
  ".["
  ".*"
  "?"
] @punctuation.special
