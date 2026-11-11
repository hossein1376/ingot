package codegen

import (
	"github.com/hossein1376/ingot/internal/sema"
	"github.com/hossein1376/ingot/internal/syntax"
)

func (g *gen) block(b *syntax.Block) {
	for _, s := range b.Stmts {
		g.stmt(s)
	}
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
	case *syntax.ExprStmt:
		r, ok := g.expr(s.X)
		if ok {
			g.free(r)
		}
	case *syntax.IfStmt:
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
		g.loops = append(g.loops, loopLabels{brk: end, cont: cont})
		g.block(s.Body)
		g.loops = g.loops[:len(g.loops)-1]
		g.label(cont)
		g.jumpIf(s.Cond, top, true)
		g.label(end)
	case *syntax.ForStmt:
		lv := g.info.Defs[s]
		t := lv.Type
		lo, _ := g.expr(s.Lo)
		g.store(lo, g.mem(g.slots[lv]), t)
		g.free(lo)
		hiSlot := g.slot(8, 8)
		hi, _ := g.expr(s.Hi)
		g.emit("str %s, %s", x(hi), g.mem(hiSlot))
		g.free(hi)
		top, cont, test, end := g.newLabel("for"), g.newLabel("fnext"), g.newLabel("ftest"), g.newLabel("fend")
		g.emit("b %s", test)
		g.label(top)
		g.loops = append(g.loops, loopLabels{brk: end, cont: cont})
		g.block(s.Body)
		g.loops = g.loops[:len(g.loops)-1]
		g.label(cont)
		g.emit("ldr x9, %s", g.mem(g.slots[lv]))
		g.emit("add x9, x9, #1")
		g.emit("str x9, %s", g.mem(g.slots[lv]))
		g.label(test)
		g.emit("ldr x9, %s", g.mem(g.slots[lv]))
		g.emit("ldr x10, %s", g.mem(hiSlot))
		g.emit("cmp x9, x10")
		if t.Signed {
			g.emit("b.lt %s", top)
		} else {
			g.emit("b.lo %s", top)
		}
		g.label(end)
	case *syntax.ReturnStmt:
		if s.Value != nil {
			t := g.typeOf(s.Value)
			r, _ := g.expr(s.Value)
			switch {
			case t.IsAggregate():
				g.emit("ldr x9, %s", g.mem(g.retSlot))
				g.copyMem("x9", x(r), t.Size)
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
		}
		g.emit("b %s", g.retLabel)
	case *syntax.BreakStmt:
		g.emit("b %s", g.loops[len(g.loops)-1].brk)
	case *syntax.ContinueStmt:
		g.emit("b %s", g.loops[len(g.loops)-1].cont)
	}
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
		g.subImm("x14", "x29", off)
		if b, ok := val.(*syntax.BuiltinCall); ok && b.Name == "zero" {
			g.zeroMem("x14", t.Size)
			return
		}
		r, _ := g.expr(val)
		g.subImm("x14", "x29", off)
		g.copyMem("x14", x(r), t.Size)
		g.free(r)
		return
	}
	r, _ := g.expr(val)
	g.store(r, g.mem(off), t)
	g.free(r)
}

func (g *gen) assign(s *syntax.AssignStmt) {
	t := g.typeOf(s.Target)
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
		r, _ := g.expr(s.Value)
		g.store(r, "["+x(addr)+"]", t)
		g.free(r)
		g.free(addr)
		return
	}
	cur := g.alloc(t, s.Sp)
	g.load(cur, "["+x(addr)+"]", t)
	rhs, _ := g.expr(s.Value)
	g.arith(s.Op[:len(s.Op)-1], cur, rhs, t, s.Sp)
	g.free(rhs)
	g.store(cur, "["+x(addr)+"]", t)
	g.free(cur)
	g.free(addr)
}
