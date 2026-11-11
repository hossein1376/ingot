package codegen

import (
	"fmt"

	"github.com/hossein1376/ingot/internal/sema"
	"github.com/hossein1376/ingot/internal/syntax"
)

func (g *gen) block(b *syntax.Block) {
	g.defers = append(g.defers, nil)
	for _, s := range b.Stmts {
		g.stmt(s)
	}
	if n := len(b.Stmts); n == 0 || !exits(b.Stmts[n-1]) {
		g.runDefers(len(g.defers) - 1)
	}
	g.defers = g.defers[:len(g.defers)-1]
}

// exits reports whether s unconditionally leaves the block by a jump (its
// own defers were already emitted at the jump).
func exits(s syntax.Stmt) bool {
	switch s.(type) {
	case *syntax.ReturnStmt, *syntax.BreakStmt, *syntax.ContinueStmt:
		return true
	}
	return false
}

// runDefers emits the deferred bodies of every open block at depth >= from,
// innermost first, each block's defers in reverse order.
func (g *gen) runDefers(from int) {
	saved := g.defers
	for d := len(saved) - 1; d >= from; d-- {
		list := saved[d]
		for i := len(list) - 1; i >= 0; i-- {
			// a defer body sees only the scopes outside it; copy so its own
			// block scope does not overwrite saved entries
			g.defers = append([][]*syntax.Block(nil), saved[:d]...)
			g.block(list[i])
		}
	}
	g.defers = saved
}

func (g *gen) typeOf(e syntax.Expr) *sema.Type { return g.info.Types[e] }

