package codegen

import (
	"fmt"
	"math"

	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/sema"
	"github.com/hossein1376/ingot/internal/syntax"
)

// expr evaluates e into a fresh pool register. Aggregates evaluate to their
// address. The bool result is false for void expressions.
func (g *gen) expr(e syntax.Expr) (reg, bool) {
	t := g.typeOf(e)
	if k, ok := g.info.Consts[e]; ok && (t.IsNumeric() || t == sema.TBool) {
		return g.constant(k, t, e.ESpan()), true
	}
	switch e := e.(type) {
	case *syntax.ParenExpr:
		return g.expr(e.X)
	case *syntax.NullLit:
		r := g.allocG(e.Sp)
		g.emit("mov %s, #0", x(r))
		return r, true
	case *syntax.StringLit:
		lbl := g.strLabel(e.Val)
		if e.C {
			r := g.allocG(e.Sp)
			g.addrOfSym(x(r), lbl)
			return r, true
		}
		off := g.slot(16, 8)
		r := g.allocG(e.Sp)
		g.addrOfSym("x9", lbl)
		g.emit("str x9, %s", g.mem(off))
		g.movImm("x9", uint64(len(e.Val)))
		g.emit("str x9, %s", g.mem(off-8))
		g.subImm(x(r), "x29", off)
		return r, true
	case *syntax.IdentExpr:
		switch sym := g.info.Uses[e].(type) {
		case *sema.Func:
			r := g.allocG(e.Sp)
			if sym.Extern {
				s := g.t.Sym(sym.Name)
				g.emit("adrp %s, %s", x(r), g.t.GotPage(s))
				g.emit("ldr %s, [%s, %s]", x(r), x(r), g.t.GotPageOff(s))
			} else {
				g.addrOfSym(x(r), g.funcSym(sym))
			}
			return r, true
		case *sema.Var:
			if sym.Type.IsAggregate() {
				return g.varAddr(sym, e.Sp), true
			}
			if pr, ok := g.promoted[sym]; ok {
				r := g.alloc(sym.Type, e.Sp)
				g.readPromoted(pr, sym.Type, r)
				return r, true
			}
			if !sym.Global {
				r := g.alloc(sym.Type, e.Sp)
				g.load(r, g.mem(g.slots[sym]), sym.Type)
				return r, true
			}
			a := g.varAddr(sym, e.Sp)
			r := g.alloc(sym.Type, e.Sp)
			g.load(r, "["+x(a)+"]", sym.Type)
			g.free(a)
			return r, true
		}
	case *syntax.UnaryExpr:
		return g.unary(e, t), true
	case *syntax.BinaryExpr:
		return g.binary(e, t), true
	case *syntax.CastExpr:
		return g.cast(e, t), true
	case *syntax.CallExpr:
		return g.call(e, t)
	case *syntax.TryExpr:
		return g.tryExpr(e, t)
	case *syntax.CatchExpr:
		return g.catchExpr(e, t)
	case *syntax.BuiltinCall:
		return g.builtin(e, t)
	case *syntax.FieldExpr:
		xt := g.typeOf(e.X)
		if xt.Kind == sema.Slice {
			base, _ := g.expr(e.X)
			off := 0
			if e.Name == "len" {
				off = 8
			}
			g.emit("ldr %s, [%s, #%d]", x(base), x(base), off)
			return base, true
		}
		a := g.addr(e)
		if t.IsAggregate() {
			return a, true
		}
		r := g.alloc(t, e.Sp)
		g.load(r, "["+x(a)+"]", t)
		g.free(a)
		return r, true
	case *syntax.IndexExpr:
		xt := g.typeOf(e.X)
		if xt.Kind == sema.Vec {
			vr, _ := g.expr(e.X)
			lane := g.info.Consts[e.Index].I
			r := g.alloc(t, e.Sp)
			switch {
			case t.Kind == sema.Float:
				g.emit("mov %s, %s.%s[%d]", rn(r, t), v(vr), laneSuffix(xt), lane)
			case t.Size == 8:
				g.emit("umov %s, %s.d[%d]", x(r), v(vr), lane)
			case t.Signed:
				g.emit("smov %s, %s.%s[%d]", x(r), v(vr), laneSuffix(xt), lane)
			default:
				g.emit("umov %s, %s.%s[%d]", w(r), v(vr), laneSuffix(xt), lane)
			}
			g.free(vr)
			return r, true
		}
		return g.loadLvalue(e, t), true
	case *syntax.DerefExpr:
		return g.loadLvalue(e, t), true
	case *syntax.SliceExpr:
		return g.sliceExpr(e), true
	case *syntax.CompositeLit:
		off := g.slot(t.Size, t.Align)
		g.subImm("x14", "x29", off)
		g.zeroMem("x14", t.Size)
		for i, val := range e.Values {
			var fo int64
			var ft *sema.Type
			if t.Kind == sema.Array {
				fo, ft = int64(i)*t.Elem.Size, t.Elem
			} else {
				for _, f := range t.Fields {
					if f.Name == e.Names[i] {
						fo, ft = f.Offset, f.Type
					}
				}
			}
			r, _ := g.expr(val)
			g.subImm("x14", "x29", off-fo)
			if ft.IsAggregate() {
				g.copyMem("x14", x(r), ft.Size)
			} else {
				g.store(r, "[x14]", ft)
			}
			g.free(r)
		}
		r := g.allocG(e.Sp)
		g.subImm(x(r), "x29", off)
		return r, true
	}
	g.errorf(e.ESpan(), "codegen: unsupported expression")
	return reg{}, false
}

// funcSym returns the assembly symbol of a function. Private functions are
// prefixed so they can never collide with libc or runtime symbols.
func (g *gen) funcSym(f *sema.Func) string {
	if f.Extern || f.Pub || f.Name == "main" {
		return g.t.Sym(f.Name)
	}
	return g.t.Sym("ingot__" + f.Name)
}

