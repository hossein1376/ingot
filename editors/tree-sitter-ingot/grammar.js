// Tree-sitter grammar for Ingot, written from docs/grammar.md in the compiler repo.
// It is a highlighting-grade grammar: it accepts a superset in corners where the
// specification needs semantic analysis, and it reports precise positions for the
// syntax it does not accept. The compiler remains the source of truth.

const commaSep = (rule) => seq(rule, repeat(seq(",", rule)), optional(","));
const commaSep1 = (rule) => seq(rule, repeat(seq(",", rule)), optional(","));

// Named operator nodes, so a query can colour every operator with one capture.
const op = ($, ...tokens) =>
  alias(tokens.length === 1 ? tokens[0] : choice(...tokens), $.operator);

const PREC = {
  POSTFIX: 20,
  UNARY: 18,
  CAST: 16,
  MUL: 14,
  ADD: 13,
  SHIFT: 12,
  BIT: 11,
  CMP: 10,
  AND: 9,
  OR: 8,
  ORELSE: 7,
  CATCH: 6,
};

module.exports = grammar({
  name: "ingot",

  word: ($) => $.identifier,

  conflicts: ($) => [
    [$.struct_expression, $._expression],
    [$.module_name, $._expression],
    [$.path_base, $._expression],
    [$.path_base],
    [$.generic_type],
    [$.function_bound],
    [$.function_type],
    [$.if_expression],
    [$.type_parameter, $.path_base],
    [$._type, $._expression],
    [$._statement, $._expression],
  ],

  extras: ($) => [/\s/, $.comment],

  rules: {
    module: ($) => repeat(choice($.import_declaration, $._item)),

    // ---- comments and literals ----
    // Zed applies the last matching highlight pattern, so `highlights.scm` captures
    // every comment as @comment and narrows `///` to @comment.doc afterwards.
    comment: (_) => token(seq("//", /[^\n]*/)),

    string: ($) =>
      seq('"', repeat(choice($.escape_sequence, /[^"\\\n]/)), '"'),

    cstring: ($) =>
      seq('c"', repeat(choice($.escape_sequence, /[^"\\\n]/)), '"'),

    char_literal: ($) =>
      seq("'", choice($.escape_sequence, /[^'\\\n]/), "'"),

    escape_sequence: (_) => /\\(x[0-9A-Fa-f]{2}|[^\n])/,

    integer: (_) =>
      token(
        choice(
          /0x[0-9A-Fa-f][0-9A-Fa-f_]*/,
          /0o[0-7][0-7_]*/,
          /0b[01][01_]*/,
          /[0-9][0-9_]*/
        )
      ),

    // A float always has a digit on both sides of the dot. After the dot of a field
    // access only `integer` is valid in the grammar, so `t.0.1` never lexes as a
    // float (tree-sitter only lexes tokens that are valid in the current state).
    float: (_) =>
      token(
        choice(
          /[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9][0-9_]*)?/,
          /[0-9][0-9_]*[eE][+-]?[0-9][0-9_]*/
        )
      ),

    boolean: (_) => choice("true", "false"),

    identifier: (_) => /[a-z_][a-z0-9_]*/,

    type_identifier: (_) => /[A-Z][A-Za-z0-9_]*/,

    builtin: (_) => /@[a-z_][a-z0-9_]*/,

    scalar_type: (_) =>
      choice(
        "i8", "i16", "i32", "i64",
        "u8", "u16", "u32", "u64",
        "f32", "f64",
        "bool", "void", "never"
      ),

    vector_type: (_) =>
      token(
        prec(
          2,
          /(i8|i16|i32|i64|u8|u16|u32|u64|f32|f64|bool)x[0-9]+/
        )
      ),

    // ---- imports ----
    import_declaration: ($) => seq("import", $.import_path, ";"),

    import_path: ($) =>
      seq(
        field("module", $.identifier),
        repeat(seq(".", field("module", $.identifier))),
        optional(seq(".", field("item", $.type_identifier))),
        optional(seq("as", field("alias", choice($.identifier, $.type_identifier))))
      ),

    // ---- items ----
    _item: ($) =>
      choice(
        $.function_item,
        $.struct_item,
        $.enum_item,
        $.interface_item,
        $.impl_item,
        $.const_item,
        $.static_item,
        $.type_item,
        $.extern_block,
        $.test_item,
        $.bench_item,
        $.when_items
      ),

    visibility: ($) => seq("pub", optional(seq("(", "pkg", ")"))),

    function_modifier: (_) =>
      choice("inline", "noinline", "unsafe", "export", "kernel", "waitable"),

    function_item: ($) =>
      seq(
        optional(field("visibility", $.visibility)),
        repeat($.function_modifier),
        "fn",
        field("name", $.identifier),
        optional(field("generics", $.generic_parameters)),
        field("parameters", $.parameters),
        optional(field("result", $.result)),
        choice(field("body", $.block), ";")
      ),

    parameters: ($) => seq("(", optional(commaSep($.parameter)), ")"),

    parameter: ($) =>
      seq(
        field("name", choice($.self, $.identifier)),
        ":",
        field("type", $._type),
        optional(seq("into", field("into", $.identifier)))
      ),

    result: ($) =>
      seq(
        "->",
        field("type", $._type),
        optional(seq("!", field("error", $._type))),
        optional(
          seq("from", field("origin", $.origin), repeat(seq(",", field("origin", $.origin))))
        ),
        optional($.reduce_clause)
      ),

    origin: ($) => choice(seq(field("name", $.identifier), optional(".*")), "static"),

    reduce_clause: ($) => seq("reduce", "(", field("operator", $.reduce_operator), ")"),

    reduce_operator: ($) => choice("+", "*", "&", "|", "^", $.identifier),

    generic_parameters: ($) => seq("[", commaSep1($._generic_parameter), "]"),

    _generic_parameter: ($) => choice($.type_parameter, $.const_parameter),

    type_parameter: ($) =>
      seq(
        field("name", $.type_identifier),
        optional(seq(":", field("bound", $._bound), repeat(seq("+", field("bound", $._bound)))))
      ),

    const_parameter: ($) =>
      seq("const", field("name", $.identifier), ":", field("type", $._type)),

    _bound: ($) => choice($.path_type, $.generic_type, $.function_bound),

    function_bound: ($) =>
      seq(
        field("name", choice("Fn", "FnMut", "FnOnce")),
        optional(
          seq(
            "(",
            optional(commaSep($._type)),
            ")",
            optional(seq("->", $._type, optional(seq("!", $._type))))
          )
        )
      ),

    struct_item: ($) =>
      seq(
        optional(field("visibility", $.visibility)),
        optional(choice("copy", "linear")),
        "struct",
        field("name", $.type_identifier),
        optional(field("generics", $.generic_parameters)),
        optional(field("alignment", $.alignment)),
        "{",
        optional(commaSep($.field_declaration)),
        "}"
      ),

    field_declaration: ($) =>
      seq(
        optional(field("visibility", $.visibility)),
        field("name", $.identifier),
        ":",
        field("type", $._type),
        optional(field("alignment", $.alignment))
      ),

    alignment: ($) => seq("align", "(", field("value", $._expression), ")"),

    enum_item: ($) =>
      seq(
        optional(field("visibility", $.visibility)),
        optional("copy"),
        "enum",
        field("name", $.type_identifier),
        optional(field("generics", $.generic_parameters)),
        optional(seq(":", field("type", $._type))),
        "{",
        optional(commaSep($.variant)),
        "}"
      ),

    variant: ($) =>
      seq(
        field("name", $.identifier),
        optional(seq(":", optional("from"), field("type", $._type))),
        optional(seq("=", field("value", $._expression)))
      ),

    interface_item: ($) =>
      seq(
        optional(field("visibility", $.visibility)),
        "interface",
        field("name", $.type_identifier),
        optional(field("generics", $.generic_parameters)),
        optional(
          seq(":", field("bound", $._bound), repeat(seq("+", field("bound", $._bound))))
        ),
        "{",
        repeat($.interface_member),
        "}"
      ),

    interface_member: ($) => choice($.associated_type, $.function_item),

    associated_type: ($) =>
      seq("type", field("name", $.type_identifier), ";"),

    impl_item: ($) =>
      seq(
        optional("unsafe"),
        "impl",
        optional(field("generics", $.generic_parameters)),
        choice(
          seq(field("interface", $._type), "for", field("type", $._type)),
          field("type", $._type)
        ),
        "{",
        repeat($._impl_member),
        "}"
      ),

    _impl_member: ($) => choice($.function_item, $.const_item, $.type_item),

    const_item: ($) =>
      seq(
        optional(field("visibility", $.visibility)),
        "const",
        field("name", $.identifier),
        ":",
        field("type", $._type),
        "=",
        field("value", $._expression),
        ";"
      ),

    static_item: ($) =>
      seq(
        optional(field("visibility", $.visibility)),
        "static",
        field("name", $.identifier),
        ":",
        field("type", $._type),
        optional(field("alignment", $.alignment)),
        "=",
        field("value", $._expression),
        ";"
      ),

    type_item: ($) =>
      seq(
        optional(field("visibility", $.visibility)),
        "type",
        field("name", $.type_identifier),
        optional(field("generics", $.generic_parameters)),
        "=",
        field("value", $._type),
        ";"
      ),

    extern_block: ($) =>
      seq("extern", field("library", $.string), "{", repeat($.extern_function), "}"),

    extern_function: ($) =>
      seq(
        "fn",
        field("name", $.identifier),
        field("parameters", $.extern_parameters),
        optional(field("result", $.result)),
        ";"
      ),

    extern_parameters: ($) =>
      seq(
        "(",
        optional(
          choice(
            seq(commaSep($.parameter), optional("...")),
            "..."
          )
        ),
        ")"
      ),

    test_item: ($) => seq("test", field("name", $.string), field("body", $.block)),

    bench_item: ($) => seq("bench", field("name", $.string), field("body", $.block)),

    when_items: ($) =>
      seq(
        "when",
        field("condition", $._expression),
        "{",
        repeat($._item),
        "}",
        optional(
          seq("else", choice(seq("{", repeat($._item), "}"), $.when_items))
        )
      ),

    // ---- types ----
    _type: ($) =>
      choice(
        $.scalar_type,
        $.vector_type,
        $.reference_type,
        $.slice_type,
        $.array_type,
        $.pointer_type,
        $.optional_type,
        $.tuple_type,
        $.function_type,
        $.generic_type,
        $.path_type
      ),

    reference_type: ($) =>
      seq("&", optional("mut"), choice($._type, $.dyn_type)),

    dyn_type: ($) => seq("dyn", field("bound", $._bound)),

    slice_type: ($) => seq("[", "]", optional("mut"), field("element", $._type)),

    array_type: ($) =>
      seq("[", field("length", $._expression), "]", field("element", $._type)),

    pointer_type: ($) => seq("*", optional("mut"), field("element", $._type)),

    optional_type: ($) => seq("?", field("element", $._type)),

    tuple_type: ($) =>
      seq("(", field("element", $._type), ",", commaSep1(field("element", $._type)), ")"),

    function_type: ($) =>
      seq(
        optional("extern"),
        "fn",
        "(",
        optional(commaSep($._type)),
        ")",
        optional(seq("->", field("result", $._type), optional(seq("!", field("error", $._type)))))
      ),

    path_type: ($) => seq($.path_base),
    path_base: ($) =>
      seq(
        repeat(seq(alias($.identifier, $.module_name), ".")),
        field("name", $.type_identifier),
        optional(seq(".", field("associated", $.type_identifier)))
      ),

    module_name: ($) => $.identifier,

    generic_type: ($) =>
      prec(
        2,
        seq(
          field("base", $.path_base),
          field("arguments", $.type_arguments),
          optional(seq(".", field("associated", $.type_identifier)))
        )
      ),

    type_arguments: ($) => seq("[", commaSep1($.generic_argument), "]"),

    generic_argument: ($) =>
      choice(
        $._type,
        seq(op($, "-"), field("value", $.integer)),
        field("value", $.integer),
        field("value", $.boolean),
        field("value", $.identifier),
        seq("{", field("value", $._expression), "}")
      ),

    // ---- statements ----
    block: ($) => seq("{", repeat($._statement), optional($._expression), "}"),

    _statement: ($) =>
      choice(
        $.let_statement,
        $.assignment_statement,
        $.expression_statement,
        $.if_expression,
        $.match_expression,
        $.while_statement,
        $.loop_statement,
        $.for_statement,
        $.return_statement,
        $.fail_statement,
        $.break_statement,
        $.continue_statement,
        $.defer_statement,
        $.scope_statement,
        $.select_statement,
        $.unsafe_expression,
        $.when_statement,
        $.block
      ),

    let_statement: ($) =>
      seq(
        choice("let", "var"),
        field("pattern", $._pattern),
        optional(seq(":", field("type", $._type))),
        "=",
        field("value", $._expression),
        ";"
      ),

    assignment_statement: ($) =>
      seq(
        field("left", choice($.wildcard_pattern, $._expression)),
        field("operator", op($, 
          "=", "+=", "-=", "*=", "/=", "%=",
          "+%=", "-%=", "*%=",
          "+|=", "-|=", "*|=",
          "&=", "|=", "^=", "<<=", ">>="
        )),
        field("right", $._expression),
        ";"
      ),

    expression_statement: ($) => seq(field("expression", $._expression), ";"),

    // `if` is one node in both positions: a statement has no `;` and may omit
    // `else`; an expression's branches hold a final expression. `block` allows
    // both, so a branch that ends in `return` is the same shape.
    if_expression: ($) =>
      seq(
        "if",
        field("condition", $.condition),
        field("then", $.block),
        optional(seq("else", field("alternative", choice($.block, $.if_expression))))
      ),

    condition: ($) => choice($.let_condition, $._expression),

    let_condition: ($) =>
      seq("let", field("pattern", $._pattern), "=", field("value", $._expression)),

    while_statement: ($) =>
      seq(optional($.label_definition), "while", field("condition", $.condition), field("body", $.block)),

    loop_statement: ($) =>
      seq(optional($.label_definition), "loop", field("body", $.block)),

    label_definition: ($) => seq(field("label", $.label), ":"),

    label: ($) => $.identifier,

    parallel_clause: ($) =>
      seq("par", optional(seq("(", "grain", ":", field("grain", $._expression), ")"))),

    for_statement: ($) =>
      seq(
        optional($.label_definition),
        optional($.parallel_clause),
        "for",
        field("item", $.identifier),
        optional(seq(",", field("index", $.identifier))),
        "in",
        field("iterable", $._expression),
        optional(
          seq(
            choice("..", "..="),
            field("range_end", $._expression),
            optional(seq("step", field("step", $._expression)))
          )
        ),
        field("body", $.block)
      ),

    return_statement: ($) =>
      seq("return", optional(field("value", $._expression)), ";"),

    fail_statement: ($) => seq("fail", field("value", $._expression), ";"),

    break_statement: ($) => seq("break", optional(field("label", $.label)), ";"),

    continue_statement: ($) => seq("continue", optional(field("label", $.label)), ";"),

    defer_statement: ($) => seq(choice("defer", "errdefer"), field("body", $.block)),

    scope_statement: ($) =>
      seq("scope", field("name", $.identifier), field("body", $.block)),

    select_statement: ($) =>
      seq(
        "select",
        "{",
        repeat($.select_arm),
        optional(seq("else", "=>", field("body", $.block))),
        "}"
      ),

    select_arm: ($) =>
      seq(
        field("pattern", $._pattern),
        "=",
        field("value", $._expression),
        "=>",
        field("body", $.block)
      ),

    unsafe_expression: ($) =>
      seq("unsafe", "(", field("reason", $.string), ")", field("body", $.block)),

    when_statement: ($) =>
      seq(
        "when",
        field("condition", $._expression),
        field("then", $.block),
        optional(seq("else", field("alternative", choice($.block, $.when_statement))))
      ),

    match_expression: ($) =>
      seq(
        "match",
        field("value", $._expression),
        "{",
        repeat($.match_arm),
        "}"
      ),

    match_arm: ($) =>
      seq(
        choice(
          seq(
            field("pattern", $._pattern),
            repeat(seq("|", field("pattern", $._pattern))),
            optional($.guard)
          ),
          "else"
        ),
        "=>",
        choice(
          field("body", $.block),
          seq(field("body", $._expression), optional(","))
        )
      ),

    guard: ($) => seq("if", field("condition", $._expression)),

    // ---- patterns ----
    _pattern: ($) =>
      choice(
        $.wildcard_pattern,
        $.identifier,
        $.literal,
        $.range_pattern,
        $.variant_pattern,
        $.tuple_pattern,
        $.struct_pattern
      ),

    wildcard_pattern: (_) => "_",

    range_pattern: ($) =>
      prec(1, seq(field("lo", $.literal), choice("..", "..="), field("hi", $.literal))),

    variant_pattern: ($) =>
      seq(
        optional(field("type", choice($.generic_type, $.path_type))),
        ".",
        field("name", $.identifier),
        optional(seq("(", field("payload", $._pattern), ")"))
      ),

    tuple_pattern: ($) =>
      seq("(", field("element", $._pattern), ",", commaSep1(field("element", $._pattern)), ")"),

    struct_pattern: ($) =>
      seq(
        optional(field("type", choice($.generic_type, $.path_type))),
        ".{",
        optional(
          seq(
            $.field_pattern,
            repeat(seq(",", $.field_pattern)),
            optional(",")
          )
        ),
        optional(".."),
        "}"
      ),

    field_pattern: ($) =>
      seq(field("name", $.identifier), optional(seq(":", field("pattern", $._pattern)))),

    // ---- expressions ----
    _expression: ($) =>
      choice(
        $.orelse_expression,
        $.catch_expression,
        $.binary_expression,
        $.unary_expression,
        $.try_expression,
        $.cast_expression,
        $.call_expression,
        $.field_expression,
        $.index_expression,
        $.slice_expression,
        $.deref_expression,
        $.type_application_expression,
        $.variant_expression,
        $.struct_expression,
        $.array_expression,
        $.tuple_expression,
        $.parenthesized_expression,
        $.closure_expression,
        $.if_expression,
        $.match_expression,
        $.spawn_expression,
        $.dispatch_expression,
        $.unsafe_expression,
        $.builtin_call,
        $.literal,
        $.self,
        $.identifier,
        $.path_type,
        $.generic_type
      ),

    orelse_expression: ($) =>
      prec.left(
        PREC.ORELSE,
        seq(
          field("value", $._expression),
          "orelse",
          field("handler", $.handler)
        )
      ),

    catch_expression: ($) =>
      prec.left(
        PREC.CATCH,
        seq(
          field("value", $._expression),
          "catch",
          field("name", choice($.identifier, $.wildcard_pattern)),
          "=>",
          field("handler", $.handler)
        )
      ),

    handler: ($) => choice($.block, $._expression),

    binary_expression: ($) =>
      choice(
        prec.left(
          PREC.OR,
          seq(field("left", $._expression), field("operator", op($, "||")), field("right", $._expression))
        ),
        prec.left(
          PREC.AND,
          seq(field("left", $._expression), field("operator", op($, "&&")), field("right", $._expression))
        ),
        prec.left(
          PREC.CMP,
          seq(
            field("left", $._expression),
            field("operator", op($, "==", "!=", "<", "<=", ">", ">=")),
            field("right", $._expression)
          )
        ),
        prec.left(
          PREC.BIT,
          seq(
            field("left", $._expression),
            field("operator", op($, "&", "|", "^")),
            field("right", $._expression)
          )
        ),
        prec.left(
          PREC.SHIFT,
          seq(field("left", $._expression), field("operator", op($, "<<", ">>")), field("right", $._expression))
        ),
        prec.left(
          PREC.ADD,
          seq(
            field("left", $._expression),
            field("operator", op($, "+", "-", "+%", "-%", "+|", "-|")),
            field("right", $._expression)
          )
        ),
        prec.left(
          PREC.MUL,
          seq(
            field("left", $._expression),
            field("operator", op($, "*", "/", "%", "*%", "*|")),
            field("right", $._expression)
          )
        )
      ),

    unary_expression: ($) =>
      prec.right(
        PREC.UNARY,
        seq(
          field("operator", op($, "-", "-%", "!", "~", seq("&", optional("mut")))),
          field("operand", $._expression)
        )
      ),

    try_expression: ($) =>
      prec.right(PREC.UNARY, seq("try", field("operand", $._expression))),

    cast_expression: ($) =>
      prec.left(
        PREC.CAST,
        seq(field("value", $._expression), "as", field("type", $._type))
      ),

    call_expression: ($) =>
      prec.left(
        PREC.POSTFIX,
        seq(field("function", $._expression), field("arguments", $.arguments))
      ),

    field_expression: ($) =>
      prec.left(
        PREC.POSTFIX,
        seq(
          field("value", $._expression),
          ".",
          field("field", choice($.identifier, $.integer))
        )
      ),

    index_expression: ($) =>
      prec.left(
        PREC.POSTFIX,
        seq(field("value", $._expression), "[", field("index", $._expression), "]")
      ),

    slice_expression: ($) =>
      prec.left(
        PREC.POSTFIX,
        seq(
          field("value", $._expression),
          "[",
          optional(field("lo", $._expression)),
          "..",
          optional(field("hi", $._expression)),
          "]"
        )
      ),

    deref_expression: ($) =>
      prec.left(PREC.POSTFIX, seq(field("value", $._expression), ".*")),

    type_application_expression: ($) =>
      prec.left(
        PREC.POSTFIX,
        seq(field("value", $._expression), field("arguments", $.explicit_type_arguments))
      ),

    explicit_type_arguments: ($) => seq(".[", commaSep1($.generic_argument), "]"),

    variant_expression: ($) =>
      choice(
        prec(
          PREC.POSTFIX + 1,
          seq(
            field("type", choice($.generic_type, $.path_type)),
            ".",
            field("name", $.identifier)
          )
        ),
        prec(
          2,
          seq(
            ".",
            field("name", $.identifier),
            field("arguments", $.arguments)
          )
        ),
        seq(".", field("name", $.identifier))
      ),

    arguments: ($) => seq("(", optional(commaSep($._expression)), ")"),

    parenthesized_expression: ($) =>
      seq("(", field("value", $._expression), ")"),

    tuple_expression: ($) =>
      seq("(", field("element", $._expression), ",", commaSep1(field("element", $._expression)), ")"),

    struct_expression: ($) =>
      seq(
        optional(field("type", choice($.generic_type, $.path_type))),
        ".{",
        optional(
          seq(
            field("field", $.field_initializer),
            repeat(seq(",", field("field", $.field_initializer))),
            optional(",")
          )
        ),
        "}"
      ),

    field_initializer: ($) =>
      seq(field("name", $.identifier), ":", field("value", $._expression)),

    array_expression: ($) =>
      seq(
        "[",
        field("length", $._expression),
        "]",
        field("element", $._type),
        ".{",
        optional(
          seq(
            field("element", $._expression),
            repeat(seq(",", field("element", $._expression))),
            optional(",")
          )
        ),
        "}"
      ),

    capture: ($) =>
      seq(optional(seq("&", optional("mut"))), field("name", $.identifier)),

    closure_expression: ($) =>
      seq(
        "fn",
        "[",
        optional(commaSep($.capture)),
        "]",
        field("parameters", $.parameters),
        optional(field("result", $.result)),
        field("body", $.block)
      ),

    spawn_expression: ($) =>
      seq(
        "spawn",
        field("call", $.call_expression),
        "in",
        field("scope", $._expression)
      ),

    dispatch_expression: ($) =>
      seq(
        "dispatch",
        field("call", $.call_expression),
        "over",
        field("domain", $.domain),
        "on",
        field("executor", $._expression)
      ),

    domain: ($) =>
      seq(
        field("range", seq($._expression, "..", $._expression)),
        repeat(seq(",", field("range", seq($._expression, "..", $._expression))))
      ),

    builtin_call: ($) =>
      prec.right(
        PREC.POSTFIX + 1,
        seq(
          field("name", $.builtin),
          optional(seq("[", field("type", $._type), "]")),
          optional(field("arguments", $.builtin_arguments))
        )
      ),

    builtin_arguments: ($) =>
      seq(
        "(",
        optional(
          seq(
            choice($._type, $._expression),
            repeat(seq(",", $._expression))
          )
        ),
        ")"
      ),

    literal: ($) =>
      choice($.integer, $.float, $.string, $.cstring, $.char_literal, $.boolean),

    // `self` is not in the keyword list: it is a value name with one special use.
    // A token of its own gives queries a node to colour without colouring every
    // identifier.
    self: (_) => token(prec(1, "self")),
  },
});
