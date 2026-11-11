package sema

import (
	"math"
	"strconv"

	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/syntax"
)

// Var is a local, parameter or global variable (or a named constant).
type Var struct {
	Name    string
	Type    *Type
	Mutable bool
	Global  bool
	Const   bool
	Val     Const // for Const
	Decl    syntax.Node
}

// Const is a compile-time constant value.
type Const struct {
	IsFloat bool
	I       int64
	F       float64
}

// Func is a function symbol.
type Func struct {
	Name   string
	Type   *Type
	Decl   *syntax.FnDecl
	Extern bool
	Pub    bool
}

// Body is a function or test body to generate code for.
type Body struct {
	Name   string // symbol name (without platform prefix)
	Func   *Func  // nil for tests
	Test   *syntax.TestDecl
	Params []*Var
	Locals []*Var // includes params
	Ret    *Type
	Err    *Type // error enum if the function is fallible
	Block  *syntax.Block
}

// Info is the result of type checking.
type Info struct {
	Types   map[syntax.Expr]*Type
	Consts  map[syntax.Expr]Const // folded constant expressions
	Uses    map[*syntax.IdentExpr]any
	Defs    map[syntax.Node]*Var // *LetStmt, *ForStmt
	Funcs   []*Func
	Bodies  []*Body
	Globals []*Var
	Tests   []*Body
}

type scope struct {
	parent *scope
	names  map[string]any
}

func (s *scope) lookup(n string) any {
	for ; s != nil; s = s.parent {
		if v, ok := s.names[n]; ok {
			return v
		}
	}
	return nil
}

type checker struct {
	dl      *diag.List
	info    *Info
	enums   map[string]*Type
	structs map[string]*Type
	global  *scope
	sc      *scope
	body    *Body
	loops   int
	// inDefer is set while checking a defer body; loopsAtDefer is the loop
	// depth outside it (break/continue may not leave the defer body).
	inDefer      bool
	handled      map[*syntax.CallExpr]bool // fallible calls wrapped in try/catch
	inlineCalls  map[*Func][]*Func         // inline fn -> inline fns it calls directly
	loopsAtDefer int
}

// Check type-checks f.
func Check(f *syntax.File, dl *diag.List) *Info {
	c := &checker{dl: dl, structs: map[string]*Type{}, enums: map[string]*Type{}, global: &scope{names: map[string]any{}}}
	c.info = &Info{Types: map[syntax.Expr]*Type{}, Consts: map[syntax.Expr]Const{}, Uses: map[*syntax.IdentExpr]any{}, Defs: map[syntax.Node]*Var{}}
	// 0. enums
	for _, ed := range f.Enums {
		c.enumDecl(ed)
	}
	// 1. struct names, then layouts (in order; fields must reference earlier or pointer types)
	for _, sd := range f.Structs {
		if _, dup := c.structs[sd.Name]; dup || primitives[sd.Name] != nil || c.enums[sd.Name] != nil {
			c.dl.Errorf("E0200", sd.Sp, "duplicate type name %q", sd.Name)
			continue
		}
		c.structs[sd.Name] = &Type{Kind: Struct, Name: sd.Name}
	}
	for _, sd := range f.Structs {
		c.layout(sd, map[string]bool{})
	}
	// 2. function signatures and globals
	for _, fd := range f.Fns {
		ft := &Type{Kind: Fn, Size: 8, Align: 8, Variadic: fd.Variadic, Ret: TVoid}
		for _, p := range fd.Params {
			ft.Params = append(ft.Params, c.resolve(p.Type))
		}
		if fd.Ret != nil {
			ft.Ret = c.resolve(fd.Ret)
		}
		if fd.Err != nil {
			et := c.resolve(fd.Err)
			switch {
			case fd.Extern:
				c.dl.Errorf("E0242", fd.Err.TSpan(), "extern (C) functions cannot have an error type")
			case et.Enum == nil:
				c.dl.Errorf("E0242", fd.Err.TSpan(), "the error type after '!' must be an enum, found %s", et)
			case ft.Ret.IsAggregate() || ft.Ret.Kind == Vec:
				c.dl.Errorf("E0242", fd.Ret.TSpan(), "a fallible function must return a scalar, pointer or void (aggregate results are not supported yet); return through an out-pointer")
			default:
				for i, v := range et.Enum.Values {
					if v == 0 {
						c.dl.Errorf("E0242", fd.Err.TSpan(), "error enum %s has member %s = 0; 0 means success, so error values must be non-zero", et, et.Enum.Names[i])
					}
				}
				ft.Err = et
			}
		}
		if fd.Inline {
			bad := fd.Extern || fd.Variadic || fd.Err != nil || ft.Ret.IsAggregate()
			for _, p := range ft.Params {
				bad = bad || p.IsAggregate()
			}
			if bad {
				c.dl.Errorf("E0244", fd.NameSp, "inline fn %q must not be extern, variadic or fallible, and its parameters and result must be scalars, pointers or vectors (pass aggregates by pointer)", fd.Name)
			}
		}
		if fd.Variadic && !fd.Extern {
			c.dl.Errorf("E0207", fd.NameSp, "only extern functions may be variadic")
		}
		fn := &Func{Name: fd.Name, Type: ft, Decl: fd, Extern: fd.Extern, Pub: fd.Pub}
		if prev, ok := c.global.names[fd.Name].(*Func); ok && prev.Extern && fd.Extern {
			// the same C function may be declared by several modules
			if !Identical(prev.Type, ft) {
				c.dl.Errorf("E0200", fd.NameSp, "extern fn %q redeclared with a different signature: %s vs %s", fd.Name, ft, prev.Type)
			}
			c.info.Funcs = append(c.info.Funcs, prev)
			continue
		}
		if fd.Extern {
			for i, p := range ft.Params {
				if p.IsAggregate() {
					c.dl.Errorf("E0208", fd.Params[i].Sp, "extern function parameters must be scalars or pointers (struct-by-value C ABI is not supported yet); pass a pointer instead")
				}
			}
			if ft.Ret.IsAggregate() {
				c.dl.Errorf("E0208", fd.Ret.TSpan(), "extern functions must return a scalar, pointer or void")
			}
		}
		c.declare(fd.Name, fn, fd.NameSp)
		c.info.Funcs = append(c.info.Funcs, fn)
	}
	for _, g := range f.Globals {
		v := &Var{Name: g.Name, Type: c.resolve(g.Type), Global: true, Const: g.Const, Mutable: !g.Const, Decl: g}
		c.sc = c.global
		t := c.expr(g.Value, v.Type)
		c.assignable(t, v.Type, g.Value)
		if g.Const {
			if !v.Type.IsNumeric() && v.Type.Kind != Bool {
				c.dl.Errorf("E0209", g.Sp, "constants must have a numeric or bool type")
			}
			if k, ok := c.info.Consts[g.Value]; ok {
				v.Val = k
			} else {
				c.dl.Errorf("E0209", g.Value.ESpan(), "constant initializer must be a compile-time constant")
			}
		} else if _, ok := c.info.Consts[g.Value]; !ok && !isZero(g.Value) {
			c.dl.Errorf("E0209", g.Value.ESpan(), "global initializer must be a constant or @zero")
		}
		c.declare(g.Name, v, g.Sp)
		c.info.Globals = append(c.info.Globals, v)
	}
	// 3. bodies
	for i, fd := range f.Fns {
		if fd.Extern {
			continue
		}
		fn := c.info.Funcs[i]
		b := &Body{Name: fd.Name, Func: fn, Ret: fn.Type.Ret, Err: fn.Type.Err, Block: fd.Body}
		c.body = b
		c.sc = &scope{parent: c.global, names: map[string]any{}}
		for j, p := range fd.Params {
			v := &Var{Name: p.Name, Type: fn.Type.Params[j], Mutable: true}
			b.Params = append(b.Params, v)
			b.Locals = append(b.Locals, v)
			c.declare(p.Name, v, p.Sp)
		}
		c.block(fd.Body, false)
		if b.Ret != TVoid && !terminates(fd.Body) {
			d := c.dl.Errorf("E0401", fd.NameSp, "function %q may reach the end of its body without returning a %s", fd.Name, b.Ret)
			end := fd.Body.Sp.End
			d.Fix("add a return at the end", diag.Span{File: end2(fd.Body.Sp).File, Start: diag.Pos{Line: end.Line, Col: end.Col - 1}, End: diag.Pos{Line: end.Line, Col: end.Col - 1}}, "return @zero; ")
		}
		if fd.Name == "main" && (!fd.Pub || len(fd.Params) != 0 || fn.Type.Ret != TI32 || fn.Type.Err != nil) {
			d := c.dl.Errorf("E0210", fd.NameSp, "main must be declared as 'pub fn main() -> i32'")
			d.Fix("use the canonical signature", diag.Span{File: fd.Sp.File, Start: fd.Sp.Start, End: fd.Body.Sp.Start}, "pub fn main() -> i32 ")
		}
		c.info.Bodies = append(c.info.Bodies, b)
	}
	c.checkInlineCycles()
	for i, td := range f.Tests {
		b := &Body{Name: "__ingot_test_" + strconv.Itoa(i), Test: td, Ret: TVoid, Block: td.Body}
		c.body = b
		c.sc = &scope{parent: c.global, names: map[string]any{}}
		c.block(td.Body, false)
		c.info.Tests = append(c.info.Tests, b)
	}
	return c.info
}