func (g *gen) loadLvalue(e syntax.Expr, t *sema.Type) reg {
	a := g.addr(e)
	if t.IsAggregate() {
		return a
	}
	r := g.alloc(t, e.ESpan())
	g.load(r, "["+x(a)+"]", t)
	g.free(a)
	return r
}

func (g *gen) constant(k sema.Const, t *sema.Type, sp diag.Span) reg {
	r := g.alloc(t, sp)
	if t.Kind == sema.Float {
		f := k.F
		if !k.IsFloat {
			f = float64(k.I)
		}
		if t.Size == 4 {
			bits := math.Float32bits(float32(f))
			if bits == 0 {
				g.emit("fmov %s, wzr", rn(r, t))
			} else {
				g.movImm("x16", uint64(bits))
				g.emit("fmov %s, w16", rn(r, t))
			}
		} else {
			bits := math.Float64bits(f)
			if bits == 0 {
				g.emit("fmov %s, xzr", rn(r, t))
			} else {
				g.movImm("x16", bits)
				g.emit("fmov %s, x16", rn(r, t))
			}
		}
		return r
	}
	val := k.I
	if k.IsFloat {
		val = int64(k.F)
	}
	g.movImm(x(r), uint64(val))
	g.normalize(r, t)
	return r
}

// varAddr loads the address of a variable's storage.
func (g *gen) varAddr(lv *sema.Var, sp diag.Span) reg {
	r := g.allocG(sp)
	switch {
	case lv.Global:
		g.addrOfSym(x(r), g.t.Sym(lv.Name))
	case g.byRef[lv]:
		g.emit("ldr %s, %s", x(r), g.mem(g.slots[lv]))
	default:
		g.subImm(x(r), "x29", g.slots[lv])
	}
	return r
}

// addr computes the address of an lvalue (or of an aggregate temporary).
func (g *gen) addr(e syntax.Expr) reg {
	switch e := e.(type) {
	case *syntax.ParenExpr:
		return g.addr(e.X)
	case *syntax.IdentExpr:
		if lv, ok := g.info.Uses[e].(*sema.Var); ok {
			return g.varAddr(lv, e.Sp)
		}
	case *syntax.DerefExpr:
		r, _ := g.expr(e.X)
		return r
	case *syntax.FieldExpr:
		base := g.aggAddr(e.X)
		st := g.typeOf(e.X)
		for _, f := range st.Fields {
			if f.Name == e.Name {
				g.addImm(x(base), x(base), f.Offset)
			}
		}
		return base
	case *syntax.IndexExpr:
		xt := g.typeOf(e.X)
		et := xt.Elem
		var base reg
		var lenReg string
		switch xt.Kind {
		case sema.Array:
			base = g.aggAddr(e.X)
		case sema.Slice:
			base = g.aggAddr(e.X)
			if g.opt.Safe {
				g.emit("ldr x15, [%s, #8]", x(base))
				lenReg = "x15"
			}
			g.emit("ldr %s, [%s]", x(base), x(base))
		case sema.Ptr:
			pb := g.use(e.X)
			if pb.abs != 0 {
				if _, isK := g.info.Consts[e.Index]; !isK && (et.Size == 1 || et.Size == 2 || et.Size == 4 || et.Size == 8 || et.Size == 16) {
					// base lives in a promoted register: one add forms the address
					idx := g.use(e.Index)
					base = g.allocG(e.Sp)
					g.emit("add %s, x%d, %s, lsl #%d", x(base), pb.abs, x(idx), log2(et.Size))
					g.free(idx)
					return base
				}
				base = g.allocG(e.Sp)
				g.emit("mov %s, x%d", x(base), pb.abs)
			} else {
				base = pb
			}
		case sema.Vec:
			base = g.aggAddr(e.X)
		}
		if k, ok := g.info.Consts[e.Index]; ok && xt.Kind != sema.Slice {
			g.addImm(x(base), x(base), k.I*et.Size)
			return base
		}
		idx := g.use(e.Index)
		if g.opt.Safe && (xt.Kind == sema.Array || xt.Kind == sema.Slice) {
			if xt.Kind == sema.Array {
				g.movImm("x15", uint64(xt.Len))
			} else if lenReg == "" {
				g.emit("ldr x15, [%s, #8]", x(base))
			}
			g.emit("cmp %s, x15", x(idx))
			g.emit("b.hs %s", g.panicStub(e.Index.ESpan(), "index out of bounds"))
		}
		g.scaledAdd(base, idx, et.Size)
		g.free(idx)
		return base
	}
	t := g.typeOf(e)
	if t != nil && t.IsAggregate() {
		r, _ := g.expr(e)
		return r
	}
	// scalar rvalue: spill to a temporary so it has an address
	r, _ := g.expr(e)
	off := g.slot(max(t.Size, 1), max(t.Align, 1))
	g.store(r, g.mem(off), t)
	g.free(r)
	a := g.allocG(e.ESpan())
	g.subImm(x(a), "x29", off)
	return a
}

// aggAddr returns the address of an aggregate-typed expression.
func (g *gen) aggAddr(e syntax.Expr) reg {
	switch e.(type) {
	case *syntax.IdentExpr, *syntax.FieldExpr, *syntax.IndexExpr, *syntax.DerefExpr, *syntax.ParenExpr:
		return g.addr(e)
	}
	r, _ := g.expr(e)
	return r
}

func (g *gen) scaledAdd(base, idx reg, size int64) {
	switch size {
	case 1, 2, 4, 8, 16:
		g.emit("add %s, %s, %s, lsl #%d", x(base), x(base), x(idx), log2(size))
	default:
		g.movImm("x16", uint64(size))
		g.emit("madd %s, %s, x16, %s", x(base), x(idx), x(base))
	}
}

