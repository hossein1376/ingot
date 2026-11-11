package driver

import (
	"strings"

	"github.com/hossein1376/ingot/internal/syntax"
)

// renamer qualifies the names of one file so modules imported with
// `import "p" as m;` live in their own namespace. Every non-extern
// top-level name D of a module with prefix P becomes "P.D" (so its own
// unqualified references are rewritten too, unless a local shadows them),
// and qualified references "alias.D" in any file are rewritten to the
// module's unique prefix. Codegen turns '.' into "__" in symbols.
type renamer struct {
	prefix  string            // "" for the root and merged imports
	own     map[string]bool   // this file's top-level names (when prefixed)
	aliases map[string]string // alias -> module prefix
	scopes  []map[string]bool // local bindings, innermost last
}

func (r *renamer) local(n string) bool {
	for i := len(r.scopes) - 1; i >= 0; i-- {
		if r.scopes[i][n] {
			return true
		}
	}
	return false
}

func (r *renamer) bind(n string) { r.scopes[len(r.scopes)-1][n] = true }
func (r *renamer) push()         { r.scopes = append(r.scopes, map[string]bool{}) }
func (r *renamer) pop()          { r.scopes = r.scopes[:len(r.scopes)-1] }

func (r *renamer) name(n string, isLocalable bool) string {
	if head, rest, ok := strings.Cut(n, "."); ok {
		if p, ok := r.aliases[head]; ok {
			return p + "." + rest
		}
		return n
	}
	if r.prefix != "" && r.own[n] && !(isLocalable && r.local(n)) {
		return r.prefix + "." + n
	}
	return n
}

func (r *renamer) file(f *syntax.File) {
	if r.prefix != "" {
		for _, fn := range f.Fns {
			if !fn.Extern {
				r.own[fn.Name] = true
			}
		}
		for _, s := range f.Structs {
			r.own[s.Name] = true
		}
		for _, e := range f.Enums {
			r.own[e.Name] = true
		}
		for _, g := range f.Globals {
			r.own[g.Name] = true
		}
	}
	r.push()
	for _, s := range f.Structs {
		s.Name = r.name(s.Name, false)
		for i := range s.Fields {
			s.Fields[i].Type = r.typ(s.Fields[i].Type)
		}
	}
	for _, e := range f.Enums {
		e.Name = r.name(e.Name, false)
		e.Base = r.typ(e.Base)
		for i := range e.Members {
			r.expr(e.Members[i].Value)
		}
	}
	for _, g := range f.Globals {
		g.Name = r.name(g.Name, false)
		g.Type = r.typ(g.Type)
		r.expr(g.Value)
	}
	for _, fn := range f.Fns {
		if !fn.Extern {
			fn.Name = r.name(fn.Name, false)
		}
		r.push()
		for i := range fn.Params {
			fn.Params[i].Type = r.typ(fn.Params[i].Type)
			r.bind(fn.Params[i].Name)
		}
		fn.Ret = r.typ(fn.Ret)
		fn.Err = r.typ(fn.Err)
		if fn.Body != nil {
			r.block(fn.Body)
		}
		r.pop()
	}
	for _, t := range f.Tests {
		r.block(t.Body)
	}
	r.pop()
}

func (r *renamer) typ(t syntax.TypeExpr) syntax.TypeExpr {
	switch t := t.(type) {
	case *syntax.NamedType:
		t.Name = r.name(t.Name, false)
		for i := range t.Args {
			t.Args[i] = r.typ(t.Args[i])
		}
	case *syntax.PtrType:
		t.Elem = r.typ(t.Elem)
	case *syntax.ArrayType:
		t.Elem = r.typ(t.Elem)
	case *syntax.SliceType:
		t.Elem = r.typ(t.Elem)
	case *syntax.FnType:
		for i := range t.Params {
			t.Params[i] = r.typ(t.Params[i])
		}
		t.Ret = r.typ(t.Ret)
		t.Err = r.typ(t.Err)
	}
	return t
}

func (r *renamer) block(b *syntax.Block) {
	r.push()
	for _, s := range b.Stmts {
		r.stmt(s)
	}
	r.pop()
}

func (r *renamer) stmt(s syntax.Stmt) {
	switch s := s.(type) {
	case *syntax.Block:
		r.block(s)
	case *syntax.LetStmt:
		s.Type = r.typ(s.Type)
		r.expr(s.Value)
		r.bind(s.Name)
	case *syntax.AssignStmt:
		r.expr(s.Target)
		r.expr(s.Value)
	case *syntax.ExprStmt:
		r.expr(s.X)
	case *syntax.IfStmt:
		r.expr(s.Cond)
		r.block(s.Then)
		if s.Else != nil {
			r.stmt(s.Else)
		}
	case *syntax.WhileStmt:
		r.expr(s.Cond)
		r.block(s.Body)
	case *syntax.ForStmt:
		r.expr(s.Lo)
		r.expr(s.Hi)
		r.push()
		r.bind(s.Var)
		r.block(s.Body)
		r.pop()
	case *syntax.ReturnStmt:
		r.expr(s.Value)
	case *syntax.DeferStmt:
		r.block(s.Body)
	case *syntax.SwitchStmt:
		r.expr(s.X)
		for _, c := range s.Cases {
			for _, v := range c.Values {
				r.expr(v)
			}
			r.block(c.Body)
		}
		if s.Else != nil {
			r.block(s.Else)
		}
	}
}

func (r *renamer) expr(e syntax.Expr) {
	switch e := e.(type) {
	case *syntax.IdentExpr:
		e.Name = r.name(e.Name, true)
	case *syntax.UnaryExpr:
		r.expr(e.X)
	case *syntax.BinaryExpr:
		r.expr(e.X)
		r.expr(e.Y)
	case *syntax.CallExpr:
		r.expr(e.Fn)
		for _, a := range e.Args {
			r.expr(a)
		}
	case *syntax.BuiltinCall:
		e.Type = r.typ(e.Type)
		for _, a := range e.Args {
			r.expr(a)
		}
	case *syntax.FieldExpr:
		r.expr(e.X)
	case *syntax.DerefExpr:
		r.expr(e.X)
	case *syntax.IndexExpr:
		r.expr(e.X)
		r.expr(e.Index)
	case *syntax.SliceExpr:
		r.expr(e.X)
		r.expr(e.Lo)
		r.expr(e.Hi)
	case *syntax.CastExpr:
		r.expr(e.X)
		e.To = r.typ(e.To)
	case *syntax.CompositeLit:
		e.Type = r.typ(e.Type)
		for _, v := range e.Values {
			r.expr(v)
		}
	case *syntax.ParenExpr:
		r.expr(e.X)
	case *syntax.TryExpr:
		r.expr(e.X)
	case *syntax.InstExpr:
		r.expr(e.X)
		for i := range e.Types {
			e.Types[i] = r.typ(e.Types[i])
		}
	case *syntax.CatchExpr:
		r.expr(e.X)
		if e.Handler != nil {
			r.push()
			r.bind(e.Name)
			r.block(e.Handler)
			r.pop()
		}
		if e.Fallback != nil {
			r.push()
			r.bind(e.Name)
			r.expr(e.Fallback)
			r.pop()
		}
	}
}
