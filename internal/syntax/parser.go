package syntax

import (
	"strconv"

	"github.com/hossein1376/ingot/internal/diag"
)

// builtins whose first argument is a type
var typeArgBuiltins = map[string]bool{"sizeof": true, "splat": true, "load": true}

// Parser is a recursive-descent parser. It stops at the first syntax error in
// a declaration and resynchronizes at the next top-level keyword.
type Parser struct {
	file string
	toks []Tok
	p    int
	dl   *diag.List
}

type bailout struct{}

// Parse parses a whole file.
func Parse(file string, src []byte, dl *diag.List) *File {
	ps := &Parser{file: file, toks: Lex(file, src, dl), dl: dl}
	f := &File{Name: file}
	if dl.HasErrors() {
		return f
	}
	for ps.cur().Kind != EOF {
		ps.decl(f)
	}
	return f
}

func (ps *Parser) cur() Tok  { return ps.toks[ps.p] }
func (ps *Parser) peek() Tok { return ps.toks[min(ps.p+1, len(ps.toks)-1)] }
func (ps *Parser) next() Tok {
	t := ps.toks[ps.p]
	if ps.p < len(ps.toks)-1 {
		ps.p++
	}
	return t
}
func (ps *Parser) is(text string) bool {
	t := ps.cur()
	return (t.Kind == Punct || t.Kind == Keyword) && t.Text == text
}
func (ps *Parser) accept(text string) bool {
	if ps.is(text) {
		ps.next()
		return true
	}
	return false
}
func (ps *Parser) sp(t Tok) diag.Span { return diag.Span{File: ps.file, Start: t.Pos, End: t.End} }
func (ps *Parser) from(start diag.Pos) diag.Span {
	prev := ps.toks[max(ps.p-1, 0)]
	return diag.Span{File: ps.file, Start: start, End: prev.End}
}

func (ps *Parser) fail(code string, sp diag.Span, format string, args ...any) *diag.Diagnostic {
	d := ps.dl.Errorf(code, sp, format, args...)
	return d
}

func (ps *Parser) expect(text string) Tok {
	if ps.is(text) {
		return ps.next()
	}
	t := ps.cur()
	d := ps.fail("E0100", ps.sp(t), "expected %q, found %s", text, t)
	if text == ";" {
		prev := ps.toks[max(ps.p-1, 0)]
		d.Fix("insert ';'", diag.Span{File: ps.file, Start: prev.End, End: prev.End}, ";")
	}
	panic(bailout{})
}

func (ps *Parser) ident() Tok {
	t := ps.cur()
	if t.Kind != Ident {
		ps.fail("E0101", ps.sp(t), "expected identifier, found %s", t)
		panic(bailout{})
	}
	return ps.next()
}

func (ps *Parser) decl(f *File) {
	defer func() {
		if r := recover(); r != nil {
			if _, ok := r.(bailout); !ok {
				panic(r)
			}
			// resync at next top-level keyword
			for ps.cur().Kind != EOF {
				t := ps.cur()
				if t.Kind == Keyword && (t.Text == "fn" || t.Text == "pub" || t.Text == "extern" || t.Text == "struct" || t.Text == "test" || t.Text == "const" || t.Text == "var" || t.Text == "import" || t.Text == "enum") && t.Pos.Col == 1 {
					break
				}
				ps.next()
			}
		}
	}()
	t := ps.cur()
	switch {
	case ps.is("pub") || ps.is("fn") || ps.is("extern"):
		f.Fns = append(f.Fns, ps.fnDecl())
	case ps.is("struct"):
		f.Structs = append(f.Structs, ps.structDecl())
	case ps.is("enum"):
		f.Enums = append(f.Enums, ps.enumDecl())
	case ps.is("const") || ps.is("var"):
		f.Globals = append(f.Globals, ps.globalDecl())
	case ps.is("import"):
		ps.next()
		pt := ps.cur()
		if pt.Kind != String {
			ps.fail("E0102", ps.sp(pt), "expected a path string after 'import'")
			panic(bailout{})
		}
		ps.next()
		ps.expect(";")
		f.Imports = append(f.Imports, &ImportDecl{Path: pt.Text, Sp: ps.from(t.Pos)})
	case ps.is("test"):
		ps.next()
		name := ps.cur()
		if name.Kind != String {
			ps.fail("E0102", ps.sp(name), "expected test name string after 'test'")
			panic(bailout{})
		}
		ps.next()
		body := ps.block()
		f.Tests = append(f.Tests, &TestDecl{Name: name.Text, Body: body, Sp: ps.from(t.Pos)})
	default:
		ps.fail("E0102", ps.sp(t), "expected a declaration (fn, pub fn, extern fn, struct, const, var, test), found %s", t)
		ps.next()
		panic(bailout{})
	}
}