func end2(s diag.Span) diag.Span { return s }

func isZero(e syntax.Expr) bool {
	b, ok := e.(*syntax.BuiltinCall)
	return ok && b.Name == "zero"
}

func (c *checker) declare(name string, sym any, sp diag.Span) {
	s := c.sc
	if s == nil {
		s = c.global
	}
	if _, dup := s.names[name]; dup {
		c.dl.Errorf("E0200", sp, "%q is already declared in this scope", name)
		return
	}
	if c.sc != nil && c.sc != c.global {
		// Ingot forbids shadowing: every name refers to exactly one thing in a body.
		if prev := c.sc.parent.lookup(name); prev != nil {
			if _, isVar := prev.(*Var); isVar && !prev.(*Var).Global {
				c.dl.Errorf("E0211", sp, "%q shadows an outer variable; choose a different name", name)
				return
			}
		}
	}
	s.names[name] = sym
}

func (c *checker) layout(sd *syntax.StructDecl, visiting map[string]bool) {
	t := c.structs[sd.Name]
	if t == nil || t.Align != 0 {
		return
	}
	var off, align int64 = 0, 1
	seen := map[string]bool{}
	for _, f := range sd.Fields {
		if seen[f.Name] {
			c.dl.Errorf("E0200", f.Sp, "duplicate field %q", f.Name)
		}
		seen[f.Name] = true
		ft := c.resolve(f.Type)
		if ft.Kind == Struct && ft.Align == 0 {
			c.dl.Errorf("E0201", f.Sp, "struct %q contains itself (directly or via an earlier-incomplete struct); declare it before use or use a pointer", ft.Name)
			ft = TI64
		}
		if ft == TVoid {
			c.dl.Errorf("E0202", f.Sp, "field cannot have type void")
			ft = TI64
		}
		off = alignUp(off, ft.Align)
		t.Fields = append(t.Fields, Field{Name: f.Name, Type: ft, Offset: off})
		off += ft.Size
		align = max(align, ft.Align)
	}
	t.Align = align
	t.Size = alignUp(off, align)
}

func (c *checker) enumDecl(ed *syntax.EnumDecl) {
	if c.enums[ed.Name] != nil || primitives[ed.Name] != nil {
		c.dl.Errorf("E0200", ed.Sp, "duplicate type name %q", ed.Name)
		return
	}
	base := TI32
	if ed.Base != nil {
		base = c.resolve(ed.Base)
		if base.Kind != Int || base.Enum != nil {
			c.dl.Errorf("E0240", ed.Base.TSpan(), "enum base type must be an integer type, found %s", base)
			base = TI32
		}
	}
	info := &EnumInfo{}
	t := &Type{Kind: Int, Name: ed.Name, Size: base.Size, Align: base.Align, Signed: base.Signed, Enum: info}
	next := int64(0)
	c.sc = c.global
	for _, m := range ed.Members {
		if _, dup := info.Member(m.Name); dup {
			c.dl.Errorf("E0200", m.Sp, "duplicate enum member %q", m.Name)
			continue
		}
		if m.Value != nil {
			c.expr(m.Value, base)
			k, ok := c.info.Consts[m.Value]
			if !ok || k.IsFloat {
				c.dl.Errorf("E0209", m.Value.ESpan(), "enum value must be an integer constant")
			} else {
				next = k.I
			}
		}
		for i, v := range info.Values {
			if v == next {
				c.dl.Errorf("E0241", m.Sp, "enum members %s and %s have the same value %d", info.Names[i], m.Name, v)
			}
		}
		info.Names = append(info.Names, m.Name)
		info.Values = append(info.Values, next)
		next++
	}
	c.enums[ed.Name] = t
}

func alignUp(x, a int64) int64 { return (x + a - 1) / a * a }

