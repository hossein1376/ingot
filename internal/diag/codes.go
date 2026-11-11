package diag

// Code documents one stable diagnostic code for `ingot explain`.
type Code struct {
	Code    string `json:"code"`
	Title   string `json:"title"`
	Explain string `json:"explain"`
}

// Codes is the catalog of every diagnostic code the compiler emits. A test
// checks that each code used in the compiler source has an entry here.
var Codes = []Code{
	{"E0001", "bad builtin token", "'@' must be followed by a builtin name, e.g. @assert. Run `ingot builtins` for the list."},
	{"E0002", "bad string literal", "A string is unterminated or has an unknown escape. Supported escapes: \\n \\t \\r \\0 \\\\ \\\" \\' \\xHH."},
	{"E0003", "import not found", "Import paths are relative to the importing file, or start with std/ for the embedded standard library."},
	{"E0004", "no entry point", "A program needs `pub fn main() -> i32`. Use `ingot test` to run a file's test blocks without a main."},
	{"E0005", "duplicate module alias", "Each `import \"p\" as name;` in a file needs a different name."},
	{"E0100", "unexpected token", "The parser expected a specific token (often ';' or a closing bracket). The fix inserts it when that is unambiguous."},
	{"E0101", "expected identifier", "A name is required here (declaration, field, parameter or loop variable)."},
	{"E0102", "import needs a path", "Write `import \"path.ig\";` with a string literal."},
	{"E0103", "array length must be a literal", "Array lengths are integer literals, so layouts are known without evaluation: [16]f32."},
	{"E0104", "expected a type", "A type is required here: a primitive, a struct or enum name, *T, [N]T, []T, a vector or fn(...)."},
	{"E0105", "uninitialized variable", "Every variable must be initialized. Use `= @zero` to make zero-initialization explicit."},
	{"E0106", "ambiguous operator mix", "Bitwise and shift operators may not be mixed with other binary operators without parentheses: write (a & b) == 0, not a & b == 0."},
	{"E0107", "mixed literal fields", "A composite literal names all its fields or none: T.{ x: 1, y: 2 }."},
	{"E0108", "array literal syntax", "Array literals are written [N]T.{ a, b, c }."},
	{"E0109", "expected an expression", "An expression is required here."},
	{"E0110", "duplicate else arm", "A switch has at most one `else =>` arm."},
	{"E0200", "duplicate declaration", "A name (type, function, field, enum member or local) is declared twice in one scope, or an extern fn is redeclared with a different signature."},
	{"E0201", "recursive struct", "A struct cannot contain itself by value. Store a pointer instead, or declare the inner struct first."},
	{"E0202", "void field", "Fields must have a sized type."},
	{"E0203", "unknown type", "The type name is not declared. The note may suggest a close match (e.g. i64 for int)."},
	{"E0204", "cannot infer type", "Add a type annotation: `let x: T = ...`. Inference only covers non-literal initializers."},
	{"E0205", "type mismatch", "Ingot has no implicit conversions. Convert explicitly with `as`; the fix inserts the cast when it is lossless or the intent is clear."},
	{"E0206", "bad literal", "An integer literal does not fit the expected type or 64 bits, or a float literal is invalid."},
	{"E0207", "variadic non-extern", "Only extern (C) functions may be variadic."},
	{"E0208", "struct by value to C", "Extern parameters must be scalars or pointers; pass a pointer to the struct."},
	{"E0209", "bad constant", "Constants must have a numeric or bool type and a compile-time value."},
	{"E0210", "bad main", "The entry point is `pub fn main() -> i32`."},
	{"E0211", "shadowing", "A local may not reuse the name of an outer local. Choose a different name."},
	{"E0212", "undefined name", "The name is not declared in any enclosing scope or imported file."},
	{"E0213", "invalid cast", "`as` converts between numeric types, pointers, enums and their base type, and pointer/u64. Other conversions need an explicit function."},
	{"E0214", "not callable", "Only functions and fn pointers can be called."},
	{"E0215", "builtin arity", "A builtin was called with the wrong number of arguments. The message shows its signature."},
	{"E0216", "bad variadic argument", "Arguments to a variadic C function must be scalars or pointers."},
	{"E0217", "bad member access", "The field or enum member does not exist, or the base has no fields. Pointers are dereferenced explicitly: p.*.x."},
	{"E0218", "deref of non-pointer", "Only pointers can be dereferenced with .*"},
	{"E0219", "bad index", "Indices are i64, and only arrays, slices, pointers and vectors can be indexed."},
	{"E0220", "constant index out of bounds", "A constant index is outside the array or vector length."},
	{"E0221", "unnamed struct fields", "Struct literals must name every field: T.{ a: 1, b: 2 }."},
	{"E0222", "missing struct fields", "Every field must be initialized explicitly; use @zero for a field you want zeroed."},
	{"E0223", "division by zero", "Division or remainder by a constant zero."},
	{"E0230", "unknown builtin", "Run `ingot builtins` for the list of builtins and their signatures."},
	{"E0231", "builtin needs a pointer", "This builtin takes a pointer argument."},
	{"E0232", "bad atomic type", "Atomics operate on *i32, *u32, *i64 or *u64."},
	{"E0240", "bad enum base", "An enum's base type must be an integer type: enum Op: u8 { ... }."},
	{"E0241", "duplicate enum value", "Two enum members have the same value."},
	{"E0242", "invalid error type", "The type after '!' must be an enum whose members are all non-zero (0 means success). Extern functions cannot be fallible, and fallible functions return scalars, pointers or void."},
	{"E0244", "invalid inline fn", "An inline fn cannot be extern, variadic or fallible, and its parameters and result must be scalars, pointers or vectors. Pass aggregates by pointer."},
	{"E0245", "recursive inline fn", "An inline fn that reaches itself through calls to inline fns cannot be expanded. Remove 'inline' from one of them."},
	{"E0250", "unhandled error", "A call to a fallible function (-> T ! E) must be wrapped in `try` (propagate) or `catch e => ...` (handle). The fix inserts `try` when the enclosing function fails with the same error enum."},
	{"E0251", "try cannot propagate", "try returns the callee's error from the enclosing function, so that function must be declared `-> T ! E` with the same E. Otherwise use catch, and convert with `return @err(...)`."},
	{"E0252", "invalid catch", "A catch block for a call with a result must leave (return, break, continue or @panic); to continue with a value write `catch e => fallback`. A void call has no value to fall back to, so it needs a block."},
	{"E0253", "misplaced @err", "@err(code) is only valid as `return @err(E.member);` inside a function declared `-> T ! E`."},
	{"E0301", "assignment to immutable", "Bindings declared with `let` cannot be assigned; the fix changes the declaration to `var`."},
	{"E0302", "invalid operator", "The operator is not defined for this type (e.g. arithmetic on enums or pointers, shift by a non-integer)."},
	{"E0303", "no-effect statement", "Only calls may be used as expression statements."},
	{"E0304", "bad for range", "for-range bounds must be integers of one type."},
	{"E0305", "bad return", "A return value is missing, or given in a void function."},
	{"E0306", "break outside loop", "break and continue must be inside while or for."},
	{"E0307", "non-bool condition", "Ingot has no truthiness. Compare explicitly: x != 0, p != null."},
	{"E0308", "invalid defer body", "A defer body may not return, break/continue out of itself, or contain another defer."},
	{"E0401", "missing return", "A non-void function can reach the end of its body. Add a return or @unreachable()."},
	{"E0500", "bad switch value", "switch works on integers and enums."},
	{"E0501", "non-constant case", "switch cases must be compile-time constants."},
	{"E0502", "duplicate case", "A value appears in more than one case."},
	{"E0503", "switch needs else", "A switch on a plain integer must have an `else =>` arm."},
	{"E0504", "non-exhaustive switch", "A switch on an enum must cover every member or have an else arm; the fix inserts the missing arms."},
	{"E0900", "expression too deep", "The code generator parks temporaries in the frame when registers run low (binary operands and call arguments), so this only remains for shapes it cannot spill yet, such as calls with more than 10 non-leaf arguments. Split the expression into named locals."},
}

// LookupCode returns the catalog entry for code.
func LookupCode(code string) (Code, bool) {
	for _, c := range Codes {
		if c.Code == code {
			return c, true
		}
	}
	return Code{}, false
}