func (g *gen) sliceExpr(e *syntax.SliceExpr) reg {
	xt := g.typeOf(e.X)
	et := xt.Elem
	var base reg
	off := g.slot(16, 8)
	switch xt.Kind {
	case sema.Array:
		base = g.aggAddr(e.X)
		g.movImm("x15", uint64(xt.Len))
		g.emit("str x15, %s", g.mem(off-8))
	case sema.Slice:
		base = g.aggAddr(e.X)
		g.emit("ldr x15, [%s, #8]", x(base))
		g.emit("str x15, %s", g.mem(off-8))
		g.emit("ldr %s, [%s]", x(base), x(base))
	case sema.Ptr:
		base, _ = g.expr(e.X)
		g.emit("mov x15, #-1")
		g.emit("str x15, %s", g.mem(off-8))
	}
	lo, _ := g.expr(e.Lo)
	hi, _ := g.expr(e.Hi)
	if g.opt.Safe {
		g.emit("ldr x15, %s", g.mem(off-8))
		g.emit("cmp %s, x15", x(hi))
		stub := g.panicStub(e.Sp, "slice bounds out of range")
		g.emit("b.hi %s", stub)
		g.emit("cmp %s, %s", x(lo), x(hi))
		g.emit("b.hi %s", stub)
	}
	g.scaledAdd(base, lo, et.Size)
	g.emit("str %s, %s", x(base), g.mem(off))
	g.emit("sub %s, %s, %s", x(hi), x(hi), x(lo))
	g.emit("str %s, %s", x(hi), g.mem(off-8))
	g.free(lo)
	g.free(hi)
	g.subImm(x(base), "x29", off)
	return base
}

func (g *gen) unary(e *syntax.UnaryExpr, t *sema.Type) reg {
	if e.Op == "&" {
		return g.addr(e.X)
	}
	r, _ := g.expr(e.X)
	switch e.Op {
	case "-":
		switch t.Kind {
		case sema.Float:
			g.emit("fneg %s, %s", rn(r, t), rn(r, t))
		case sema.Vec:
			op := "neg"
			if t.Elem.Kind == sema.Float {
				op = "fneg"
			}
			g.emit("%s %s.%s, %s.%s", op, v(r), arr(t), v(r), arr(t))
		default:
			g.emit("neg %s, %s", x(r), x(r))
			g.normalize(r, t)
		}
	case "!":
		g.emit("eor %s, %s, #1", x(r), x(r))
	case "~":
		g.emit("mvn %s, %s", x(r), x(r))
		g.normalize(r, t)
	}
	return r
}

var intCond = map[string][2]string{ // op -> {signed, unsigned}
	"==": {"eq", "eq"}, "!=": {"ne", "ne"},
	"<": {"lt", "lo"}, "<=": {"le", "ls"}, ">": {"gt", "hi"}, ">=": {"ge", "hs"},
}

// float conditions are chosen so that comparisons with NaN are false.
var floatCond = map[string]string{"==": "eq", "!=": "ne", "<": "mi", "<=": "ls", ">": "gt", ">=": "ge"}

var invert = map[string]string{"eq": "ne", "ne": "eq", "lt": "ge", "ge": "lt", "le": "gt", "gt": "le",
	"lo": "hs", "hs": "lo", "ls": "hi", "hi": "ls", "mi": "pl", "pl": "mi"}

// compare emits a comparison and returns the condition code for "true".
func (g *gen) compare(e *syntax.BinaryExpr) string {
	ot := g.typeOf(e.X)
	if ot == sema.TNull {
		ot = g.typeOf(e.Y)
	}
	if k, ok := g.info.Consts[e.Y]; ok && k.I == 0 && (e.Op == "==" || e.Op == "!=") && ot.Kind == sema.Int {
		if and, ok := e.X.(*syntax.BinaryExpr); ok && and.Op == "&" {
			if m, ok := g.info.Consts[and.Y]; ok && isLogicalImm(uint64(m.I)) {
				a := g.use(and.X)
				g.emit("tst %s, #%d", x(a), uint64(m.I))
				g.free(a)
				return intCond[e.Op][0]
			}
		}
	}
	a := g.use(e.X)
	if k, ok := g.info.Consts[e.Y]; ok && ot.Kind == sema.Int && !k.IsFloat && k.I >= 0 && k.I < 4096 {
		g.emit("cmp %s, #%d", x(a), k.I)
		g.free(a)
		pair := intCond[e.Op]
		if ot.Signed {
			return pair[0]
		}
		return pair[1]
	}
	b := g.use(e.Y)
	var cc string
	if ot.Kind == sema.Float {
		g.emit("fcmp %s, %s", rn(a, ot), rn(b, ot))
		cc = floatCond[e.Op]
	} else {
		g.emit("cmp %s, %s", x(a), x(b))
		pair := intCond[e.Op]
		cc = pair[0]
		if ot.Kind != sema.Int || !ot.Signed {
			cc = pair[1]
		}
	}
	g.free(a)
	g.free(b)
	return cc
}

func isCmp(op string) bool {
	_, ok := intCond[op]
	return ok
}