func (ps *Parser) fnDecl() *FnDecl {
	start := ps.cur().Pos
	fd := &FnDecl{}
	if ps.accept("pub") {
		fd.Pub = true
	}
	if ps.accept("extern") {
		fd.Extern = true
	}
	ps.expect("fn")
	nt := ps.ident()
	fd.Name, fd.NameSp = nt.Text, ps.sp(nt)
	ps.expect("(")
	for !ps.is(")") {
		if ps.accept("...") {
			fd.Variadic = true
			break
		}
		pt := ps.ident()
		ps.expect(":")
		ty := ps.typeExpr()
		fd.Params = append(fd.Params, Param{Name: pt.Text, Type: ty, Sp: ps.from(pt.Pos)})
		if !ps.accept(",") {
			break
		}
	}
	ps.expect(")")
	if ps.accept("->") {
		fd.Ret = ps.typeExpr()
	}
	if fd.Extern {
		ps.expect(";")
	} else {
		fd.Body = ps.block()
	}
	fd.Sp = ps.from(start)
	return fd
}

func (ps *Parser) structDecl() *StructDecl {
	start := ps.next().Pos
	sd := &StructDecl{Name: ps.ident().Text}
	ps.expect("{")
	for !ps.is("}") {
		ft := ps.ident()
		ps.expect(":")
		sd.Fields = append(sd.Fields, Field{Name: ft.Text, Type: ps.typeExpr(), Sp: ps.from(ft.Pos)})
		if !ps.accept(",") {
			break
		}
	}
	ps.expect("}")
	sd.Sp = ps.from(start)
	return sd
}

func (ps *Parser) enumDecl() *EnumDecl {
	start := ps.next().Pos
	ed := &EnumDecl{Name: ps.ident().Text}
	if ps.accept(":") {
		ed.Base = ps.typeExpr()
	}
	ps.expect("{")
	for !ps.is("}") {
		mt := ps.ident()
		m := EnumMember{Name: mt.Text}
		if ps.accept("=") {
			m.Value = ps.expr()
		}
		m.Sp = ps.from(mt.Pos)
		ed.Members = append(ed.Members, m)
		if !ps.accept(",") {
			break
		}
	}
	ps.expect("}")
	ed.Sp = ps.from(start)
	return ed
}

func (ps *Parser) switchStmt() *SwitchStmt {
	start := ps.next().Pos
	s := &SwitchStmt{X: ps.expr()}
	ps.expect("{")
	for !ps.is("}") && ps.cur().Kind != EOF {
		ct := ps.cur()
		if ps.accept("else") {
			ps.expect("=>")
			if s.Else != nil {
				ps.fail("E0110", ps.sp(ct), "switch has more than one 'else' arm")
			}
			s.Else = ps.block()
			continue
		}
		c := &SwitchCase{}
		for {
			c.Values = append(c.Values, ps.expr())
			if !ps.accept(",") {
				break
			}
		}
		ps.expect("=>")
		c.Body = ps.block()
		c.Sp = ps.from(ct.Pos)
		s.Cases = append(s.Cases, c)
	}
	ps.expect("}")
	s.Sp = ps.from(start)
	return s
}

func (ps *Parser) globalDecl() *GlobalDecl {
	kw := ps.next()
	g := &GlobalDecl{Const: kw.Text == "const"}
	g.Name = ps.ident().Text
	ps.expect(":")
	g.Type = ps.typeExpr()
	ps.expect("=")
	g.Value = ps.expr()
	ps.expect(";")
	g.Sp = ps.from(kw.Pos)
	return g
}

