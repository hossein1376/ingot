package driver

import (
	"reflect"
	"strconv"
	"strings"

	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/syntax"
)

// Generics are explicit templates. `fn f[T](...)` and `struct S[T] {...}`
// are not checked on their own; each use with concrete type arguments,
// f.[i64](x) or S.[f64], creates one copy named "f[i64]" in which T is
// replaced by i64, and that copy is checked and compiled like any other
// declaration. There is no inference and there are no bounds: a missing
// operation is reported inside the instantiated body.

const maxInstDepth = 32

type mono struct {
	dl      *diag.List
	fns     map[string]*syntax.FnDecl
	structs map[string]*syntax.StructDecl
	done    map[string]bool
	out     *syntax.File
	depth   int
}

// Monomorphize replaces every generic instantiation in f with a concrete
// copy and drops the generic templates themselves.
func Monomorphize(f *syntax.File, dl *diag.List) {
	m := &mono{dl: dl, fns: map[string]*syntax.FnDecl{}, structs: map[string]*syntax.StructDecl{}, done: map[string]bool{}, out: f}
	var fns []*syntax.FnDecl
	for _, fn := range f.Fns {
		if len(fn.TypeParams) == 0 {
			fns = append(fns, fn)
			continue
		}
		if fn.Extern || fn.Pub {
			dl.Errorf("E0263", fn.NameSp, "generic function %q cannot be extern or pub", fn.Name).Note("each instantiation gets its own mangled symbol; export a non-generic wrapper instead")
		}
		m.fns[fn.Name] = fn
	}
	var structs []*syntax.StructDecl
	for _, s := range f.Structs {
		if len(s.TypeParams) == 0 {
			structs = append(structs, s)
			continue
		}
		m.structs[s.Name] = s
	}
	if len(m.fns) == 0 && len(m.structs) == 0 && !hasInst(f) {
		return
	}
	f.Fns, f.Structs = fns, structs
	n := len(f.Fns)
	for i := 0; i < n; i++ {
		m.rewrite(f.Fns[i])
	}
	for _, s := range structs {
		m.rewrite(s)
	}
	for _, g := range f.Globals {
		m.rewrite(g)
	}
	for _, t := range f.Tests {
		m.rewrite(t)
	}
}

func hasInst(f *syntax.File) bool {
	found := false
	visit(reflect.ValueOf(f), func(v reflect.Value) {
		switch n := v.Interface().(type) {
		case *syntax.InstExpr:
			found = true
		case *syntax.NamedType:
			if len(n.Args) > 0 {
				found = true
			}
		}
	})
	return found
}

// visit calls f on every non-nil pointer reachable from v (post-order).
func visit(v reflect.Value, f func(reflect.Value)) {
	switch v.Kind() {
	case reflect.Interface:
		if !v.IsNil() {
			visit(v.Elem(), f)
		}
	case reflect.Pointer:
		if v.IsNil() {
			return
		}
		visit(v.Elem(), f)
		f(v)
	case reflect.Struct:
		if v.Type() == reflect.TypeOf(diag.Span{}) {
			return
		}
		for i := 0; i < v.NumField(); i++ {
			visit(v.Field(i), f)
		}
	case reflect.Slice:
		for i := 0; i < v.Len(); i++ {
			visit(v.Index(i), f)
		}
	}
}

// rewrite replaces, in place, every InstExpr below n with an IdentExpr
// naming its instance, and every NamedType with type arguments with the
// instance struct's name.
func (m *mono) rewrite(n any) {
	m.rewriteValue(reflect.ValueOf(n))
}