// jumpIf branches to label when cond evaluates to sense.
func (g *gen) jumpIf(cond syntax.Expr, label string, sense bool) {
	if k, ok := g.info.Consts[cond]; ok {
		if (k.I != 0) == sense {
			g.emit("b %s", label)
		}
		return
	}
	switch c := cond.(type) {
	case *syntax.ParenExpr:
		g.jumpIf(c.X, label, sense)
		return
	case *syntax.UnaryExpr:
		if c.Op == "!" {
			g.jumpIf(c.X, label, !sense)
			return
		}
	case *syntax.BinaryExpr:
		switch {
		case c.Op == "&&" && !sense:
			g.jumpIf(c.X, label, false)
			g.jumpIf(c.Y, label, false)
			return
		case c.Op == "&&":
			skip := g.newLabel("and")
			g.jumpIf(c.X, skip, false)
			g.jumpIf(c.Y, label, true)
			g.label(skip)
			return
		case c.Op == "||" && sense:
			g.jumpIf(c.X, label, true)
			g.jumpIf(c.Y, label, true)
			return
		case c.Op == "||":
			skip := g.newLabel("or")
			g.jumpIf(c.X, skip, true)
			g.jumpIf(c.Y, label, false)
			g.label(skip)
			return
		case isCmp(c.Op):
			cc := g.compare(c)
			if !sense {
				cc = invert[cc]
			}
			g.emit("b.%s %s", cc, label)
			return
		}
	}
	r, _ := g.expr(cond)
	if sense {
		g.emit("cbnz %s, %s", w(r), label)
	} else {
		g.emit("cbz %s, %s", w(r), label)
	}
	g.free(r)
}

func (g *gen) binary(e *syntax.BinaryExpr, t *sema.Type) reg {
	if e.Op == "&&" || e.Op == "||" {
		r := g.allocG(e.Sp)
		f, end := g.newLabel("bf"), g.newLabel("bend")
		g.jumpIf(e, f, false)
		g.emit("mov %s, #1", x(r))
		g.emit("b %s", end)
		g.label(f)
		g.emit("mov %s, #0", x(r))
		g.label(end)
		return r
	}
	if isCmp(e.Op) {
		cc := g.compare(e)
		r := g.allocG(e.Sp)
		g.emit("cset %s, %s", w(r), cc)
		return r
	}
	ex, ey := g.operands(e, t)
	a := g.use(ex)
	d := a
	if a.abs != 0 {
		d = g.alloc(t, e.Sp)
	}
	g.arithExpr3(e.Op, d, a, ey, t, e.Sp)
	return d
}

// use evaluates e for reading only. A promoted variable is returned as a
// borrowed register (no copy); anything else gets a fresh temporary.
func (g *gen) use(e syntax.Expr) reg {
	if p, ok := e.(*syntax.ParenExpr); ok {
		return g.use(p.X)
	}
	if id, ok := e.(*syntax.IdentExpr); ok {
		if lv, ok := g.info.Uses[id].(*sema.Var); ok {
			if pr, ok := g.promoted[lv]; ok {
				return reg{abs: pr.n, fp: pr.fp}
			}
		}
	}
	r, _ := g.expr(e)
	return r
}

// operands puts an integer constant on the right of a commutative operator,
// where the immediate forms can use it.
func (g *gen) operands(e *syntax.BinaryExpr, t *sema.Type) (syntax.Expr, syntax.Expr) {
	switch e.Op {
	case "+", "*", "&", "|", "^":
		_, kx := g.info.Consts[e.X]
		_, ky := g.info.Consts[e.Y]
		if kx && !ky && t.Kind == sema.Int {
			return e.Y, e.X
		}
	}
	return e.X, e.Y
}

// arithExpr computes a = a op y.
func (g *gen) arithExpr(op string, a reg, y syntax.Expr, t *sema.Type, sp diag.Span) {
	g.arithExpr3(op, a, a, y, t, sp)
}

// arithExpr3 computes d = a op y, using an immediate form when y is a
// small constant (add/sub #imm12, shifts by #imm).
func (g *gen) arithExpr3(op string, d, a reg, y syntax.Expr, t *sema.Type, sp diag.Span) {
	if k, ok := g.info.Consts[y]; ok && t.Kind == sema.Int && !k.IsFloat {
		switch {
		case (op == "+" || op == "-") && k.I >= 0 && k.I < 4096:
			ins := map[string]string{"+": "add", "-": "sub"}[op]
			g.emit("%s %s, %s, #%d", ins, x(d), x(a), k.I)
			g.normalize(d, t)
			return
		case (op == "&" || op == "|" || op == "^") && isLogicalImm(uint64(k.I)):
			ins := map[string]string{"&": "and", "|": "orr", "^": "eor"}[op]
			g.emit("%s %s, %s, #%d", ins, x(d), x(a), uint64(k.I))
			g.normalize(d, t)
			return
		case op == "*" && k.I > 0 && k.I&(k.I-1) == 0:
			g.emit("lsl %s, %s, #%d", x(d), x(a), log2(k.I))
			g.normalize(d, t)
			return
		case op == "*" && k.I > 2 && (k.I-1)&(k.I-2) == 0:
			// 2^s+1: a + (a << s)
			g.emit("add %s, %s, %s, lsl #%d", x(d), x(a), x(a), log2(k.I-1))
			g.normalize(d, t)
			return
		case (op == "<<" || op == ">>") && k.I >= 0 && k.I < t.Size*8:
			ins := "lsl"
			if op == ">>" {
				ins = "lsr"
				if t.Signed {
					ins = "asr"
				}
			}
			g.emit("%s %s, %s, #%d", ins, x(d), x(a), k.I)
			g.normalize(d, t)
			return
		}
	}
	b := g.use(y)
	g.arith3(op, d, a, b, t, sp)
	g.free(b)
}

// isLogicalImm reports whether u is a contiguous run of ones (possibly
// shifted), a subset of AArch64's bitmask immediates.
func isLogicalImm(u uint64) bool {
	if u == 0 || u == ^uint64(0) {
		return false
	}
	for u&1 == 0 {
		u >>= 1
	}
	return u&(u+1) == 0
}

// arith computes a = a op b for type t.
func (g *gen) arith(op string, a, b reg, t *sema.Type, sp diag.Span) {
	g.arith3(op, a, a, b, t, sp)
}