func (g *gen) stmt(s syntax.Stmt) {
	switch s := s.(type) {
	case *syntax.Block:
		g.block(s)
	case *syntax.LetStmt:
		lv := g.info.Defs[s]
		g.initVar(lv, s.Value)
	case *syntax.AssignStmt:
		g.assign(s)
	case *syntax.DeferStmt:
		top := len(g.defers) - 1
		g.defers[top] = append(g.defers[top], s.Body)
	case *syntax.ExprStmt:
		r, ok := g.expr(s.X)
		if ok {
			g.free(r)
		}
	case *syntax.IfStmt:
		if val, known := g.constCond(s.Cond); known {
			// compile-time condition (e.g. @is_macos()): the untaken branch is not compiled
			if val {
				g.block(s.Then)
			} else if s.Else != nil {
				g.stmt(s.Else)
			}
			return
		}
		end := g.newLabel("endif")
		els := end
		if s.Else != nil {
			els = g.newLabel("else")
		}
		g.jumpIf(s.Cond, els, false)
		g.block(s.Then)
		if s.Else != nil {
			g.emit("b %s", end)
			g.label(els)
			g.stmt(s.Else)
		}
		g.label(end)
	case *syntax.WhileStmt:
		top, cont, end := g.newLabel("while"), g.newLabel("wcond"), g.newLabel("wend")
		// rotated loop: test at the bottom, one branch per iteration
		g.emit("b %s", cont)
		g.label(top)
		g.loops = append(g.loops, loopLabels{brk: end, cont: cont, depth: len(g.defers)})
		g.block(s.Body)
		g.loops = g.loops[:len(g.loops)-1]
		g.label(cont)
		g.jumpIf(s.Cond, top, true)
		g.label(end)
	case *syntax.ForStmt:
		lv := g.info.Defs[s]
		t := lv.Type
		pr, promoted := g.promoted[lv]
		if promoted {
			g.initVar(lv, s.Lo)
		} else {
			lo, _ := g.expr(s.Lo)
			g.store(lo, g.mem(g.slots[lv]), t)
			g.free(lo)
		}
		// the bound is evaluated once; it stays in a callee-saved register
		// when the counter is promoted, otherwise in a frame slot
		hiSlot := g.slot(8, 8)
		hi, _ := g.expr(s.Hi)
		if !promoted {
			g.emit("str %s, %s", x(hi), g.mem(hiSlot))
			g.free(hi)
		}
		top, cont, test, end := g.newLabel("for"), g.newLabel("fnext"), g.newLabel("ftest"), g.newLabel("fend")
		g.emit("b %s", test)
		g.label(top)
		g.loops = append(g.loops, loopLabels{brk: end, cont: cont, depth: len(g.defers)})
		g.block(s.Body)
		g.loops = g.loops[:len(g.loops)-1]
		g.label(cont)
		ctr := fmt.Sprintf("x%d", pr.n)
		if promoted {
			g.emit("add %s, %s, #1", ctr, ctr)
		} else {
			ctr = "x9"
			g.emit("ldr x9, %s", g.mem(g.slots[lv]))
			g.emit("add x9, x9, #1")
			g.emit("str x9, %s", g.mem(g.slots[lv]))
		}
		g.label(test)
		if !promoted {
			g.emit("ldr x9, %s", g.mem(g.slots[lv]))
		}
		if promoted {
			g.emit("cmp %s, %s", ctr, x(hi))
			g.free(hi)
		} else {
			g.emit("ldr x10, %s", g.mem(hiSlot))
			g.emit("cmp %s, x10", ctr)
		}
		if t.Signed {
			g.emit("b.lt %s", top)
		} else {
			g.emit("b.lo %s", top)
		}
		g.label(end)
	case *syntax.ReturnStmt:
		if g.inl != nil {
			res := g.inl.res
			switch {
			case s.Value == nil:
				g.runDefers(0)
			case !g.hasDefers() && g.intoReg(res, g.typeOf(s.Value), s.Value):
			default:
				r, _ := g.expr(s.Value)
				g.runDefers(0)
				if r.fp {
					g.emit("mov %s.16b, %s.16b", v(res), v(r))
				} else {
					g.emit("mov %s, %s", x(res), x(r))
				}
				g.free(r)
			}
			if s != g.inl.last || len(g.loops) != 0 {
				g.emit("b %s", g.inl.end)
			}
			return
		}
		if b, ok := s.Value.(*syntax.BuiltinCall); ok && b.Name == "err" {
			code, _ := g.expr(b.Args[0])
			g.runDefers(0)
			g.emit("mov x1, %s", x(code))
			g.free(code)
			g.emit("b %s", g.retLabel)
			return
		}
		if s.Value != nil {
			t := g.typeOf(s.Value)
			r, _ := g.expr(s.Value)
			if t.IsAggregate() {
				g.emit("ldr x14, %s", g.mem(g.retSlot))
				g.copyMem("x14", x(r), t.Size)
			}
			// the value is computed before deferred code runs; it stays in
			// its temporary (preserved across calls) until the jump
			g.runDefers(0)
			switch {
			case t.IsAggregate():
			case r.fp:
				if t.Kind == sema.Vec {
					g.emit("mov v0.16b, %s.16b", v(r))
				} else {
					g.emit("fmov %s, %s", retF(t), rn(r, t))
				}
			default:
				g.emit("mov x0, %s", x(r))
			}
			g.free(r)
		} else {
			g.runDefers(0)
		}
		if g.body.Err != nil {
			g.emit("mov x1, #0")
		}
		g.emit("b %s", g.retLabel)
	case *syntax.SwitchStmt:
		r, _ := g.expr(s.X)
		end := g.newLabel("swend")
		labels := make([]string, len(s.Cases))
		for i, cs := range s.Cases {
			labels[i] = g.newLabel("case")
			for _, v := range cs.Values {
				k := g.info.Consts[v].I
				if k >= 0 && k < 4096 {
					g.emit("cmp %s, #%d", x(r), k)
				} else {
					g.movImm("x16", uint64(k))
					g.emit("cmp %s, x16", x(r))
				}
				g.emit("b.eq %s", labels[i])
			}
		}
		g.free(r)
		els := end
		if s.Else != nil {
			els = g.newLabel("swelse")
		}
		g.emit("b %s", els)
		for i, cs := range s.Cases {
			g.label(labels[i])
			g.block(cs.Body)
			g.emit("b %s", end)
		}
		if s.Else != nil {
			g.label(els)
			g.block(s.Else)
		}
		g.label(end)
	case *syntax.BreakStmt:
		l := g.loops[len(g.loops)-1]
		g.runDefers(l.depth)
		g.emit("b %s", l.brk)
	case *syntax.ContinueStmt:
		l := g.loops[len(g.loops)-1]
		g.runDefers(l.depth)
		g.emit("b %s", l.cont)
	}
}