func (c *checker) resolve(te syntax.TypeExpr) *Type {
	switch t := te.(type) {
	case *syntax.NamedType:
		if p := primitives[t.Name]; p != nil {
			return p
		}
		if s := c.structs[t.Name]; s != nil {
			return s
		}
		if en := c.enums[t.Name]; en != nil {
			return en
		}
		d := c.dl.Errorf("E0203", t.Sp, "unknown type %q", t.Name)
		if sug := suggest(t.Name); sug != "" {
			d.Fix("did you mean "+sug+"?", t.Sp, sug)
		}
		return TI64
	case *syntax.PtrType:
		return PtrTo(c.resolve(t.Elem))
	case *syntax.SliceType:
		return SliceOf(c.resolve(t.Elem))
	case *syntax.ArrayType:
		e := c.resolve(t.Elem)
		if e.Kind == Struct && e.Align == 0 {
			c.dl.Errorf("E0201", t.Sp, "array of incomplete struct %q", e.Name)
			return ArrayOf(t.Len, TI64)
		}
		return ArrayOf(t.Len, e)
	case *syntax.FnType:
		ft := &Type{Kind: Fn, Size: 8, Align: 8, Variadic: t.Variadic, Ret: TVoid}
		for _, p := range t.Params {
			ft.Params = append(ft.Params, c.resolve(p))
		}
		if t.Ret != nil {
			ft.Ret = c.resolve(t.Ret)
		}
		return ft
	}
	return TI64
}

func suggest(name string) string {
	m := map[string]string{"int": "i64", "long": "i64", "usize": "u64", "isize": "i64", "size_t": "u64", "char": "u8", "byte": "u8",
		"float": "f32", "double": "f64", "boolean": "bool", "uint": "u64", "int32": "i32", "int64": "i64", "float32": "f32", "float64": "f64", "String": "[]u8", "str": "[]u8"}
	return m[name]
}

// ---- statements ----

func (c *checker) block(b *syntax.Block, newScope bool) {
	if newScope {
		c.sc = &scope{parent: c.sc, names: map[string]any{}}
		defer func() { c.sc = c.sc.parent }()
	}
	for _, s := range b.Stmts {
		c.stmt(s)
	}
}

func (c *checker) stmt(s syntax.Stmt) {
	switch s := s.(type) {
	case *syntax.LetStmt:
		var want *Type
		if s.Type != nil {
			want = c.resolve(s.Type)
		}
		t := c.expr(s.Value, want)
		if want == nil {
			if t == TNull || (isZero(s.Value)) {
				c.dl.Errorf("E0204", s.NameSp, "cannot infer the type of %q; add a type annotation", s.Name)
				t = TI64
			}
			want = t
		} else {
			c.assignable(t, want, s.Value)
		}
		if want == TVoid {
			c.dl.Errorf("E0202", s.Value.ESpan(), "cannot bind a void value")
		}
		v := &Var{Name: s.Name, Type: want, Mutable: s.Mutable, Decl: s}
		c.info.Defs[s] = v
		c.body.Locals = append(c.body.Locals, v)
		c.declare(s.Name, v, s.NameSp)
	case *syntax.AssignStmt:
		tt := c.expr(s.Target, nil)
		c.checkLvalue(s.Target, true)
		vt := c.expr(s.Value, tt)
		if s.Op != "=" {
			op := s.Op[:len(s.Op)-1]
			if !c.binOpOK(op, tt) {
				c.dl.Errorf("E0302", s.Sp, "operator %q is not defined for %s", s.Op, tt)
			}
		}
		if s.Op == "<<=" || s.Op == ">>=" {
			if vt.Kind != Int {
				c.dl.Errorf("E0302", s.Value.ESpan(), "shift amount must be an integer")
			}
		} else {
			c.assignable(vt, tt, s.Value)
		}
	case *syntax.ExprStmt:
		switch s.X.(type) {
		case *syntax.CallExpr, *syntax.BuiltinCall, *syntax.TryExpr, *syntax.CatchExpr:
		default:
			c.dl.Errorf("E0303", s.Sp, "expression statement has no effect; only calls may be used as statements")
		}
		c.expr(s.X, nil)
	case *syntax.IfStmt:
		c.cond(s.Cond)
		c.block(s.Then, true)
		if s.Else != nil {
			c.stmt(s.Else)
		}
	case *syntax.WhileStmt:
		c.cond(s.Cond)
		c.loops++
		c.block(s.Body, true)
		c.loops--
	case *syntax.ForStmt:
		lt := c.expr(s.Lo, nil)
		var t *Type
		if isUntyped(s.Lo) {
			t = c.expr(s.Hi, nil)
			if isUntyped(s.Hi) {
				t = TI64
			}
			c.expr(s.Lo, t)
			c.expr(s.Hi, t)
		} else {
			t = lt
			c.assignable(c.expr(s.Hi, t), t, s.Hi)
		}
		if t.Kind != Int {
			c.dl.Errorf("E0304", s.Lo.ESpan(), "for-range bounds must be integers, found %s", t)
		}
		c.sc = &scope{parent: c.sc, names: map[string]any{}}
		v := &Var{Name: s.Var, Type: t}
		c.info.Defs[s] = v
		c.body.Locals = append(c.body.Locals, v)
		c.declare(s.Var, v, s.Sp)
		c.loops++
		c.block(s.Body, true)
		c.loops--
		c.sc = c.sc.parent
	case *syntax.ReturnStmt:
		if c.inDefer {
			c.dl.Errorf("E0308", s.Sp, "return is not allowed inside a defer body")
		}
		if b, ok := s.Value.(*syntax.BuiltinCall); ok && b.Name == "err" {
			c.info.Types[b] = TVoid
			if c.body.Err == nil {
				c.dl.Errorf("E0253", b.Sp, "@err in a function that cannot fail; declare the error type: -> %s ! E", c.body.Ret)
				return
			}
			if len(b.Args) != 1 {
				c.dl.Errorf("E0215", b.Sp, "@err takes one argument: return @err(%s.member);", c.body.Err)
				return
			}
			c.assignable(c.expr(b.Args[0], c.body.Err), c.body.Err, b.Args[0])
			return
		}
		if s.Value == nil {
			if c.body.Ret != TVoid {
				d := c.dl.Errorf("E0305", s.Sp, "missing return value of type %s", c.body.Ret)
				d.Fix("return a zero value", s.Sp, "return @zero;")
			}
			return
		}
		if c.body.Ret == TVoid {
			d := c.dl.Errorf("E0305", s.Value.ESpan(), "function returns void but a value was given")
			d.Fix("remove the value", s.Sp, "return;")
			c.expr(s.Value, nil)
			return
		}
		c.assignable(c.expr(s.Value, c.body.Ret), c.body.Ret, s.Value)
	case *syntax.SwitchStmt:
		c.switchStmt(s)
	case *syntax.DeferStmt:
		if c.inDefer {
			c.dl.Errorf("E0308", s.Sp, "defer inside a defer body is not allowed")
		}
		saved, savedLoops := c.inDefer, c.loopsAtDefer
		c.inDefer, c.loopsAtDefer = true, c.loops
		c.block(s.Body, true)
		c.inDefer, c.loopsAtDefer = saved, savedLoops
	case *syntax.BreakStmt, *syntax.ContinueStmt:
		if c.inDefer && c.loops == c.loopsAtDefer {
			c.dl.Errorf("E0308", s.SSpan(), "break/continue may not leave a defer body")
			return
		}
		if c.loops == 0 {
			c.dl.Errorf("E0306", s.SSpan(), "break/continue outside of a loop")
		}
	case *syntax.Block:
		c.block(s, true)
	}
}