// arith3 computes d = a op b for type t (d may equal a; b is read-only).
func (g *gen) arith3(op string, d, a, b reg, t *sema.Type, sp diag.Span) {
	switch t.Kind {
	case sema.Float:
		ins := map[string]string{"+": "fadd", "-": "fsub", "*": "fmul", "/": "fdiv"}[op]
		g.emit("%s %s, %s, %s", ins, rn(d, t), rn(a, t), rn(b, t))
		return
	case sema.Vec:
		var ins string
		if t.Elem.Kind == sema.Float {
			ins = map[string]string{"+": "fadd", "-": "fsub", "*": "fmul", "/": "fdiv"}[op]
		} else {
			ins = map[string]string{"+": "add", "-": "sub", "*": "mul", "&": "and", "|": "orr", "^": "eor"}[op]
			if op == "*" && t.Elem.Size == 8 {
				g.errorf(sp, "i64x2 multiplication has no AArch64 NEON instruction")
			}
		}
		ar := arr(t)
		if op == "&" || op == "|" || op == "^" {
			ar = "16b"
		}
		g.emit("%s %s.%s, %s.%s, %s.%s", ins, v(d), ar, v(a), ar, v(b), ar)
		return
	}
	D, A, B := x(d), x(a), x(b)
	switch op {
	case "+":
		g.emit("add %s, %s, %s", D, A, B)
	case "-":
		g.emit("sub %s, %s, %s", D, A, B)
	case "*":
		g.emit("mul %s, %s, %s", D, A, B)
	case "/", "%":
		if g.opt.Safe {
			g.emit("cbz %s, %s", B, g.panicStub(sp, "division by zero"))
		}
		div := "udiv"
		if t.Signed {
			div = "sdiv"
		}
		if op == "/" {
			g.emit("%s %s, %s, %s", div, D, A, B)
		} else {
			g.emit("%s x16, %s, %s", div, A, B)
			g.emit("msub %s, x16, %s, %s", D, B, A)
		}
	case "&":
		g.emit("and %s, %s, %s", D, A, B)
	case "|":
		g.emit("orr %s, %s, %s", D, A, B)
	case "^":
		g.emit("eor %s, %s, %s", D, A, B)
	case "<<", ">>":
		amt := B
		if t.Size < 8 {
			g.emit("and x16, %s, #%d", B, t.Size*8-1)
			amt = "x16"
		}
		ins := "lsl"
		if op == ">>" {
			ins = "lsr"
			if t.Signed {
				ins = "asr"
			}
		}
		g.emit("%s %s, %s, %s", ins, D, A, amt)
	}
	g.normalize(d, t)
}

func (g *gen) cast(e *syntax.CastExpr, to *sema.Type) reg {
	from := g.typeOf(e.X)
	r, _ := g.expr(e.X)
	switch {
	case from.Kind == sema.Float && to.Kind == sema.Float:
		if from.Size == to.Size {
			return r
		}
		g.emit("fcvt %s, %s", rn(r, to), rn(r, from))
		return r
	case from.Kind == sema.Float: // float -> int (saturating, NaN -> 0)
		d := g.allocG(e.Sp)
		ins := "fcvtzs"
		if !to.Signed {
			ins = "fcvtzu"
		}
		g.emit("%s %s, %s", ins, x(d), rn(r, from))
		g.free(r)
		g.normalize(d, to)
		return d
	case to.Kind == sema.Float: // int -> float
		d := g.allocF(e.Sp)
		ins := "scvtf"
		if !from.Signed || from.Kind != sema.Int {
			ins = "ucvtf"
		}
		g.emit("%s %s, %s", ins, rn(d, to), x(r))
		g.free(r)
		return d
	case to.Kind == sema.Int:
		g.normalize(r, to)
	}
	return r
}

// ---- calls ----

type argLoc struct {
	r     reg
	t     *sema.Type
	vari  bool
	stack bool
	off   int64
	n     int
}

func (g *gen) call(e *syntax.CallExpr, ret *sema.Type) (reg, bool) {
	if cb := g.inlineBody(e); cb != nil {
		return g.inlineCall(e, cb, ret)
	}
	ft := g.typeOf(e.Fn)
	var direct string
	var callee reg
	if id, ok := e.Fn.(*syntax.IdentExpr); ok {
		if fn, ok := g.info.Uses[id].(*sema.Func); ok {
			direct = g.funcSym(fn)
		}
	}
	if direct == "" {
		callee, _ = g.expr(e.Fn)
	}
	var args []argLoc
	for i, a := range e.Args {
		at := g.typeOf(a)
		if at == sema.TNull {
			at = sema.TI64
		}
		var r reg
		if at.IsAggregate() {
			r, _ = g.expr(a)
		} else {
			r = g.use(a)
		}
		if at.IsAggregate() {
			// by-value semantics: pass a pointer to a private copy
			off := g.slot(at.Size, at.Align)
			g.subImm("x14", "x29", off)
			g.copyMem("x14", x(r), at.Size)
			g.subImm(x(r), "x29", off)
		}
		args = append(args, argLoc{r: r, t: at, vari: i >= len(ft.Params)})
	}
	if g.errFor == e {
		g.errNext, g.errFor = true, nil
	}
	return g.emitCall(direct, callee, args, ret, e.Sp), ret != sema.TVoid
}