func (ps *Parser) typeExpr() TypeExpr {
	t := ps.cur()
	switch {
	case ps.accept("*"):
		return &PtrType{Elem: ps.typeExpr(), Sp: ps.from(t.Pos)}
	case ps.accept("["):
		if ps.accept("]") {
			return &SliceType{Elem: ps.typeExpr(), Sp: ps.from(t.Pos)}
		}
		lt := ps.cur()
		if lt.Kind != Int {
			ps.fail("E0103", ps.sp(lt), "array length must be an integer literal")
			panic(bailout{})
		}
		ps.next()
		n, err := parseInt(lt.Text)
		if err != nil || n < 0 {
			ps.fail("E0103", ps.sp(lt), "invalid array length %s", lt.Text)
			panic(bailout{})
		}
		ps.expect("]")
		return &ArrayType{Len: n, Elem: ps.typeExpr(), Sp: ps.from(t.Pos)}
	case ps.accept("fn"):
		ft := &FnType{}
		ps.expect("(")
		for !ps.is(")") {
			if ps.accept("...") {
				ft.Variadic = true
				break
			}
			ft.Params = append(ft.Params, ps.typeExpr())
			if !ps.accept(",") {
				break
			}
		}
		ps.expect(")")
		if ps.accept("->") {
			ft.Ret = ps.typeExpr()
		}
		ft.Sp = ps.from(t.Pos)
		return ft
	case t.Kind == Ident:
		ps.next()
		return &NamedType{Name: t.Text, Sp: ps.sp(t)}
	}
	ps.fail("E0104", ps.sp(t), "expected a type, found %s", t)
	panic(bailout{})
}

func (ps *Parser) block() *Block {
	start := ps.expect("{").Pos
	b := &Block{}
	for !ps.is("}") && ps.cur().Kind != EOF {
		b.Stmts = append(b.Stmts, ps.stmt())
	}
	ps.expect("}")
	b.Sp = ps.from(start)
	return b
}

var assignOps = map[string]bool{"=": true, "+=": true, "-=": true, "*=": true, "/=": true, "%=": true, "&=": true, "|=": true, "^=": true, "<<=": true, ">>=": true}

func (ps *Parser) stmt() Stmt {
	t := ps.cur()
	switch {
	case ps.is("let") || ps.is("var"):
		ps.next()
		nt := ps.ident()
		s := &LetStmt{Mutable: t.Text == "var", Name: nt.Text, NameSp: ps.sp(nt)}
		if ps.accept(":") {
			s.Type = ps.typeExpr()
		}
		if !ps.is("=") {
			d := ps.fail("E0105", ps.sp(ps.cur()), "variable %q must be initialized; use '= @zero' for zero-initialization", nt.Text)
			d.Fix("zero-initialize", diag.Span{File: ps.file, Start: ps.cur().Pos, End: ps.cur().Pos}, " = @zero")
			panic(bailout{})
		}
		ps.next()
		s.Value = ps.expr()
		ps.expect(";")
		s.Sp = ps.from(t.Pos)
		return s
	case ps.is("if"):
		return ps.ifStmt()
	case ps.is("switch"):
		return ps.switchStmt()
	case ps.is("while"):
		ps.next()
		c := ps.expr()
		return &WhileStmt{Cond: c, Body: ps.block(), Sp: ps.from(t.Pos)}
	case ps.is("for"):
		ps.next()
		v := ps.ident()
		ps.expect("in")
		lo := ps.expr()
		ps.expect("..")
		hi := ps.expr()
		return &ForStmt{Var: v.Text, Lo: lo, Hi: hi, Body: ps.block(), Sp: ps.from(t.Pos)}
	case ps.is("return"):
		ps.next()
		s := &ReturnStmt{}
		if !ps.is(";") {
			s.Value = ps.expr()
		}
		ps.expect(";")
		s.Sp = ps.from(t.Pos)
		return s
	case ps.is("defer"):
		ps.next()
		return &DeferStmt{Body: ps.block(), Sp: ps.from(t.Pos)}
	case ps.is("break"):
		ps.next()
		ps.expect(";")
		return &BreakStmt{Sp: ps.from(t.Pos)}
	case ps.is("continue"):
		ps.next()
		ps.expect(";")
		return &ContinueStmt{Sp: ps.from(t.Pos)}
	case ps.is("{"):
		return ps.block()
	}
	x := ps.expr()
	if c := ps.cur(); c.Kind == Punct && assignOps[c.Text] {
		ps.next()
		v := ps.expr()
		ps.expect(";")
		return &AssignStmt{Op: c.Text, Target: x, Value: v, Sp: ps.from(t.Pos)}
	}
	ps.expect(";")
	return &ExprStmt{X: x, Sp: ps.from(t.Pos)}
}

func (ps *Parser) ifStmt() *IfStmt {
	start := ps.next().Pos
	s := &IfStmt{Cond: ps.expr(), Then: ps.block()}
	if ps.accept("else") {
		if ps.is("if") {
			s.Else = ps.ifStmt()
		} else {
			s.Else = ps.block()
		}
	}
	s.Sp = ps.from(start)
	return s
}

