package syntax

import "github.com/hossein1376/ingot/internal/diag"

// ---- Types (syntactic) ----

// TypeExpr is a syntactic type.
type TypeExpr interface{ TSpan() diag.Span }

type (
	NamedType struct {
		Name string
		Sp   diag.Span
	}
	PtrType struct {
		Elem TypeExpr
		Sp   diag.Span
	}
	ArrayType struct {
		Len  int64
		Elem TypeExpr
		Sp   diag.Span
	}
	SliceType struct {
		Elem TypeExpr
		Sp   diag.Span
	}
	FnType struct {
		Params   []TypeExpr
		Variadic bool
		Ret      TypeExpr // nil = void
		Sp       diag.Span
	}
)

func (t *NamedType) TSpan() diag.Span { return t.Sp }
func (t *PtrType) TSpan() diag.Span   { return t.Sp }
func (t *ArrayType) TSpan() diag.Span { return t.Sp }
func (t *SliceType) TSpan() diag.Span { return t.Sp }
func (t *FnType) TSpan() diag.Span    { return t.Sp }

// ---- Expressions ----

// Expr is an expression node. Sema fills in the type via the checker's side table.
type Expr interface{ ESpan() diag.Span }

type (
	IdentExpr struct {
		Name string
		Sp   diag.Span
	}
	IntLit struct {
		Text string
		Sp   diag.Span
	}
	FloatLit struct {
		Text string
		Sp   diag.Span
	}
	BoolLit struct {
		Val bool
		Sp  diag.Span
	}
	NullLit   struct{ Sp diag.Span }
	StringLit struct {
		Val string
		C   bool // c"..." -> *u8 (NUL-terminated)
		Sp  diag.Span
	}
	UnaryExpr struct {
		Op string // - ! ~ &
		X  Expr
		Sp diag.Span
	}
	BinaryExpr struct {
		Op     string
		X, Y   Expr
		Sp     diag.Span
		Parens bool // written inside parentheses
	}
	CallExpr struct {
		Fn   Expr
		Args []Expr
		Sp   diag.Span
	}
	BuiltinCall struct {
		Name string // without '@'
		Type TypeExpr
		Args []Expr
		Sp   diag.Span
	}
	FieldExpr struct {
		X    Expr
		Name string
		Sp   diag.Span
	}
	DerefExpr struct { // p.*
		X  Expr
		Sp diag.Span
	}
	IndexExpr struct {
		X, Index Expr
		Sp       diag.Span
	}
	SliceExpr struct { // x[lo..hi]
		X, Lo, Hi Expr
		Sp        diag.Span
	}
	CastExpr struct {
		X  Expr
		To TypeExpr
		Sp diag.Span
	}
	CompositeLit struct { // T.{ a: 1, b: 2 } or [N]T.{1, 2}
		Type   TypeExpr
		Names  []string // empty for positional
		Values []Expr
		Sp     diag.Span
	}
	ParenExpr struct {
		X  Expr
		Sp diag.Span
	}
)

func (e *IdentExpr) ESpan() diag.Span    { return e.Sp }
func (e *IntLit) ESpan() diag.Span       { return e.Sp }
func (e *FloatLit) ESpan() diag.Span     { return e.Sp }
func (e *BoolLit) ESpan() diag.Span      { return e.Sp }
func (e *NullLit) ESpan() diag.Span      { return e.Sp }
func (e *StringLit) ESpan() diag.Span    { return e.Sp }
func (e *UnaryExpr) ESpan() diag.Span    { return e.Sp }
func (e *BinaryExpr) ESpan() diag.Span   { return e.Sp }
func (e *CallExpr) ESpan() diag.Span     { return e.Sp }
func (e *BuiltinCall) ESpan() diag.Span  { return e.Sp }
func (e *FieldExpr) ESpan() diag.Span    { return e.Sp }
func (e *DerefExpr) ESpan() diag.Span    { return e.Sp }
func (e *IndexExpr) ESpan() diag.Span    { return e.Sp }
func (e *SliceExpr) ESpan() diag.Span    { return e.Sp }
func (e *CastExpr) ESpan() diag.Span     { return e.Sp }

// TryExpr is `try f(...)`: on error, run defers and return the error.
type TryExpr struct {
	X  Expr // a call to a fallible function
	Sp diag.Span
}

// CatchExpr is `f(...) catch e => { ... }` (the block must leave) or
// `f(...) catch e => fallback` (a value of the call's result type).
type CatchExpr struct {
	X        Expr
	Name     string // "_" to ignore the error
	NameSp   diag.Span
	Handler  *Block // nil when Fallback is used
	Fallback Expr
	Sp       diag.Span
}