// emitCall places arguments per the target ABI, performs the call and
// returns the result register.
func (g *gen) emitCall(direct string, callee reg, args []argLoc, ret *sema.Type, sp diag.Span) reg {
	ng, nf := 0, 0
	var stack int64
	for i := range args {
		a := &args[i]
		fp := a.t.IsFPR()
		size := a.t.Size
		if a.t.IsAggregate() {
			size = 8
		}
		if a.vari && g.t.Darwin {
			// Apple arm64: variadic arguments always go on the stack, 8-byte slots.
			a.stack = true
			a.off = (stack + 7) / 8 * 8
			stack = a.off + 8
			continue
		}
		if fp && nf < 8 {
			a.n = nf
			nf++
			continue
		}
		if !fp && ng < 8 {
			a.n = ng
			ng++
			continue
		}
		a.stack = true
		a.off = stackSlot(&stack, size, g.t.Darwin)
	}
	g.outArgs = max(g.outArgs, (stack+15)/16*16)
	// spill live vector/float temporaries that are not arguments
	argF := map[int]bool{}
	for _, a := range args {
		if a.r.fp && a.r.abs == 0 {
			argF[a.r.n] = true
		}
	}
	var spilled []int
	for i := 0; i < fprCount; i++ {
		if g.fpr[i] && !argF[i] {
			if g.fprSpill[i] == 0 {
				g.fprSpill[i] = g.slot(16, 16)
			}
			g.emit("str q%d, %s", fprBase+i, g.mem(g.fprSpill[i]))
			spilled = append(spilled, i)
		}
	}
	for _, a := range args {
		switch {
		case a.stack:
			if a.r.fp {
				if a.vari {
					g.emit("str %s, [sp, #%d]", rn(a.r, a.t), a.off)
				} else {
					g.emit("str %s, [sp, #%d]", rn(a.r, a.t), a.off)
				}
			} else {
				sz := a.t.Size
				if a.vari || a.t.IsAggregate() || !g.t.Darwin {
					sz = 8
				}
				switch sz {
				case 1:
					g.emit("strb %s, [sp, #%d]", w(a.r), a.off)
				case 2:
					g.emit("strh %s, [sp, #%d]", w(a.r), a.off)
				case 4:
					g.emit("str %s, [sp, #%d]", w(a.r), a.off)
				default:
					g.emit("str %s, [sp, #%d]", x(a.r), a.off)
				}
			}
		case a.r.fp:
			g.emit("mov v%d.16b, %s.16b", a.n, v(a.r))
		default:
			g.emit("mov x%d, %s", a.n, x(a.r))
		}
		g.free(a.r)
	}
	var resOff int64
	if ret.IsAggregate() {
		resOff = g.slot(ret.Size, ret.Align)
		g.subImm("x8", "x29", resOff)
	}
	if direct != "" {
		g.emit("bl %s", direct)
	} else {
		g.emit("blr %s", x(callee))
		g.free(callee)
	}
	if g.errNext {
		// fallible call: the error code comes back in x1 (0 = success)
		g.emit("mov %s, x1", x(g.errReg))
		g.errNext = false
	}
	for _, i := range spilled {
		g.emit("ldr q%d, %s", fprBase+i, g.mem(g.fprSpill[i]))
	}
	switch {
	case ret == sema.TVoid:
		return reg{}
	case ret.IsAggregate():
		r := g.allocG(sp)
		g.subImm(x(r), "x29", resOff)
		return r
	case ret.IsFPR():
		r := g.allocF(sp)
		g.emit("mov %s.16b, v0.16b", v(r))
		return r
	}
	r := g.allocG(sp)
	g.emit("mov %s, x0", x(r))
	g.normalize(r, ret) // callee may leave upper bits undefined for narrow types
	return r
}

// ---- builtins ----