// ---- expressions ----

func opGroup(op string) string {
	switch op {
	case "+", "-":
		return "add"
	case "*", "/", "%":
		return "mul"
	case "<<", ">>":
		return "shift"
	case "&", "|", "^":
		return "bit:" + op
	case "==", "!=", "<", "<=", ">", ">=":
		return "cmp"
	}
	return op
}

func (ps *Parser) expr() Expr { return ps.binary(0) }

var levels = [][]string{
	{"||"},
	{"&&"},
	{"==", "!=", "<", "<=", ">", ">="},
	{"|", "^", "&"},
	{"<<", ">>"},
	{"+", "-"},
	{"*", "/", "%"},
}

func (ps *Parser) binary(level int) Expr {
	if level == len(levels) {
		return ps.unary()
	}
	x := ps.binary(level + 1)
	for {
		t := ps.cur()
		if t.Kind != Punct || !contains(levels[level], t.Text) {
			return x
		}
		ps.next()
		y := ps.binary(level + 1)
		b := &BinaryExpr{Op: t.Text, X: x, Y: y, Sp: diag.Span{File: ps.file, Start: x.ESpan().Start, End: y.ESpan().End}}
		ps.checkMixing(b)
		x = b
	}
}

// checkMixing enforces Ingot's "no precedence puzzles" rule: bitwise and
// shift operators may not be combined with a different operator family
// without explicit parentheses, and comparisons do not chain.
func (ps *Parser) checkMixing(b *BinaryExpr) {
	g := opGroup(b.Op)
	for _, side := range []Expr{b.X, b.Y} {
		inner, ok := side.(*BinaryExpr)
		if !ok || inner.Parens {
			continue
		}
		ig := opGroup(inner.Op)
		bad := false
		switch {
		case g == "cmp" && (ig == "cmp" || ig == "shift" || ig[:min(3, len(ig))] == "bit"):
			// C parses `a & b == 0` as `a & (b == 0)`; Ingot refuses to guess.
			bad = true
		case g == "shift" || g[:min(3, len(g))] == "bit":
			bad = ig != g && ig != "cmp" && ig != "&&" && ig != "||"
		case ig == "shift" || ig[:min(3, len(ig))] == "bit":
			bad = g == "add" || g == "mul"
		}
		if bad {
			sp := inner.Sp
			d := ps.fail("E0106", sp, "operator %q mixed with %q requires explicit parentheses", inner.Op, b.Op)
			d.Note("Ingot has no implicit precedence between bitwise/shift operators and other operators")
			d.Fixes = append(d.Fixes, diag.Fix{Message: "parenthesize", Edits: []diag.Edit{
				{Span: diag.Span{File: sp.File, Start: sp.Start, End: sp.Start}, Text: "("},
				{Span: diag.Span{File: sp.File, Start: sp.End, End: sp.End}, Text: ")"},
			}})
		}
	}
}

func contains(s []string, x string) bool {
	for _, v := range s {
		if v == x {
			return true
		}
	}
	return false
}

// prefix parses prefix operators; '&x as T' means '(&x) as T'.
func (ps *Parser) prefix() Expr {
	t := ps.cur()
	if t.Kind == Punct && (t.Text == "-" || t.Text == "!" || t.Text == "~" || t.Text == "&") {
		ps.next()
		x := ps.prefix()
		return &UnaryExpr{Op: t.Text, X: x, Sp: diag.Span{File: ps.file, Start: t.Pos, End: x.ESpan().End}}
	}
	return ps.postfix()
}

func (ps *Parser) unary() Expr {
	x := ps.prefix()
	for ps.is("as") {
		ps.next()
		to := ps.typeExpr()
		x = &CastExpr{X: x, To: to, Sp: diag.Span{File: ps.file, Start: x.ESpan().Start, End: to.TSpan().End}}
	}
	return x
}

