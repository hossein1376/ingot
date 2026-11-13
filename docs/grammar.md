# Ingot v1 grammar

The grammar is LL(1) apart from two places that need one more token of lookahead, both
noted below. Terminals in capitals are tokens from section 1: `IDENT` (lowercase
initial), `TYPE` (uppercase initial), `INT`, `FLOAT`, `STRING`, `CSTRING`, `BUILTIN`
(`@name`), `SCALAR` (`i8` ... `f64`, `bool`, `void`, `never`) and `VECTOR` (`f32x4`,
`boolx8`, ...). The words `step`, `over`, `on`, `reduce` and `grain` are contextual:
they are keywords only in the positions shown and are ordinary identifiers elsewhere.

```ebnf
file         = { import } { item } ;
import       = "import" IDENT { "." IDENT } [ "." TYPE ] [ "as" ( IDENT | TYPE ) ] ";" ;

item         = [ "pub" ] ( fn_decl | struct_decl | enum_decl | interface_decl
                         | const_decl | static_decl | type_decl )
             | impl_decl | extern_block | test_decl | bench_decl | when_items ;

(* ---- functions ---- *)
fn_decl      = { fn_mod } "fn" IDENT [ gparams ] "(" [ params ] ")" [ result ] ( block | ";" ) ;
fn_mod       = "inline" | "noinline" | "unsafe" | "export" | "kernel" | "waitable" ;
params       = param { "," param } [ "," ] ;
param        = IDENT ":" type [ "into" IDENT ] ;
result       = "->" type [ "!" type ] [ "from" origin { "," origin } ] [ "reduce" "(" reduce_op ")" ] ;
origin       = IDENT [ ".*" ] | "static" ;
reduce_op    = "+" | "*" | "&" | "|" | "^" | IDENT ;          (* IDENT: min, max *)

gparams      = "[" gparam { "," gparam } "]" ;
gparam       = TYPE [ ":" bound { "+" bound } ] | "const" TYPE ":" type ;
bound        = path_type | fn_bound ;
fn_bound     = ( "Fn" | "FnMut" | "FnOnce" ) "(" [ type { "," type } ] ")" [ "->" type [ "!" type ] ] ;

(* ---- types ---- *)
struct_decl  = [ "copy" | "linear" ] "struct" TYPE [ gparams ] [ align ] "{" [ field { "," field } [ "," ] ] "}" ;
field        = [ "pub" ] IDENT ":" type [ align ] ;
enum_decl    = [ "copy" ] "enum" TYPE [ gparams ] [ ":" type ] "{" [ variant { "," variant } [ "," ] ] "}" ;
variant      = IDENT [ ":" [ "from" ] type ] [ "=" expr ] ;
interface_decl = "interface" TYPE [ gparams ] [ ":" bound { "+" bound } ] "{" { iface_member } "}" ;
iface_member = "type" TYPE ";" | fn_decl ;
impl_decl    = [ "unsafe" ] "impl" [ gparams ] ( type | path_type "for" type ) "{" { [ "pub" ] ( fn_decl | const_decl | type_decl ) } "}" ;
const_decl   = "const" IDENT ":" type "=" expr ";" ;
static_decl  = "static" IDENT ":" type [ align ] "=" expr ";" ;
type_decl    = "type" TYPE [ gparams ] "=" type ";" ;
align        = "align" "(" expr ")" ;

extern_block = "extern" STRING "{" { "fn" IDENT "(" [ ext_params ] ")" [ "->" type ] ";" } "}" ;
ext_params   = param { "," param } [ "," "..." ] | "..." ;
test_decl    = "test" STRING block ;
bench_decl   = "bench" STRING block ;
when_items   = "when" expr "{" { item } "}" [ "else" ( "{" { item } "}" | when_items ) ] ;

type         = SCALAR | VECTOR
             | path_type
             | "&" [ "mut" ] ( type | "dyn" bound )
             | "[" "]" [ "mut" ] type
             | "[" expr "]" type
             | "*" [ "mut" ] type
             | "?" type
             | "(" type "," type { "," type } ")"
             | [ "extern" ] "fn" "(" [ type { "," type } ] ")" [ "->" type [ "!" type ] ] ;
path_type    = { IDENT "." } TYPE [ gargs ] [ "." TYPE ] ;   (* trailing .TYPE: associated type *)
gargs        = "[" garg { "," garg } "]" ;
garg         = type | INT | "true" | "false" | "{" expr "}" ;  (* a TYPE alone is a type or a const parameter *)

(* ---- statements ---- *)
block        = "{" { stmt } "}" ;
stmt         = ( "let" | "var" ) pattern [ ":" type ] "=" expr ";"
             | "_" "=" expr ";"
             | place assign_op expr ";"
             | expr ";"                                        (* a call, optionally under try or with catch *)
             | if_stmt | match_stmt
             | [ IDENT ":" ] loop_stmt
             | "return" [ expr ] ";" | "fail" expr ";"
             | "break" [ IDENT ] ";" | "continue" [ IDENT ] ";"
             | ( "defer" | "errdefer" ) block
             | "scope" IDENT block
             | "spawn" call_expr "in" expr ";"
             | "select" "{" { pattern "=" call_expr "=>" block } [ "else" "=>" block ] "}"
             | "dispatch" call_expr "over" domain "on" expr ";"
             | "unsafe" "(" STRING ")" block
             | when_stmt
             | block ;
when_stmt    = "when" expr block [ "else" ( block | when_stmt ) ] ;
assign_op    = "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "+%=" | "-%=" | "*%="
             | "+|=" | "-|=" | "*|=" | "&=" | "|=" | "^=" | "<<=" | ">>=" ;

if_stmt      = "if" cond block [ "else" ( block | if_stmt ) ] ;
cond         = expr | "let" pattern "=" expr ;
loop_stmt    = "while" cond block
             | "loop" block
             | [ "par" [ "(" "grain" ":" expr ")" ] ] "for" IDENT [ "," IDENT ] "in" expr [ ( ".." | "..=" ) expr [ "step" expr ] ] block ;
match_stmt   = "match" expr "{" { arm } "}" ;
arm          = ( pattern { "|" pattern } [ "if" expr ] | "else" ) "=>" ( block | expr "," ) ;

pattern      = "_" | IDENT | literal | literal ( ".." | "..=" ) literal
             | [ path_type ] "." IDENT [ "(" pattern ")" ]
             | "(" pattern "," pattern { "," pattern } ")"
             | path_type ".{" [ fpat { "," fpat } [ "," ] ] [ ".." ] "}" ;
fpat         = IDENT [ ":" pattern ] ;

(* ---- expressions ---- *)
expr         = or_expr [ "orelse" handler | "catch" ( IDENT | "_" ) "=>" handler ] ;
handler      = block | or_expr ;
or_expr      = and_expr { "||" and_expr } ;
and_expr     = cmp_expr { "&&" cmp_expr } ;
cmp_expr     = bit_expr [ ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) bit_expr ] ;
bit_expr     = shift_expr { ( "&" | "|" | "^" ) shift_expr } ;
shift_expr   = add_expr { ( "<<" | ">>" ) add_expr } ;
add_expr     = mul_expr { ( "+" | "-" | "+%" | "-%" | "+|" | "-|" ) mul_expr } ;
mul_expr     = cast_expr { ( "*" | "/" | "%" | "*%" | "*|" ) cast_expr } ;
cast_expr    = prefix_expr { "as" type } ;
prefix_expr  = ( "-" | "-%" | "!" | "~" | "&" [ "mut" ] | "try" ) prefix_expr | postfix_expr ;
postfix_expr = primary { "(" [ args ] ")" | "[" expr [ ".." expr ] "]"
                       | "." IDENT | "." INT | ".*" | ".[" type { "," type } "]" } ;
call_expr    = postfix_expr ;                                  (* must end in a call *)
place        = postfix_expr ;                                  (* must be a place, section 5.5 *)
args         = expr { "," expr } [ "," ] ;

primary      = IDENT | literal | BUILTIN [ "(" [ builtin_args ] ")" ]
             | "(" expr ")" | "(" expr "," expr { "," expr } ")"
             | path_type [ ".{" [ inits ] "}" ]                (* a type as a value: T.f(), T.{..}, a const parameter *)
             | "[" expr "]" type ".{" [ args ] "}"             (* array literal *)
             | "." IDENT [ "(" expr ")" ] | ".{" [ inits ] "}" (* expected-type shorthand *)
             | "fn" "[" [ capture { "," capture } ] "]" "(" [ params ] ")" [ result ] block
             | "if" expr "{" expr "}" "else" ( "{" expr "}" | if_expr_tail )
             | "match" expr "{" { arm } "}"
             | "spawn" call_expr "in" expr
             | "dispatch" call_expr "over" domain "on" expr
             | "unsafe" "(" STRING ")" "{" expr "}" ;
domain       = or_expr ".." or_expr { "," or_expr ".." or_expr } ;   (* one range per dimension *)
if_expr_tail = "if" expr "{" expr "}" "else" ( "{" expr "}" | if_expr_tail ) ;
capture      = [ "&" [ "mut" ] ] IDENT ;
inits        = IDENT ":" expr { "," IDENT ":" expr } [ "," ] ;
builtin_args = ( type | expr ) { "," expr } ;
literal      = INT | FLOAT | STRING | CSTRING | "true" | "false" ;
```