func (g *gen) builtin(e *syntax.BuiltinCall, t *sema.Type) (reg, bool) {
	switch e.Name {
	case "zero":
		if t.IsAggregate() {
			off := g.slot(t.Size, t.Align)
			g.subImm("x14", "x29", off)
			g.zeroMem("x14", t.Size)
			r := g.allocG(e.Sp)
			g.subImm(x(r), "x29", off)
			return r, true
		}
		r := g.alloc(t, e.Sp)
		if r.fp {
			g.emit("movi %s.16b, #0", v(r))
		} else {
			g.emit("mov %s, #0", x(r))
		}
		return r, true
	case "assert":
		ok := g.newLabel("assertok")
		g.jumpIf(e.Args[0], ok, true)
		msg := "assertion failed"
		if len(e.Args) == 2 {
			msg += ": " + e.Args[1].(*syntax.StringLit).Val
		} else if src := g.srcText(e.Args[0].ESpan()); src != "" {
			msg += ": " + src
		}
		g.emit("b %s", g.panicStub(e.Sp, msg))
		g.label(ok)
		return reg{}, false
	case "panic":
		g.emit("b %s", g.panicStub(e.Sp, "panic: "+e.Args[0].(*syntax.StringLit).Val))
		return reg{}, false
	case "trap":
		g.emit("brk #1")
		return reg{}, false
	case "unreachable":
		g.emit("b %s", g.panicStub(e.Sp, "reached @unreachable()"))
		return reg{}, false
	case "fence":
		g.emit("dmb ish")
		return reg{}, false
	case "is_macos":
		r := g.allocG(e.Sp)
		g.emit("mov %s, #%d", x(r), map[bool]int{true: 1}[g.t.Darwin])
		return r, true
	case "ticks", "tick_freq":
		r := g.allocG(e.Sp)
		g.emit("mrs %s, %s", x(r), map[string]string{"ticks": "cntvct_el0", "tick_freq": "cntfrq_el0"}[e.Name])
		return r, true
	case "splat":
		a, _ := g.expr(e.Args[0])
		r := g.allocF(e.Sp)
		if a.fp {
			g.emit("dup %s.%s, %s.%s[0]", v(r), arr(t), v(a), laneSuffix(t))
		} else if t.Elem.Size == 8 {
			g.emit("dup %s.%s, %s", v(r), arr(t), x(a))
		} else {
			g.emit("dup %s.%s, %s", v(r), arr(t), w(a))
		}
		g.free(a)
		return r, true
	case "load":
		p := g.use(e.Args[0])
		r := g.allocF(e.Sp)
		g.emit("ldr %s, [%s]", rn(r, t), x(p))
		g.free(p)
		return r, true
	case "store":
		p, _ := g.expr(e.Args[0])
		val, _ := g.expr(e.Args[1])
		g.emit("str q%d, [%s]", fprBase+val.n, x(p))
		g.free(p)
		g.free(val)
		return reg{}, false
	case "prefetch":
		p, _ := g.expr(e.Args[0])
		g.emit("prfm pldl1keep, [%s]", x(p))
		g.free(p)
		return reg{}, false
	case "reduce_add", "reduce_min", "reduce_max":
		vt := g.typeOf(e.Args[0])
		a, _ := g.expr(e.Args[0])
		ls := laneSuffix(vt)
		if vt.Elem.Kind == sema.Float {
			switch {
			case e.Name == "reduce_add" && vt.Elem.Size == 4:
				g.emit("faddp %s.4s, %s.4s, %s.4s", v(a), v(a), v(a))
				g.emit("faddp s%d, %s.2s", fprBase+a.n, v(a))
			case e.Name == "reduce_add":
				g.emit("faddp d%d, %s.2d", fprBase+a.n, v(a))
			default:
				g.emit("%s s%d, %s.4s", map[string]string{"reduce_min": "fminv", "reduce_max": "fmaxv"}[e.Name], fprBase+a.n, v(a))
			}
			return a, true
		}
		r := g.allocG(e.Sp)
		switch {
		case vt.Elem.Size == 8:
			g.emit("addp d%d, %s.2d", fprBase+a.n, v(a))
			g.emit("fmov %s, d%d", x(r), fprBase+a.n)
		default:
			ins := map[string]string{"reduce_add": "addv", "reduce_min": "minv", "reduce_max": "maxv"}[e.Name]
			if e.Name != "reduce_add" {
				if vt.Elem.Signed {
					ins = "s" + ins
				} else {
					ins = "u" + ins
				}
			}
			g.emit("%s %s%d, %s.%s", ins, ls, fprBase+a.n, v(a), arr(vt))
			g.emit("umov %s, %s.%s[0]", w(r), v(a), ls)
		}
		g.free(a)
		g.normalize(r, vt.Elem)
		return r, true
	case "sqrt":
		a, _ := g.expr(e.Args[0])
		if t.Kind == sema.Vec {
			g.emit("fsqrt %s.%s, %s.%s", v(a), arr(t), v(a), arr(t))
		} else {
			g.emit("fsqrt %s, %s", rn(a, t), rn(a, t))
		}
		return a, true
	case "select":
		a, _ := g.expr(e.Args[1])
		b := g.use(e.Args[2])
		cc := g.selectCond(e.Args[0])
		if a.fp {
			g.emit("fcsel %s, %s, %s, %s", rn(a, t), rn(a, t), rn(b, t), cc)
		} else {
			g.emit("csel %s, %s, %s, %s", x(a), x(a), x(b), cc)
		}
		g.free(b)
		return a, true
	case "fma":
		a := g.use(e.Args[0])
		b := g.use(e.Args[1])
		c, _ := g.expr(e.Args[2])
		if t.Kind == sema.Vec {
			g.emit("fmla %s.%s, %s.%s, %s.%s", v(c), arr(t), v(a), arr(t), v(b), arr(t))
		} else {
			g.emit("fmadd %s, %s, %s, %s", rn(c, t), rn(a, t), rn(b, t), rn(c, t))
		}
		g.free(a)
		g.free(b)
		return c, true
	case "min", "max":
		a, _ := g.expr(e.Args[0])
		b, _ := g.expr(e.Args[1])
		switch {
		case t.Kind == sema.Vec:
			ins := e.Name
			switch {
			case t.Elem.Kind == sema.Float:
				ins = "f" + ins
			case t.Elem.Signed:
				ins = "s" + ins
			default:
				ins = "u" + ins
			}
			g.emit("%s %s.%s, %s.%s, %s.%s", ins, v(a), arr(t), v(a), arr(t), v(b), arr(t))
		case t.Kind == sema.Float:
			g.emit("f%s %s, %s, %s", e.Name, rn(a, t), rn(a, t), rn(b, t))
		default:
			cc := map[bool]string{true: "lt", false: "lo"}[t.Signed]
			if e.Name == "max" {
				cc = map[bool]string{true: "gt", false: "hi"}[t.Signed]
			}
			g.emit("cmp %s, %s", x(a), x(b))
			g.emit("csel %s, %s, %s, %s", x(a), x(a), x(b), cc)
		}
		g.free(b)
		return a, true
	case "atomic_load":
		p, _ := g.expr(e.Args[0])
		r := g.allocG(e.Sp)
		if t.Size == 4 {
			g.emit("ldar %s, [%s]", w(r), x(p))
		} else {
			g.emit("ldar %s, [%s]", x(r), x(p))
		}
		g.free(p)
		g.normalize(r, t)
		return r, true
	case "atomic_store":
		p, _ := g.expr(e.Args[0])
		val, _ := g.expr(e.Args[1])
		rt := g.typeOf(e.Args[1])
		if rt.Size == 4 {
			g.emit("stlr %s, [%s]", w(val), x(p))
		} else {
			g.emit("stlr %s, [%s]", x(val), x(p))
		}
		g.free(p)
		g.free(val)
		return reg{}, false
	case "atomic_add":
		p, _ := g.expr(e.Args[0])
		val, _ := g.expr(e.Args[1])
		if t.Size == 4 {
			g.emit("ldaddal %s, %s, [%s]", w(val), w(val), x(p))
		} else {
			g.emit("ldaddal %s, %s, [%s]", x(val), x(val), x(p))
		}
		g.free(p)
		g.normalize(val, t)
		return val, true
	case "atomic_cas":
		p, _ := g.expr(e.Args[0])
		old, _ := g.expr(e.Args[1])
		nw, _ := g.expr(e.Args[2])
		if t.Size == 4 {
			g.emit("casal %s, %s, [%s]", w(old), w(nw), x(p))
		} else {
			g.emit("casal %s, %s, [%s]", x(old), x(nw), x(p))
		}
		g.free(p)
		g.free(nw)
		g.normalize(old, t)
		return old, true
	case "qos":
		a, _ := g.expr(e.Args[0])
		if !g.t.Darwin {
			g.emit("mov %s, #0", x(a))
			return a, true
		}
		zero := g.allocG(e.Sp)
		g.emit("mov %s, #0", x(zero))
		return g.emitCall(g.t.Sym("pthread_set_qos_class_self_np"), reg{}, []argLoc{{r: a, t: sema.TI32}, {r: zero, t: sema.TI32}}, sema.TI32, e.Sp), true
	}
	g.errorf(e.Sp, "codegen: unsupported builtin @%s", e.Name)
	return reg{}, false
}