func (ps *Parser) postfix() Expr {
	x := ps.primary()
	for {
		start := x.ESpan().Start
		switch {
		case ps.is("("):
			ps.next()
			var args []Expr
			for !ps.is(")") {
				args = append(args, ps.expr())
				if !ps.accept(",") {
					break
				}
			}
			ps.expect(")")
			x = &CallExpr{Fn: x, Args: args, Sp: ps.from(start)}
		case ps.is("["):
			ps.next()
			idx := ps.expr()
			if ps.accept("..") {
				hi := ps.expr()
				ps.expect("]")
				x = &SliceExpr{X: x, Lo: idx, Hi: hi, Sp: ps.from(start)}
				continue
			}
			ps.expect("]")
			x = &IndexExpr{X: x, Index: idx, Sp: ps.from(start)}
		case ps.is(".*"):
			ps.next()
			x = &DerefExpr{X: x, Sp: ps.from(start)}
		case ps.is(".{"):
			id, ok := x.(*IdentExpr)
			if !ok {
				return x
			}
			x = ps.composite(&NamedType{Name: id.Name, Sp: id.Sp}, start)
		case ps.is("."):
			ps.next()
			f := ps.ident()
			x = &FieldExpr{X: x, Name: f.Text, Sp: ps.from(start)}
		default:
			return x
		}
	}
}

func (ps *Parser) composite(ty TypeExpr, start diag.Pos) Expr {
	ps.expect(".{")
	c := &CompositeLit{Type: ty}
	for !ps.is("}") {
		if ps.cur().Kind == Ident && ps.peek().Kind == Punct && ps.peek().Text == ":" {
			c.Names = append(c.Names, ps.next().Text)
			ps.next()
		} else if len(c.Names) > 0 {
			ps.fail("E0107", ps.sp(ps.cur()), "cannot mix named and positional fields in a composite literal")
			panic(bailout{})
		}
		c.Values = append(c.Values, ps.expr())
		if !ps.accept(",") {
			break
		}
	}
	ps.expect("}")
	if len(c.Names) > 0 && len(c.Names) != len(c.Values) {
		ps.fail("E0107", ps.from(start), "cannot mix named and positional fields in a composite literal")
	}
	c.Sp = ps.from(start)
	return c
}

func (ps *Parser) primary() Expr {
	t := ps.cur()
	switch t.Kind {
	case Ident:
		ps.next()
		return &IdentExpr{Name: t.Text, Sp: ps.sp(t)}
	case Int:
		ps.next()
		return &IntLit{Text: t.Text, Sp: ps.sp(t)}
	case Float:
		ps.next()
		return &FloatLit{Text: t.Text, Sp: ps.sp(t)}
	case String, CString:
		ps.next()
		return &StringLit{Val: t.Text, C: t.Kind == CString, Sp: ps.sp(t)}
	case Builtin:
		ps.next()
		b := &BuiltinCall{Name: t.Text[1:]}
		if b.Name == "zero" {
			b.Sp = ps.sp(t)
			return b
		}
		ps.expect("(")
		if typeArgBuiltins[b.Name] {
			b.Type = ps.typeExpr()
			if !ps.accept(",") {
				ps.expect(")")
				b.Sp = ps.from(t.Pos)
				return b
			}
		}
		for !ps.is(")") {
			b.Args = append(b.Args, ps.expr())
			if !ps.accept(",") {
				break
			}
		}
		ps.expect(")")
		b.Sp = ps.from(t.Pos)
		return b
	case Keyword:
		switch t.Text {
		case "true", "false":
			ps.next()
			return &BoolLit{Val: t.Text == "true", Sp: ps.sp(t)}
		case "null":
			ps.next()
			return &NullLit{Sp: ps.sp(t)}
		}
	case Punct:
		switch t.Text {
		case "(":
			ps.next()
			x := ps.expr()
			ps.expect(")")
			if b, ok := x.(*BinaryExpr); ok {
				b.Parens = true
				return b
			}
			return &ParenExpr{X: x, Sp: ps.from(t.Pos)}
		case "[":
			ty := ps.typeExpr()
			if !ps.is(".{") {
				ps.fail("E0108", ps.sp(ps.cur()), "expected '.{' after array type in expression")
				panic(bailout{})
			}
			return ps.composite(ty, t.Pos)
		}
	}
	ps.fail("E0109", ps.sp(t), "expected an expression, found %s", t)
	panic(bailout{})
}

// parseInt parses decimal, 0x and 0b literals with '_' separators.
func parseInt(s string) (int64, error) {
	clean := make([]byte, 0, len(s))
	for i := 0; i < len(s); i++ {
		if s[i] != '_' {
			clean = append(clean, s[i])
		}
	}
	u, err := strconv.ParseUint(string(clean), 0, 64)
	return int64(u), err
}

// ParseIntLit is exported for the checker.
func ParseIntLit(s string) (uint64, error) {
	clean := make([]byte, 0, len(s))
	for i := 0; i < len(s); i++ {
		if s[i] != '_' {
			clean = append(clean, s[i])
		}
	}
	return strconv.ParseUint(string(clean), 0, 64)
}