func (m *mono) rewriteValue(v reflect.Value) {
	if v.Kind() == reflect.Pointer && !v.IsNil() {
		if c, ok := v.Interface().(*syntax.CallExpr); ok {
			if id, ok := c.Fn.(*syntax.IdentExpr); ok {
				if fn := m.fns[id.Name]; fn != nil {
					m.dl.Errorf("E0262", id.Sp, "generic function %q needs explicit type arguments", id.Name).
						Note("write %s.[%s](...); Ingot does not infer type arguments", id.Name, strings.Join(fn.TypeParams, ", "))
				}
			}
		}
	}
	switch v.Kind() {
	case reflect.Interface:
		if v.IsNil() {
			return
		}
		if in, ok := v.Interface().(*syntax.InstExpr); ok {
			for i := range in.Types {
				m.rewriteType(in.Types[i])
			}
			if name, ok := m.instFn(in); ok && v.CanSet() {
				v.Set(reflect.ValueOf(&syntax.IdentExpr{Name: name, Sp: in.Sp}))
			}
			return
		}
		m.rewriteValue(v.Elem())
	case reflect.Pointer:
		if v.IsNil() {
			return
		}
		if nt, ok := v.Interface().(*syntax.NamedType); ok {
			m.rewriteType(nt)
			return
		}
		m.rewriteValue(v.Elem())
	case reflect.Struct:
		if v.Type() == reflect.TypeOf(diag.Span{}) {
			return
		}
		for i := 0; i < v.NumField(); i++ {
			m.rewriteValue(v.Field(i))
		}
	case reflect.Slice:
		for i := 0; i < v.Len(); i++ {
			m.rewriteValue(v.Index(i))
		}
	}
}

func (m *mono) rewriteType(t syntax.TypeExpr) {
	if nt, ok := t.(*syntax.NamedType); ok {
		if len(nt.Args) == 0 {
			if _, generic := m.structs[nt.Name]; generic {
				m.dl.Errorf("E0262", nt.Sp, "generic struct %q needs type arguments: %s.[...]", nt.Name, nt.Name)
			}
			return
		}
		for _, a := range nt.Args {
			m.rewriteType(a)
		}
		nt.Name = m.instStruct(nt)
		nt.Args = nil
		return
	}
	m.rewriteValue(reflect.ValueOf(t))
}

func (m *mono) instFn(in *syntax.InstExpr) (string, bool) {
	id, ok := in.X.(*syntax.IdentExpr)
	var fn *syntax.FnDecl
	if ok {
		fn = m.fns[id.Name]
		if _, isStruct := m.structs[id.Name]; isStruct && fn == nil {
			// a generic struct or union named in an expression: Option.[i64].some(x)
			return m.instStruct(&syntax.NamedType{Name: id.Name, Args: in.Types, Sp: in.Sp}), true
		}
	}
	if fn == nil {
		m.dl.Errorf("E0260", in.Sp, "type arguments given to something that is not a generic function")
		return "", false
	}
	if len(in.Types) != len(fn.TypeParams) {
		m.dl.Errorf("E0261", in.Sp, "%q takes %d type argument(s), got %d", fn.Name, len(fn.TypeParams), len(in.Types))
		return "", false
	}
	name := instName(fn.Name, in.Types)
	if m.done[name] {
		return name, true
	}
	if m.depth >= maxInstDepth {
		m.dl.Errorf("E0264", in.Sp, "generic instantiation nested more than %d deep (recursive instantiation?)", maxInstDepth)
		return "", false
	}
	m.done[name] = true
	m.out.Insts = append(m.out.Insts, syntax.Instance{Name: name, Template: fn.Sp, At: in.Sp})
	c := clone(fn, subst(fn.TypeParams, in.Types)).(*syntax.FnDecl)
	c.Name, c.TypeParams = name, nil
	m.out.Fns = append(m.out.Fns, c)
	m.depth++
	m.rewrite(c)
	m.depth--
	return name, true
}

func (m *mono) instStruct(nt *syntax.NamedType) string {
	sd := m.structs[nt.Name]
	if sd == nil {
		m.dl.Errorf("E0260", nt.Sp, "type arguments given to %q, which is not a generic struct", nt.Name)
		return nt.Name
	}
	if len(nt.Args) != len(sd.TypeParams) {
		m.dl.Errorf("E0261", nt.Sp, "%q takes %d type argument(s), got %d", sd.Name, len(sd.TypeParams), len(nt.Args))
		return nt.Name
	}
	name := instName(sd.Name, nt.Args)
	if m.done[name] {
		return name
	}
	if m.depth >= maxInstDepth {
		m.dl.Errorf("E0264", nt.Sp, "generic instantiation nested more than %d deep (recursive instantiation?)", maxInstDepth)
		return name
	}
	m.done[name] = true
	m.out.Insts = append(m.out.Insts, syntax.Instance{Name: name, Template: sd.Sp, At: nt.Sp})
	c := clone(sd, subst(sd.TypeParams, nt.Args)).(*syntax.StructDecl)
	c.Name, c.TypeParams = name, nil
	m.out.Structs = append(m.out.Structs, c)
	m.depth++
	m.rewrite(c)
	m.depth--
	return name
}