func (c *checker) switchStmt(s *syntax.SwitchStmt) {
	xt := c.expr(s.X, nil)
	if xt.Kind != Int {
		c.dl.Errorf("E0500", s.X.ESpan(), "switch requires an integer or enum value, found %s", xt)
		return
	}
	seen := map[int64]bool{}
	for _, cs := range s.Cases {
		for _, v := range cs.Values {
			vt := c.expr(v, xt)
			c.assignable(vt, xt, v)
			k, ok := c.info.Consts[v]
			if !ok || k.IsFloat {
				c.dl.Errorf("E0501", v.ESpan(), "switch case must be a compile-time constant")
				continue
			}
			if seen[k.I] {
				c.dl.Errorf("E0502", v.ESpan(), "duplicate switch case %d", k.I)
			}
			seen[k.I] = true
		}
		c.block(cs.Body, true)
	}
	if s.Else != nil {
		c.block(s.Else, true)
		return
	}
	end := s.Sp.End
	at := diag.Span{File: s.Sp.File, Start: diag.Pos{Line: end.Line, Col: end.Col - 1}, End: diag.Pos{Line: end.Line, Col: end.Col - 1}}
	if xt.Enum == nil {
		d := c.dl.Errorf("E0503", s.X.ESpan(), "switch on %s must have an 'else' arm", xt)
		d.Fix("add an else arm", at, "else => { @unreachable(); } ")
		return
	}
	var missing []string
	for i, n := range xt.Enum.Names {
		if !seen[xt.Enum.Values[i]] {
			missing = append(missing, n)
		}
	}
	if len(missing) > 0 {
		d := c.dl.Errorf("E0504", s.X.ESpan(), "switch on enum %s is not exhaustive; missing %v", xt.Name, missing)
		text := ""
		for _, m := range missing {
			text += xt.Name + "." + m + " => { } "
		}
		d.Fix("handle the missing members", at, text)
		return
	}
	s.Exhaustive = true
}

func (c *checker) cond(e syntax.Expr) {
	t := c.expr(e, TBool)
	if t != TBool {
		d := c.dl.Errorf("E0307", e.ESpan(), "condition must be bool, found %s (Ingot has no truthiness)", t)
		if t.Kind == Int {
			d.Fix("compare against zero", diag.Span{File: e.ESpan().File, Start: e.ESpan().End, End: e.ESpan().End}, " != 0")
		} else if t.Kind == Ptr {
			d.Fix("compare against null", diag.Span{File: e.ESpan().File, Start: e.ESpan().End, End: e.ESpan().End}, " != null")
		}
	}
}

// fallibleCall returns the call that try/catch wraps, reporting E0250 if x is
// not a call to a fallible function.
func (c *checker) fallibleCall(x syntax.Expr, kw string) *syntax.CallExpr {
	for {
		p, ok := x.(*syntax.ParenExpr)
		if !ok {
			break
		}
		x = p.X
	}
	call, ok := x.(*syntax.CallExpr)
	if !ok {
		c.dl.Errorf("E0250", x.ESpan(), "%s needs a call to a fallible function (one declared -> T ! E)", kw)
		return nil
	}
	if c.handled == nil {
		c.handled = map[*syntax.CallExpr]bool{}
	}
	c.handled[call] = true
	return call
}

// leavesLoop reports whether a block ends in break or continue.
func leavesLoop(b *syntax.Block) bool {
	if len(b.Stmts) == 0 {
		return false
	}
	switch b.Stmts[len(b.Stmts)-1].(type) {
	case *syntax.BreakStmt, *syntax.ContinueStmt:
		return true
	}
	return false
}

func terminates(s syntax.Stmt) bool {
	switch s := s.(type) {
	case *syntax.ReturnStmt:
		return true
	case *syntax.Block:
		return len(s.Stmts) > 0 && terminates(s.Stmts[len(s.Stmts)-1])
	case *syntax.IfStmt:
		return s.Else != nil && terminates(s.Then) && terminates(s.Else)
	case *syntax.WhileStmt:
		b, ok := s.Cond.(*syntax.BoolLit)
		return ok && b.Val && !hasBreak(s.Body)
	case *syntax.SwitchStmt:
		if s.Else == nil && !s.Exhaustive {
			return false
		}
		for _, cs := range s.Cases {
			if !terminates(cs.Body) {
				return false
			}
		}
		return s.Else == nil || terminates(s.Else)
	case *syntax.ExprStmt:
		if b, ok := s.X.(*syntax.BuiltinCall); ok && (b.Name == "trap" || b.Name == "panic" || b.Name == "unreachable") {
			return true
		}
	}
	return false
}

func hasBreak(s syntax.Stmt) bool {
	switch s := s.(type) {
	case *syntax.BreakStmt:
		return true
	case *syntax.Block:
		for _, x := range s.Stmts {
			if hasBreak(x) {
				return true
			}
		}
	case *syntax.IfStmt:
		return hasBreak(s.Then) || (s.Else != nil && hasBreak(s.Else))
	case *syntax.SwitchStmt:
		for _, cs := range s.Cases {
			if hasBreak(cs.Body) {
				return true
			}
		}
		return s.Else != nil && hasBreak(s.Else)
	}
	return false // nested loops own their breaks
}