func (e *TryExpr) ESpan() diag.Span   { return e.Sp }
func (e *CatchExpr) ESpan() diag.Span { return e.Sp }
func (e *CompositeLit) ESpan() diag.Span { return e.Sp }
func (e *ParenExpr) ESpan() diag.Span    { return e.Sp }

// ---- Statements ----

type Stmt interface{ SSpan() diag.Span }

type (
	LetStmt struct {
		Mutable bool
		Name    string
		Type    TypeExpr // may be nil
		Value   Expr
		Sp      diag.Span
		NameSp  diag.Span
	}
	AssignStmt struct {
		Op     string // "=" or "+=" ...
		Target Expr
		Value  Expr
		Sp     diag.Span
	}
	ExprStmt struct {
		X  Expr
		Sp diag.Span
	}
	IfStmt struct {
		Cond Expr
		Then *Block
		Else Stmt // *Block or *IfStmt or nil
		Sp   diag.Span
	}
	WhileStmt struct {
		Cond Expr
		Body *Block
		Sp   diag.Span
	}
	ForStmt struct { // for i in lo..hi { }
		Var    string
		Lo, Hi Expr
		Body   *Block
		Sp     diag.Span
	}
	ReturnStmt struct {
		Value Expr // may be nil
		Sp    diag.Span
	}
	SwitchStmt struct {
		X     Expr
		Cases []*SwitchCase
		Else  *Block // nil if absent
		Sp    diag.Span
		// Exhaustive is set by the checker when every enum member is covered.
		Exhaustive bool
	}
	// DeferStmt runs Body when the enclosing block exits (normally or via
	// return/break/continue), in reverse order of declaration.
	DeferStmt struct {
		Body *Block
		Sp   diag.Span
	}
	BreakStmt    struct{ Sp diag.Span }
	ContinueStmt struct{ Sp diag.Span }
	Block        struct {
		Stmts []Stmt
		Sp    diag.Span
	}
)

func (s *LetStmt) SSpan() diag.Span    { return s.Sp }
func (s *AssignStmt) SSpan() diag.Span { return s.Sp }
func (s *ExprStmt) SSpan() diag.Span   { return s.Sp }
func (s *IfStmt) SSpan() diag.Span     { return s.Sp }
func (s *WhileStmt) SSpan() diag.Span  { return s.Sp }
func (s *ForStmt) SSpan() diag.Span    { return s.Sp }
func (s *ReturnStmt) SSpan() diag.Span { return s.Sp }
func (s *BreakStmt) SSpan() diag.Span  { return s.Sp }
func (s *SwitchStmt) SSpan() diag.Span { return s.Sp }

// SwitchCase is `v1, v2 => { ... }`.
type SwitchCase struct {
	Values []Expr
	Body   *Block
	Sp     diag.Span
}

func (s *ContinueStmt) SSpan() diag.Span { return s.Sp }
func (s *DeferStmt) SSpan() diag.Span    { return s.Sp }
func (s *Block) SSpan() diag.Span        { return s.Sp }

// ---- Declarations ----

type Param struct {
	Name string
	Type TypeExpr
	Sp   diag.Span
}

type FnDecl struct {
	Pub      bool
	Extern   bool
	Name     string
	Params   []Param
	Variadic bool
	Ret      TypeExpr // nil = void
	Err      TypeExpr // error enum after '!', nil if the function cannot fail
	Body     *Block   // nil for extern
	Sp       diag.Span
	NameSp   diag.Span
}

type Field struct {
	Name string
	Type TypeExpr
	Sp   diag.Span
}

type StructDecl struct {
	Name   string
	Fields []Field
	Sp     diag.Span
}

type GlobalDecl struct { // const NAME: T = expr;  or  var NAME: T = @zero;
	Const bool
	Name  string
	Type  TypeExpr
	Value Expr
	Sp    diag.Span
}

type EnumDecl struct {
	Name    string
	Base    TypeExpr // underlying integer type; nil = i32
	Members []EnumMember
	Sp      diag.Span
}

type EnumMember struct {
	Name  string
	Value Expr // optional explicit value
	Sp    diag.Span
}

type TestDecl struct {
	Name string
	Body *Block
	Sp   diag.Span
}

// File is a parsed source file.
type File struct {
	Name    string
	Fns     []*FnDecl
	Structs []*StructDecl
	Globals []*GlobalDecl
	Tests   []*TestDecl
	Imports []*ImportDecl
	Enums   []*EnumDecl
}

// ImportDecl is `import "path";`.
type ImportDecl struct {
	Path string
	Sp   diag.Span
}

// Node is any AST node (used as a map key by later passes).
type Node any