func subst(params []string, args []syntax.TypeExpr) map[string]syntax.TypeExpr {
	s := map[string]syntax.TypeExpr{}
	for i, p := range params {
		s[p] = args[i]
	}
	return s
}

// instName renders "name[T1,T2]" with canonical type spellings.
func instName(name string, args []syntax.TypeExpr) string {
	parts := make([]string, len(args))
	for i, a := range args {
		parts[i] = typeKey(a)
	}
	return name + "[" + strings.Join(parts, ",") + "]"
}

func typeKey(t syntax.TypeExpr) string {
	switch t := t.(type) {
	case *syntax.NamedType:
		if len(t.Args) > 0 {
			return instName(t.Name, t.Args)
		}
		return t.Name
	case *syntax.PtrType:
		return "*" + typeKey(t.Elem)
	case *syntax.SliceType:
		return "[]" + typeKey(t.Elem)
	case *syntax.ArrayType:
		return "[" + strconv.FormatInt(t.Len, 10) + "]" + typeKey(t.Elem)
	case *syntax.FnType:
		ps := make([]string, len(t.Params))
		for i, p := range t.Params {
			ps[i] = typeKey(p)
		}
		s := "fn(" + strings.Join(ps, ",")
		if t.Variadic {
			s += ",..."
		}
		s += ")"
		if t.Ret != nil {
			s += "->" + typeKey(t.Ret)
		}
		return s
	}
	return "?"
}

// clone deep-copies an AST node, replacing named types listed in sub.
func clone(n any, sub map[string]syntax.TypeExpr) any {
	return cloneValue(reflect.ValueOf(n), sub).Interface()
}

func cloneValue(v reflect.Value, sub map[string]syntax.TypeExpr) reflect.Value {
	switch v.Kind() {
	case reflect.Interface:
		if v.IsNil() {
			return v
		}
		c := cloneValue(v.Elem(), sub)
		out := reflect.New(v.Type()).Elem()
		out.Set(c)
		return out
	case reflect.Pointer:
		if v.IsNil() {
			return v
		}
		if nt, ok := v.Interface().(*syntax.NamedType); ok && len(nt.Args) == 0 {
			if r, ok := sub[nt.Name]; ok {
				return cloneValue(reflect.ValueOf(r), nil)
			}
		}
		out := reflect.New(v.Elem().Type())
		out.Elem().Set(cloneValue(v.Elem(), sub))
		return out
	case reflect.Struct:
		out := reflect.New(v.Type()).Elem()
		out.Set(v)
		for i := 0; i < v.NumField(); i++ {
			if out.Field(i).CanSet() {
				out.Field(i).Set(cloneValue(v.Field(i), sub))
			}
		}
		return out
	case reflect.Slice:
		if v.IsNil() {
			return v
		}
		out := reflect.MakeSlice(v.Type(), v.Len(), v.Len())
		for i := 0; i < v.Len(); i++ {
			out.Index(i).Set(cloneValue(v.Index(i), sub))
		}
		return out
	}
	return v
}

// AnnotateInstances adds "in f[T], instantiated at ..." notes to errors
// that sema reported inside generic templates.
func AnnotateInstances(f *syntax.File, dl *diag.List) {
	if len(f.Insts) == 0 {
		return
	}
	for _, d := range dl.Items {
		n := 0
		for _, in := range f.Insts {
			if !within(d.Span, in.Template) {
				continue
			}
			if n == 3 {
				d.Note("(and other instances)")
				break
			}
			d.Note("in %s, instantiated at %s:%d:%d", in.Name, in.At.File, in.At.Start.Line, in.At.Start.Col)
			n++
		}
	}
}

func within(s, outer diag.Span) bool {
	before := func(a, b diag.Pos) bool { return a.Line < b.Line || a.Line == b.Line && a.Col <= b.Col }
	return s.File == outer.File && before(outer.Start, s.Start) && before(s.End, outer.End)
}