var _ = fmt.Sprintf

func unparen(e syntax.Expr) syntax.Expr {
	for {
		p, ok := e.(*syntax.ParenExpr)
		if !ok {
			return e
		}
		e = p.X
	}
}

// fallible evaluates a fallible call, returning its value and error registers.
func (g *gen) fallible(x syntax.Expr, t *sema.Type) (reg, bool, reg) {
	call := unparen(x).(*syntax.CallExpr)
	errR := g.allocG(call.Sp)
	g.errFor, g.errReg = call, errR
	r, ok := g.expr(call)
	if t == sema.TVoid {
		ok = false
	}
	return r, ok, errR
}

// tryExpr: on a non-zero error code, run every pending defer and return the
// same code from the enclosing function.
func (g *gen) tryExpr(e *syntax.TryExpr, t *sema.Type) (reg, bool) {
	r, ok, errR := g.fallible(e.X, t)
	okL := g.newLabel("tryok")
	g.emit("cbz %s, %s", x(errR), okL)
	g.runDefers(0)
	g.emit("mov x1, %s", x(errR))
	g.emit("b %s", g.retLabel)
	g.label(okL)
	g.free(errR)
	return r, ok
}

func (g *gen) catchExpr(e *syntax.CatchExpr, t *sema.Type) (reg, bool) {
	r, ok, errR := g.fallible(e.X, t)
	okL := g.newLabel("catchok")
	g.emit("cbz %s, %s", x(errR), okL)
	if lv := g.info.Defs[e]; lv != nil {
		if pr, isP := g.promoted[lv]; isP {
			g.writePromoted(pr, lv.Type, errR)
		} else {
			g.store(errR, g.mem(g.slots[lv]), lv.Type)
		}
	}
	g.free(errR)
	if e.Handler != nil {
		g.block(e.Handler) // always leaves
	} else {
		fb, _ := g.expr(e.Fallback)
		if r.fp {
			g.emit("mov %s.16b, %s.16b", v(r), v(fb))
		} else {
			g.emit("mov %s, %s", x(r), x(fb))
		}
		g.free(fb)
	}
	g.label(okL)
	return r, ok
}

// inlineCtx describes the inline fn body being expanded.
type inlineCtx struct {
	end  string             // label after the expanded body
	res  reg                // result register (zero reg for void)
	last *syntax.ReturnStmt // trailing return of the body: falls through to end
}

func (g *gen) hasInlineCall(e syntax.Expr) bool {
	found := false
	syntax.Inspect(e, func(n any) bool {
		if c, ok := n.(*syntax.CallExpr); ok && g.inlineBody(c) != nil {
			found = true
		}
		return !found
	})
	return found
}

func (g *gen) hasDefers() bool {
	for _, d := range g.defers {
		if len(d) > 0 {
			return true
		}
	}
	return false
}

// inlineCall expands an inline fn at the call site: arguments are evaluated
// left to right, then bound to the callee's parameters (which live in this
// frame, usually in registers), and every return jumps past the body.
func (g *gen) inlineCall(e *syntax.CallExpr, cb *sema.Body, ret *sema.Type) (reg, bool) {
	// Arguments up to the last one that expands an inline fn go through
	// temporaries (that expansion may reuse these parameters); the rest are
	// computed straight into the parameters.
	k := -1
	for i, a := range e.Args {
		if g.hasInlineCall(a) {
			k = i
		}
	}
	vals := make([]reg, len(e.Args))
	for i := 0; i <= k; i++ {
		vals[i], _ = g.expr(e.Args[i])
	}
	for i, p := range cb.Params {
		if i > k {
			g.initVar(p, e.Args[i])
			continue
		}
		if pr, ok := g.promoted[p]; ok {
			g.writePromoted(pr, p.Type, vals[i])
		} else {
			g.store(vals[i], g.mem(g.slots[p]), p.Type)
		}
		g.free(vals[i])
	}
	var res reg
	if ret != sema.TVoid {
		res = g.alloc(ret, e.Sp)
	}
	saved, savedDefers, savedLoops := g.inl, g.defers, g.loops
	g.inl = &inlineCtx{end: g.newLabel("inl"), res: res}
	if n := len(cb.Block.Stmts); n > 0 {
		g.inl.last, _ = cb.Block.Stmts[n-1].(*syntax.ReturnStmt)
	}
	g.defers, g.loops = nil, nil
	g.block(cb.Block)
	g.label(g.inl.end)
	g.inl, g.defers, g.loops = saved, savedDefers, savedLoops
	return res, ret != sema.TVoid
}

// selectCond sets the flags for a @select condition and returns its condition code.
func (g *gen) selectCond(c syntax.Expr) string {
	if b, ok := unparen(c).(*syntax.BinaryExpr); ok && isCmp(b.Op) {
		return g.compare(b)
	}
	cr := g.use(c)
	g.emit("cmp %s, #0", w(cr))
	g.free(cr)
	return "ne"
}