// checkLvalue verifies e denotes assignable storage.
func (c *checker) checkLvalue(e syntax.Expr, write bool) bool {
	switch e := e.(type) {
	case *syntax.IdentExpr:
		v, ok := c.info.Uses[e].(*Var)
		if !ok {
			c.dl.Errorf("E0301", e.Sp, "%q is not a variable", e.Name)
			return false
		}
		if v.Const || (write && !v.Mutable) || (!write && !v.Mutable && !v.Type.IsAggregate()) {
			d := c.dl.Errorf("E0301", e.Sp, "cannot %s immutable binding %q", map[bool]string{true: "assign to", false: "take the address of"}[write], e.Name)
			if ls, ok := v.Decl.(*syntax.LetStmt); ok {
				d.Fix("declare it with 'var'", diag.Span{File: ls.Sp.File, Start: ls.Sp.Start, End: diag.Pos{Line: ls.Sp.Start.Line, Col: ls.Sp.Start.Col + 3}}, "var")
			}
			return false
		}
		return true
	case *syntax.FieldExpr:
		xt := c.info.Types[e.X]
		if xt != nil && xt.Kind == Slice {
			c.dl.Errorf("E0301", e.Sp, "slice fields are read-only")
			return false
		}
		return c.checkLvalue(e.X, write)
	case *syntax.IndexExpr:
		xt := c.info.Types[e.X]
		if xt != nil && (xt.Kind == Slice || xt.Kind == Ptr) {
			return true
		}
		return c.checkLvalue(e.X, write)
	case *syntax.DerefExpr:
		return true
	case *syntax.ParenExpr:
		return c.checkLvalue(e.X, write)
	}
	c.dl.Errorf("E0301", e.ESpan(), "expression is not assignable")
	return false
}

// assignable reports whether a value of type from can be stored in to.
func (c *checker) assignable(from, to *Type, e syntax.Expr) {
	if from == nil || to == nil || Identical(from, to) {
		return
	}
	if from == TNull && (to.Kind == Ptr || to.Kind == Fn) {
		return
	}
	d := c.dl.Errorf("E0205", e.ESpan(), "type mismatch: expected %s, found %s (Ingot has no implicit conversions)", to, from)
	if from.IsNumeric() && to.IsNumeric() && from.Enum == nil && to.Enum == nil {
		sp := e.ESpan()
		d.Fixes = append(d.Fixes, diag.Fix{Message: "convert explicitly with 'as'", Edits: []diag.Edit{
			{Span: diag.Span{File: sp.File, Start: sp.Start, End: sp.Start}, Text: "("},
			{Span: diag.Span{File: sp.File, Start: sp.End, End: sp.End}, Text: ") as " + to.String()},
		}})
	}
	if from.Kind == Array && to.Kind == Slice && Identical(from.Elem, to.Elem) {
		sp := e.ESpan()
		d.Fixes = append(d.Fixes, diag.Fix{Message: "take a slice of the whole array", Edits: []diag.Edit{
			{Span: diag.Span{File: sp.File, Start: sp.End, End: sp.End}, Text: fmtSlice(from.Len)},
		}})
	}
}

func fmtSlice(n int64) string { return "[0.." + strconv.FormatInt(n, 10) + "]" }

// ---- expressions ----

func isUntyped(e syntax.Expr) bool {
	switch e := e.(type) {
	case *syntax.IntLit, *syntax.FloatLit:
		return true
	case *syntax.UnaryExpr:
		return e.Op == "-" && isUntyped(e.X)
	case *syntax.ParenExpr:
		return isUntyped(e.X)
	case *syntax.BinaryExpr:
		return opIsArith(e.Op) && isUntyped(e.X) && isUntyped(e.Y)
	}
	return false
}

func opIsArith(op string) bool {
	switch op {
	case "+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>":
		return true
	}
	return false
}

func (c *checker) expr(e syntax.Expr, want *Type) *Type {
	t := c.expr0(e, want)
	if t == nil {
		t = TI64
	}
	c.info.Types[e] = t
	return t
}

func (c *checker) binOpOK(op string, t *Type) bool {
	if t.Enum != nil {
		return false // enums support only == and != (cast with 'as' for arithmetic)
	}
	switch op {
	case "+", "-", "*":
		return t.Kind == Int || t.Kind == Float || t.Kind == Vec
	case "/":
		return t.Kind == Int || t.Kind == Float || (t.Kind == Vec && t.Elem.Kind == Float)
	case "%":
		return t.Kind == Int
	case "&", "|", "^":
		return t.Kind == Int || t == TBool || (t.Kind == Vec && t.Elem.Kind == Int)
	case "<<", ">>":
		return t.Kind == Int
	}
	return false
}