## Notes

* **Two tokens of lookahead** are needed in two places. In a statement, `IDENT ":"`
  followed by `for`, `while` or `loop` is a label. In `gargs`, a `TYPE` token alone may
  be a type or a constant parameter, which name resolution decides.
* **No ambiguity between a type and a value.** A `TYPE` token never begins a value
  expression except as the start of `path_type`, which is then followed by `.name`,
  `.{` or nothing (a constant parameter). `Name[` after a `TYPE` is always type
  application, and `name[` after an `IDENT` is always indexing. Explicit type arguments
  on a function use `.[`.
* **Statement or expression.** `if`, `match`, `spawn`, `dispatch` and `unsafe` begin a
  statement when they appear in statement position, and an expression elsewhere. In
  expression form each branch of `if` and each arm of `match` holds exactly one
  expression.
* **Struct literals** always use `.{`, so `if x == y { ... }` never needs lookahead to
  tell a literal from a block.
* **The parenthesization rule** (section 5.1) and the ban on chained comparisons are
  enforced by the parser on top of this grammar.
* **`step`** takes a positive constant expression.
* **Tuple fields.** After a `.`, the lexer reads digits as an integer field index, so
  `t.0.1` is never lexed as a float.
* **`..=`** is allowed in `for` ranges and in patterns. Slices and `dispatch` domains
  use `..` only.
* **Trailing commas** are allowed wherever a comma-separated list is closed by a
  bracket on a later line, and `ingot fmt` adds them.
