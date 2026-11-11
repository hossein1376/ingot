package driver

import (
	"strconv"
	"strings"

	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/syntax"
	"github.com/hossein1376/ingot/std"
)

// APIEntry describes one top-level declaration for `ingot api`.
type APIEntry struct {
	Kind      string `json:"kind"` // fn | struct | union | enum | const | var
	Name      string `json:"name"`
	Signature string `json:"signature"`
	Doc       string `json:"doc,omitempty"` // the // comment lines right above it
	File      string `json:"file"`
	Line      int    `json:"line"`
}

// StdFiles lists the embedded standard library as "std/NAME.ig" paths.
func StdFiles() []string {
	ents, _ := std.FS.ReadDir(".")
	var out []string
	for _, e := range ents {
		if strings.HasSuffix(e.Name(), ".ig") {
			out = append(out, "std/"+e.Name())
		}
	}
	return out
}

// ReadSource reads path, or an embedded std file for "std/..." paths.
func ReadSource(path string) ([]byte, bool) {
	_, data, ok := resolve("", path)
	return data, ok
}

// API lists the declarations of one file (imports are not followed), in
// source order, with signatures rendered in Ingot syntax.
func API(path string, src []byte, dl *diag.List) []APIEntry {
	f := syntax.Parse(path, src, dl)
	lines := strings.Split(string(src), "\n")
	doc := func(line int) string {
		var out []string
		for l := line - 2; l >= 0; l-- {
			t := strings.TrimSpace(lines[l])
			if !strings.HasPrefix(t, "//") {
				break
			}
			out = append([]string{strings.TrimSpace(strings.TrimPrefix(t, "//"))}, out...)
		}
		return strings.Join(out, "\n")
	}
	var es []APIEntry
	add := func(kind, name, sig string, sp diag.Span) {
		es = append(es, APIEntry{Kind: kind, Name: name, Signature: sig, Doc: doc(sp.Start.Line), File: path, Line: sp.Start.Line})
	}
	for _, s := range f.Structs {
		var fs []string
		for _, fl := range s.Fields {
			if fl.Type == nil {
				fs = append(fs, fl.Name)
			} else {
				fs = append(fs, fl.Name+": "+typeText(fl.Type))
			}
		}
		kind := "struct"
		if s.Union {
			kind = "union"
		}
		add(kind, s.Name, kind+" "+s.Name+tparams(s.TypeParams)+" { "+strings.Join(fs, ", ")+" }", s.Sp)
	}
	for _, e := range f.Enums {
		var ms []string
		for _, m := range e.Members {
			if lit, ok := m.Value.(*syntax.IntLit); ok {
				ms = append(ms, m.Name+" = "+lit.Text)
			} else {
				ms = append(ms, m.Name)
			}
		}
		base := ""
		if e.Base != nil {
			base = ": " + typeText(e.Base)
		}
		add("enum", e.Name, "enum "+e.Name+base+" { "+strings.Join(ms, ", ")+" }", e.Sp)
	}
	for _, g := range f.Globals {
		kind := "var"
		if g.Const {
			kind = "const"
		}
		sig := kind + " " + g.Name + ": " + typeText(g.Type)
		if lit, ok := g.Value.(*syntax.IntLit); ok && g.Const {
			sig += " = " + lit.Text
		}
		add(kind, g.Name, sig, g.Sp)
	}
	for _, fn := range f.Fns {
		var b strings.Builder
		if fn.Pub {
			b.WriteString("pub ")
		}
		if fn.Extern {
			b.WriteString("extern ")
		}
		if fn.Inline {
			b.WriteString("inline ")
		}
		b.WriteString("fn " + fn.Name + tparams(fn.TypeParams) + "(")
		for i, p := range fn.Params {
			if i > 0 {
				b.WriteString(", ")
			}
			b.WriteString(p.Name + ": " + typeText(p.Type))
		}
		if fn.Variadic {
			if len(fn.Params) > 0 {
				b.WriteString(", ")
			}
			b.WriteString("...")
		}
		b.WriteString(")")
		if fn.Ret != nil {
			b.WriteString(" -> " + typeText(fn.Ret))
		}
		if fn.Err != nil {
			if fn.Ret == nil {
				b.WriteString(" -> void")
			}
			b.WriteString(" ! " + typeText(fn.Err))
		}
		add("fn", fn.Name, b.String(), fn.Sp)
	}
	// source order
	for i := 1; i < len(es); i++ {
		for j := i; j > 0 && es[j].Line < es[j-1].Line; j-- {
			es[j], es[j-1] = es[j-1], es[j]
		}
	}
	return es
}

func tparams(ps []string) string {
	if len(ps) == 0 {
		return ""
	}
	return "[" + strings.Join(ps, ", ") + "]"
}

// typeText renders a syntactic type the way it is written in source.
func typeText(t syntax.TypeExpr) string {
	switch t := t.(type) {
	case *syntax.NamedType:
		if len(t.Args) == 0 {
			return t.Name
		}
		as := make([]string, len(t.Args))
		for i, a := range t.Args {
			as[i] = typeText(a)
		}
		return t.Name + ".[" + strings.Join(as, ", ") + "]"
	case *syntax.PtrType:
		return "*" + typeText(t.Elem)
	case *syntax.SliceType:
		return "[]" + typeText(t.Elem)
	case *syntax.ArrayType:
		return "[" + strconv.FormatInt(t.Len, 10) + "]" + typeText(t.Elem)
	case *syntax.FnType:
		ps := make([]string, len(t.Params))
		for i, p := range t.Params {
			ps[i] = typeText(p)
		}
		if t.Variadic {
			ps = append(ps, "...")
		}
		s := "fn(" + strings.Join(ps, ", ") + ")"
		if t.Ret != nil {
			s += " -> " + typeText(t.Ret)
		}
		if t.Err != nil {
			s += " ! " + typeText(t.Err)
		}
		return s
	}
	return "?"
}