func (c *checker) expr0(e syntax.Expr, want *Type) *Type {
	switch e := e.(type) {
	case *syntax.IntLit:
		u, err := syntax.ParseIntLit(e.Text)
		if want != nil && want.Enum != nil {
			want = nil // integers never silently become enum values; use 'as'
		}
		if want == nil {
			want = TI64
			if err == nil {
				// literal with no context: Ingot still requires a context, but i64 is
				// the documented default inside untyped arithmetic.
			}
		}
		if err != nil {
			c.dl.Errorf("E0206", e.Sp, "integer literal %s does not fit in 64 bits", e.Text)
			return want
		}
		switch want.Kind {
		case Int:
			if !fitsInt(u, want) {
				c.dl.Errorf("E0206", e.Sp, "integer literal %s overflows %s", e.Text, want)
			}
			c.info.Consts[e] = Const{I: int64(u)}
			return want
		case Float:
			c.info.Consts[e] = Const{IsFloat: true, F: float64(u)}
			return want
		}
		c.info.Consts[e] = Const{I: int64(u)}
		return TI64
	case *syntax.FloatLit:
		f, err := strconv.ParseFloat(stripUnderscore(e.Text), 64)
		if err != nil {
			c.dl.Errorf("E0206", e.Sp, "invalid float literal %s", e.Text)
		}
		c.info.Consts[e] = Const{IsFloat: true, F: f}
		if want != nil && want.Kind == Float {
			return want
		}
		if want != nil && want.Kind == Int {
			d := c.dl.Errorf("E0205", e.Sp, "float literal used where %s is expected", want)
			_ = d
		}
		return TF64
	case *syntax.BoolLit:
		c.info.Consts[e] = Const{I: map[bool]int64{true: 1}[e.Val]}
		return TBool
	case *syntax.NullLit:
		if want != nil && (want.Kind == Ptr || want.Kind == Fn) {
			return want
		}
		return TNull
	case *syntax.StringLit:
		if e.C {
			return PtrTo(TU8)
		}
		return SliceOf(TU8)
	case *syntax.ParenExpr:
		t := c.expr(e.X, want)
		if k, ok := c.info.Consts[e.X]; ok {
			c.info.Consts[e] = k
		}
		return t
	case *syntax.IdentExpr:
		sym := c.sc.lookup(e.Name)
		if sym == nil {
			c.dl.Errorf("E0212", e.Sp, "undefined name %q", e.Name)
			return nil
		}
		c.info.Uses[e] = sym
		switch s := sym.(type) {
		case *Var:
			if s.Const {
				c.info.Consts[e] = s.Val
			}
			return s.Type
		case *Func:
			return s.Type
		}
	case *syntax.UnaryExpr:
		switch e.Op {
		case "-":
			t := c.expr(e.X, want)
			if !(t.Kind == Int && t.Signed) && t.Kind != Float && t.Kind != Vec {
				c.dl.Errorf("E0302", e.Sp, "unary '-' requires a signed integer, float or vector, found %s", t)
			}
			if k, ok := c.info.Consts[e.X]; ok {
				k.I, k.F = -k.I, -k.F
				c.info.Consts[e] = k
			}
			return t
		case "!":
			t := c.expr(e.X, TBool)
			if t != TBool {
				c.dl.Errorf("E0302", e.Sp, "'!' requires bool, found %s; use '~' for bitwise not", t)
			}
			return TBool
		case "~":
			t := c.expr(e.X, want)
			if t.Kind != Int {
				c.dl.Errorf("E0302", e.Sp, "'~' requires an integer, found %s", t)
			}
			if k, ok := c.info.Consts[e.X]; ok {
				k.I = ^k.I
				c.info.Consts[e] = k
			}
			return t
		case "&":
			var inner *Type
			if want != nil && want.Kind == Ptr {
				inner = want.Elem
			}
			t := c.expr(e.X, inner)
			if id, ok := e.X.(*syntax.IdentExpr); ok {
				if _, isFn := c.info.Uses[id].(*Func); isFn {
					c.dl.Errorf("E0302", e.Sp, "functions are already values; write %s instead of &%s", id.Name, id.Name)
					return t
				}
			}
			c.checkLvalue(e.X, false)
			return PtrTo(t)
		}
	case *syntax.BinaryExpr:
		return c.binary(e, want)
	case *syntax.CastExpr:
		to := c.resolve(e.To)
		var from *Type
		if isUntyped(e.X) && to.IsNumeric() {
			from = c.expr(e.X, to)
		} else {
			from = c.expr(e.X, nil)
		}
		ok := (from.IsNumeric() && to.IsNumeric()) ||
			(from == TBool && to.Kind == Int) ||
			((from.Kind == Ptr || from.Kind == Fn) && (to.Kind == Ptr || to.Kind == Fn || to == TI64 || to == TU64)) ||
			((from == TI64 || from == TU64) && to.Kind == Ptr) ||
			(from.Kind == Vec && to.Kind == Vec && from.Size == to.Size) || Identical(from, to)
		if !ok {
			c.dl.Errorf("E0213", e.Sp, "cannot cast %s to %s", from, to)
		}
		if k, ok := c.info.Consts[e.X]; ok && to.IsNumeric() {
			if to.Kind == Float && !k.IsFloat {
				k = Const{IsFloat: true, F: float64(k.I)}
			} else if to.Kind == Int && k.IsFloat {
				k = Const{I: int64(k.F)}
			}
			c.info.Consts[e] = k
		}
		return to
	case *syntax.CallExpr:
		ft := c.expr(e.Fn, nil)
		if ft.Kind != Fn {
			c.dl.Errorf("E0214", e.Fn.ESpan(), "cannot call a value of type %s", ft)
			for _, a := range e.Args {
				c.expr(a, nil)
			}
			return nil
		}
		if len(e.Args) < len(ft.Params) || (!ft.Variadic && len(e.Args) > len(ft.Params)) {
			c.dl.Errorf("E0215", e.Sp, "wrong number of arguments: expected %d, found %d", len(ft.Params), len(e.Args))
		}
		for i, a := range e.Args {
			if i < len(ft.Params) {
				c.assignable(c.expr(a, ft.Params[i]), ft.Params[i], a)
				continue
			}
			at := c.expr(a, nil)
			if at.IsAggregate() || at.Kind == Vec || at == TNull {
				c.dl.Errorf("E0216", a.ESpan(), "variadic arguments must be scalars or pointers, found %s", at)
			}
			if at.Kind == Float && at != TF64 {
				d := c.dl.Errorf("E0216", a.ESpan(), "C variadic float arguments must be f64 (no implicit promotion)")
				d.Fix("convert to f64", diag.Span{File: a.ESpan().File, Start: a.ESpan().End, End: a.ESpan().End}, " as f64")
			}
		}
		if id, ok := e.Fn.(*syntax.IdentExpr); ok && c.body != nil && c.body.Func != nil && c.body.Func.Decl.Inline {
			if callee, ok := c.info.Uses[id].(*Func); ok && callee.Decl != nil && callee.Decl.Inline {
				if c.inlineCalls == nil {
					c.inlineCalls = map[*Func][]*Func{}
				}
				c.inlineCalls[c.body.Func] = append(c.inlineCalls[c.body.Func], callee)
			}
		}
		if ft.Err != nil && !c.handled[e] {
			d := c.dl.Errorf("E0250", e.Sp, "this call can fail with %s; handle the error with try or catch", ft.Err)
			if c.body != nil && c.body.Err == ft.Err && !c.inDefer {
				d.Fix("propagate the error with try", diag.Span{File: e.Sp.File, Start: e.Sp.Start, End: e.Sp.Start}, "try ")
			}
		}
		return ft.Ret
	case *syntax.TryExpr:
		call := c.fallibleCall(e.X, "try")
		t := c.expr(e.X, want)
		if call == nil {
			return t
		}
		ft := c.info.Types[call.Fn]
		switch {
		case ft == nil || ft.Kind != Fn:
		case ft.Err == nil:
			c.dl.Errorf("E0250", e.Sp, "try on a call that cannot fail; remove try")
		case c.inDefer:
			c.dl.Errorf("E0308", e.Sp, "try is not allowed inside a defer body (it may return)")
		case c.body.Err == nil:
			d := c.dl.Errorf("E0251", e.Sp, "try needs the enclosing function to be fallible with %s; declare -> %s ! %s or use catch", ft.Err, c.body.Ret, ft.Err)
			d.Notes = append(d.Notes, "tests and main cannot fail; use catch there")
		case c.body.Err != ft.Err:
			c.dl.Errorf("E0251", e.Sp, "try propagates %s, but the enclosing function fails with %s; use catch and return @err(...) to convert", ft.Err, c.body.Err)
		}
		return t
	case *syntax.CatchExpr:
		call := c.fallibleCall(e.X, "catch")
		t := c.expr(e.X, want)
		if call == nil {
			return t
		}
		ft := c.info.Types[call.Fn]
		if ft == nil || ft.Kind != Fn || ft.Err == nil {
			c.dl.Errorf("E0250", e.Sp, "catch on a call that cannot fail; remove the catch")
			return t
		}
		c.sc = &scope{parent: c.sc, names: map[string]any{}}
		if e.Name != "_" {
			v := &Var{Name: e.Name, Type: ft.Err}
			c.info.Defs[e] = v
			c.body.Locals = append(c.body.Locals, v)
			c.declare(e.Name, v, e.NameSp)
		}
		if e.Handler != nil {
			c.block(e.Handler, false)
			if t != TVoid && !terminates(e.Handler) && !leavesLoop(e.Handler) {
				d := c.dl.Errorf("E0252", e.Handler.Sp, "this catch block must leave (return, break, continue or @panic), because the call's %s result would be undefined after it", t)
				d.Notes = append(d.Notes, "to continue with a value, write: catch "+e.Name+" => fallback_value")
			}
		} else if t == TVoid {
			c.dl.Errorf("E0252", e.Fallback.ESpan(), "the call returns void, so there is no value to fall back to; use a block: catch %s => { ... }", e.Name)
			c.expr(e.Fallback, nil)
		} else {
			c.assignable(c.expr(e.Fallback, t), t, e.Fallback)
		}
		c.sc = c.sc.parent
		return t
	case *syntax.FieldExpr:
		if id, ok := e.X.(*syntax.IdentExpr); ok && c.sc.lookup(id.Name) == nil && c.enums[id.Name] != nil {
			et := c.enums[id.Name]
			v, ok := et.Enum.Member(e.Name)
			if !ok {
				d := c.dl.Errorf("E0217", e.Sp, "enum %s has no member %q", et.Name, e.Name)
				d.Note("members: %v", et.Enum.Names)
				return et
			}
			c.info.Consts[e] = Const{I: v}
			return et
		}
		xt := c.expr(e.X, nil)
		switch xt.Kind {
		case Struct:
			for _, f := range xt.Fields {
				if f.Name == e.Name {
					return f.Type
				}
			}
			c.dl.Errorf("E0217", e.Sp, "struct %s has no field %q", xt, e.Name)
			return nil
		case Slice:
			switch e.Name {
			case "len":
				return TI64
			case "ptr":
				return PtrTo(xt.Elem)
			}
		case Array:
			if e.Name == "len" {
				c.info.Consts[e] = Const{I: xt.Len}
				return TI64
			}
		case Ptr:
			if xt.Elem.Kind == Struct {
				d := c.dl.Errorf("E0217", e.Sp, "field access through a pointer must be explicit")
				sp := e.X.ESpan()
				d.Fix("dereference with '.*'", diag.Span{File: sp.File, Start: sp.End, End: sp.End}, ".*")
				return nil
			}
		}
		c.dl.Errorf("E0217", e.Sp, "type %s has no field %q", xt, e.Name)
		return nil
	case *syntax.DerefExpr:
		xt := c.expr(e.X, nil)
		if xt.Kind != Ptr {
			c.dl.Errorf("E0218", e.Sp, "cannot dereference non-pointer type %s", xt)
			return nil
		}
		return xt.Elem
	case *syntax.IndexExpr:
		xt := c.expr(e.X, nil)
		it := c.expr(e.Index, TI64)
		if it != TI64 {
			d := c.dl.Errorf("E0219", e.Index.ESpan(), "index must be i64, found %s", it)
			if it.Kind == Int {
				sp := e.Index.ESpan()
				d.Fix("convert to i64", diag.Span{File: sp.File, Start: sp.End, End: sp.End}, " as i64")
			}
		}
		switch xt.Kind {
		case Array, Slice, Ptr:
			if k, ok := c.info.Consts[e.Index]; ok && xt.Kind == Array && (k.I < 0 || k.I >= xt.Len) {
				c.dl.Errorf("E0220", e.Index.ESpan(), "index %d is out of bounds for %s", k.I, xt)
			}
			return xt.Elem
		case Vec:
			if _, ok := c.info.Consts[e.Index]; !ok {
				c.dl.Errorf("E0219", e.Index.ESpan(), "vector lane index must be a constant")
			}
			return xt.Elem
		}
		c.dl.Errorf("E0219", e.Sp, "cannot index type %s", xt)
		return nil
	case *syntax.SliceExpr:
		xt := c.expr(e.X, nil)
		for _, b := range []syntax.Expr{e.Lo, e.Hi} {
			if t := c.expr(b, TI64); t != TI64 {
				c.dl.Errorf("E0219", b.ESpan(), "slice bound must be i64, found %s", t)
			}
		}
		switch xt.Kind {
		case Array, Slice, Ptr:
			if xt.Kind == Array {
				c.checkLvalue(e.X, false)
			}
			return SliceOf(xt.Elem)
		}
		c.dl.Errorf("E0219", e.Sp, "cannot slice type %s", xt)
		return nil
	case *syntax.CompositeLit:
		t := c.resolve(e.Type)
		switch t.Kind {
		case Struct:
			if len(e.Names) == 0 && len(e.Values) > 0 {
				c.dl.Errorf("E0221", e.Sp, "struct literals must name their fields")
				return t
			}
			got := map[string]bool{}
			for i, n := range e.Names {
				var ft *Type
				for _, f := range t.Fields {
					if f.Name == n {
						ft = f.Type
					}
				}
				if ft == nil {
					c.dl.Errorf("E0217", e.Values[i].ESpan(), "struct %s has no field %q", t, n)
					c.expr(e.Values[i], nil)
					continue
				}
				got[n] = true
				c.assignable(c.expr(e.Values[i], ft), ft, e.Values[i])
			}
			var missing []string
			for _, f := range t.Fields {
				if !got[f.Name] {
					missing = append(missing, f.Name)
				}
			}
			if len(missing) > 0 {
				d := c.dl.Errorf("E0222", e.Sp, "missing fields in %s literal: %v (every field must be initialized explicitly)", t, missing)
				d.Note("use @zero to zero-initialize a whole struct")
			}
		case Array:
			if int64(len(e.Values)) != t.Len {
				c.dl.Errorf("E0222", e.Sp, "array literal has %d elements, type %s needs %d", len(e.Values), t, t.Len)
			}
			for _, v := range e.Values {
				c.assignable(c.expr(v, t.Elem), t.Elem, v)
			}
		default:
			c.dl.Errorf("E0221", e.Sp, "composite literal of non-aggregate type %s", t)
		}
		return t
	case *syntax.BuiltinCall:
		return c.builtin(e, want)
	}
	return nil
}