// constCond evaluates conditions known at compile time for the current target.
func (g *gen) constCond(e syntax.Expr) (bool, bool) {
	if k, ok := g.info.Consts[e]; ok && g.typeOf(e) == sema.TBool {
		return k.I != 0, true
	}
	switch e := e.(type) {
	case *syntax.ParenExpr:
		return g.constCond(e.X)
	case *syntax.BuiltinCall:
		if e.Name == "is_macos" {
			return g.t.Darwin, true
		}
	case *syntax.UnaryExpr:
		if e.Op == "!" {
			v, ok := g.constCond(e.X)
			return !v, ok
		}
	}
	return false, false
}

func retF(t *sema.Type) string {
	if t.Size == 4 {
		return "s0"
	}
	return "d0"
}

// initVar stores the initial value of a local.
func (g *gen) initVar(lv *sema.Var, val syntax.Expr) {
	t := lv.Type
	off := g.slots[lv]
	if t.IsAggregate() {
		if b, ok := val.(*syntax.BuiltinCall); ok && b.Name == "zero" {
			g.subImm("x14", "x29", off)
			g.zeroMem("x14", t.Size)
			return
		}
		r, _ := g.expr(val)
		g.subImm("x14", "x29", off)
		g.copyMem("x14", x(r), t.Size)
		g.free(r)
		return
	}
	if pr, ok := g.promoted[lv]; ok && !pr.fp {
		if k, isConst := g.info.Consts[val]; isConst && t.Kind == sema.Int || isConst && t == sema.TBool {
			g.movImm(fmt.Sprintf("x%d", pr.n), uint64(k.I))
			return
		}
	}
	if pr, ok := g.promoted[lv]; ok && g.arithInto(pr, t, val) {
		return
	}
	r := g.use(val)
	if pr, ok := g.promoted[lv]; ok {
		g.writePromoted(pr, t, r)
	} else {
		g.store(r, g.mem(off), t)
	}
	g.free(r)
}

// arithInto computes an arithmetic expression straight into a promoted
// variable's register (the destination is written by the last instruction).
func (g *gen) arithInto(pr preg, t *sema.Type, val syntax.Expr) bool {
	if bc, ok := val.(*syntax.BuiltinCall); ok {
		switch {
		case bc.Name == "zero" && pr.fp:
			g.emit("movi v%d.16b, #0", pr.n)
			return true
		case bc.Name == "zero":
			g.emit("mov x%d, #0", pr.n)
			return true
		case bc.Name == "select" && !pr.fp:
			a := g.use(bc.Args[1])
			b := g.use(bc.Args[2])
			cc := g.selectCond(bc.Args[0])
			g.emit("csel x%d, %s, %s, %s", pr.n, x(a), x(b), cc)
			g.free(a)
			g.free(b)
			return true
		case bc.Name == "fma" && pr.fp:
			// acc = @fma(a, b, acc) accumulates in place
			if _, cp, ok := g.promotedTarget(bc.Args[2]); ok && cp == pr {
				a := g.use(bc.Args[0])
				b := g.use(bc.Args[1])
				if t.Kind == sema.Vec {
					g.emit("fmla v%d.%s, %s.%s, %s.%s", pr.n, arr(t), v(a), arr(t), v(b), arr(t))
				} else {
					g.emit("fmadd %s, %s, %s, %s", pname(pr, t), rn(a, t), rn(b, t), pname(pr, t))
				}
				g.free(a)
				g.free(b)
				return true
			}
		}
		return false
	}
	b, ok := val.(*syntax.BinaryExpr)
	if !ok || !opIsArith(b.Op) || t.Kind == sema.Vec || g.info.Consts[val] != (sema.Const{}) {
		return false
	}
	if _, isConst := g.info.Consts[val]; isConst {
		return false
	}
	bx, by := g.operands(b, t)
	a := g.use(bx)
	g.arithExpr3(b.Op, reg{abs: pr.n, fp: pr.fp}, a, by, t, b.Sp)
	g.free(a)
	return true
}