func stripUnderscore(s string) string {
	out := make([]byte, 0, len(s))
	for i := 0; i < len(s); i++ {
		if s[i] != '_' {
			out = append(out, s[i])
		}
	}
	return string(out)
}

func fitsInt(u uint64, t *Type) bool {
	bits := uint(t.Size * 8)
	if t.Signed {
		return u <= uint64(1)<<(bits-1) // allows -2^(bits-1) via unary minus; checked loosely
	}
	if bits == 64 {
		return true
	}
	return u < uint64(1)<<bits
}

func (c *checker) binary(e *syntax.BinaryExpr, want *Type) *Type {
	switch e.Op {
	case "&&", "||":
		for _, s := range []syntax.Expr{e.X, e.Y} {
			if t := c.expr(s, TBool); t != TBool {
				c.dl.Errorf("E0307", s.ESpan(), "operand of %q must be bool, found %s", e.Op, t)
			}
		}
		return TBool
	}
	cmp := false
	switch e.Op {
	case "==", "!=", "<", "<=", ">", ">=":
		cmp = true
	}
	var xt, yt *Type
	shift := e.Op == "<<" || e.Op == ">>"
	switch {
	case shift:
		xt = c.expr(e.X, want)
		yt = c.expr(e.Y, xt)
		if yt.Kind != Int {
			c.dl.Errorf("E0302", e.Y.ESpan(), "shift amount must be an integer, found %s", yt)
		}
	case isUntyped(e.X) && !isUntyped(e.Y):
		yt = c.expr(e.Y, nil)
		xt = c.expr(e.X, yt)
	case isUntyped(e.Y) && !isUntyped(e.X):
		xt = c.expr(e.X, nil)
		yt = c.expr(e.Y, xt)
	default:
		w := want
		if cmp {
			w = nil
		}
		xt = c.expr(e.X, w)
		yt = c.expr(e.Y, xt)
	}
	if !cmp && (xt.Enum != nil || yt.Enum != nil) {
		d := c.dl.Errorf("E0302", e.Sp, "operator %q is not defined for enum values", e.Op)
		d.Note("convert explicitly, e.g. ((v as i64) + 1) as %s", map[bool]*Type{true: xt, false: yt}[xt.Enum != nil])
		return map[bool]*Type{true: xt, false: yt}[xt.Enum != nil]
	}
	if !shift && !Identical(xt, yt) && !(xt == TNull && yt.Kind == Ptr) && !(yt == TNull && xt.Kind == Ptr) {
		d := c.dl.Errorf("E0205", e.Sp, "mismatched operand types %s and %s for %q (Ingot has no implicit conversions)", xt, yt, e.Op)
		if xt.IsNumeric() && yt.IsNumeric() && xt.Enum == nil && yt.Enum == nil {
			sp := e.Y.ESpan()
			d.Fixes = append(d.Fixes, diag.Fix{Message: "convert the right operand", Edits: []diag.Edit{
				{Span: diag.Span{File: sp.File, Start: sp.Start, End: sp.Start}, Text: "("},
				{Span: diag.Span{File: sp.File, Start: sp.End, End: sp.End}, Text: ") as " + xt.String()},
			}})
		}
		return map[bool]*Type{true: TBool, false: xt}[cmp]
	}
	if cmp {
		ok := xt.IsNumeric() || xt == TBool || xt.Kind == Ptr || xt == TNull || xt.Kind == Fn
		if e.Op != "==" && e.Op != "!=" {
			ok = (xt.IsNumeric() && xt.Enum == nil) || xt.Kind == Ptr
		}
		if !ok {
			c.dl.Errorf("E0302", e.Sp, "operator %q is not defined for %s", e.Op, xt)
		}
		c.fold(e)
		return TBool
	}
	if !c.binOpOK(e.Op, xt) {
		c.dl.Errorf("E0302", e.Sp, "operator %q is not defined for %s", e.Op, xt)
	}
	c.fold(e)
	return xt
}

func (c *checker) fold(e *syntax.BinaryExpr) {
	a, ok1 := c.info.Consts[e.X]
	b, ok2 := c.info.Consts[e.Y]
	if !ok1 || !ok2 {
		return
	}
	var r Const
	if a.IsFloat || b.IsFloat {
		x, y := a.F, b.F
		r.IsFloat = true
		switch e.Op {
		case "+":
			r.F = x + y
		case "-":
			r.F = x - y
		case "*":
			r.F = x * y
		case "/":
			r.F = x / y
		default:
			return
		}
		if math.IsNaN(r.F) {
			return
		}
		c.info.Consts[e] = r
		return
	}
	x, y := a.I, b.I
	if t := c.info.Types[e.X]; t != nil && t.Kind == Int && !t.Signed {
		ux, uy := uint64(x), uint64(y)
		switch e.Op {
		case "/", "%":
			if uy == 0 {
				c.dl.Errorf("E0223", e.Sp, "division by constant zero")
				return
			}
			if e.Op == "/" {
				c.info.Consts[e] = Const{I: int64(ux / uy)}
			} else {
				c.info.Consts[e] = Const{I: int64(ux % uy)}
			}
			return
		case ">>":
			c.info.Consts[e] = Const{I: int64(ux >> (uy & 63))}
			return
		}
	}
	switch e.Op {
	case "+":
		r.I = x + y
	case "-":
		r.I = x - y
	case "*":
		r.I = x * y
	case "/", "%":
		if y == 0 {
			c.dl.Errorf("E0223", e.Sp, "division by constant zero")
			return
		}
		if e.Op == "/" {
			r.I = x / y
		} else {
			r.I = x % y
		}
	case "&":
		r.I = x & y
	case "|":
		r.I = x | y
	case "^":
		r.I = x ^ y
	case "<<":
		r.I = x << uint(y&63)
	case ">>":
		r.I = x >> uint(y&63)
	default:
		return // comparisons are not folded (codegen handles them)
	}
	if t := c.info.Types[e.X]; t != nil && t.Kind == Int && t.Size < 8 {
		return // let codegen apply wrap-around for narrow types
	}
	c.info.Consts[e] = r
}

// checkInlineCycles rejects inline fns that reach themselves through direct
// calls to inline fns, since they could not be expanded.
func (c *checker) checkInlineCycles() {
	for _, f := range c.info.Funcs {
		if f.Decl == nil || !f.Decl.Inline {
			continue
		}
		seen := map[*Func]bool{}
		stack := append([]*Func(nil), c.inlineCalls[f]...)
		for len(stack) > 0 {
			g := stack[len(stack)-1]
			stack = stack[:len(stack)-1]
			if g == f {
				c.dl.Errorf("E0245", f.Decl.NameSp, "inline fn %q is recursive (directly or through other inline fns); remove 'inline'", f.Name)
				break
			}
			if !seen[g] {
				seen[g] = true
				stack = append(stack, c.inlineCalls[g]...)
			}
		}
	}
}