func opIsArith(op string) bool {
	switch op {
	case "+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>":
		return true
	}
	return false
}

// promotedTarget returns the promoted variable an assignment targets, if any.
func (g *gen) promotedTarget(e syntax.Expr) (*sema.Var, preg, bool) {
	for {
		p, ok := e.(*syntax.ParenExpr)
		if !ok {
			break
		}
		e = p.X
	}
	id, ok := e.(*syntax.IdentExpr)
	if !ok {
		return nil, preg{}, false
	}
	lv, ok := g.info.Uses[id].(*sema.Var)
	if !ok {
		return nil, preg{}, false
	}
	pr, ok := g.promoted[lv]
	return lv, pr, ok
}

func (g *gen) assign(s *syntax.AssignStmt) {
	t := g.typeOf(s.Target)
	if _, pr, ok := g.promotedTarget(s.Target); ok {
		if s.Op == "=" {
			if g.arithInto(pr, t, s.Value) {
				return
			}
			r := g.use(s.Value)
			if r.abs != pr.n || r.fp != pr.fp {
				g.writePromoted(pr, t, r)
			}
			g.free(r)
			return
		}
		// x op= y updates the promoted register in place
		self := reg{abs: pr.n, fp: pr.fp}
		g.arithExpr3(s.Op[:len(s.Op)-1], self, self, s.Value, t, s.Sp)
		return
	}
	addr := g.addr(s.Target)
	if t.IsAggregate() {
		if b, ok := s.Value.(*syntax.BuiltinCall); ok && b.Name == "zero" {
			g.zeroMem(x(addr), t.Size)
			g.free(addr)
			return
		}
		r, _ := g.expr(s.Value)
		g.copyMem(x(addr), x(r), t.Size)
		g.free(r)
		g.free(addr)
		return
	}
	if s.Op == "=" {
		r := g.use(s.Value)
		g.store(r, "["+x(addr)+"]", t)
		g.free(r)
		g.free(addr)
		return
	}
	cur := g.alloc(t, s.Sp)
	g.load(cur, "["+x(addr)+"]", t)
	g.arithExpr(s.Op[:len(s.Op)-1], cur, s.Value, t, s.Sp)
	g.store(cur, "["+x(addr)+"]", t)
	g.free(cur)
	g.free(addr)
}

// intoReg computes an integer arithmetic expression or a constant straight
// into d. It reports false (emitting nothing) for other expressions.
func (g *gen) intoReg(d reg, t *sema.Type, val syntax.Expr) bool {
	if d.fp || t.Kind != sema.Int && t != sema.TBool {
		return false
	}
	if k, ok := g.info.Consts[val]; ok && !k.IsFloat {
		g.movImm(x(d), uint64(k.I))
		return true
	}
	if _, pr, ok := g.promotedTarget(val); ok {
		g.emit("mov %s, x%d", x(d), pr.n)
		return true
	}
	b, ok := unparen(val).(*syntax.BinaryExpr)
	if !ok || !opIsArith(b.Op) {
		return false
	}
	bx, by := g.operands(b, t)
	a := g.use(bx)
	g.arithExpr3(b.Op, d, a, by, t, b.Sp)
	g.free(a)
	return true
}
