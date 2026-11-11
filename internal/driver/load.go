package driver

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/syntax"
	"github.com/hossein1376/ingot/std"
)

// Load parses path and everything it imports (transitively) into one
// merged file. Imports resolve relative to the importing file; paths that
// start with "std/" come from the embedded standard library. Test blocks
// are kept only from the root file.
//
// `import "p";` merges p's names into one global namespace. `import "p" as
// m;` gives p its own namespace: its names are reached as m.name, and each
// such module file is loaded once under a unique prefix (the first alias
// it was imported with, numbered on a clash).
func Load(path string, src []byte, dl *diag.List) *syntax.File {
	type unit struct {
		f      *syntax.File
		prefix string
	}
	root := syntax.Parse(path, src, dl)
	seen := map[string]bool{"|" + key(path): true}
	prefixOf := map[string]string{} // module key -> prefix
	usedPrefix := map[string]bool{}
	queue := []unit{{f: root}}
	merged := &syntax.File{Name: path, Tests: root.Tests}
	for len(queue) > 0 {
		u := queue[0]
		queue = queue[1:]
		f := u.f
		r := &renamer{prefix: u.prefix, own: map[string]bool{}, aliases: map[string]string{}}
		for _, imp := range f.Imports {
			ipath, data, ok := resolve(f.Name, imp.Path)
			if !ok {
				dl.Errorf("E0003", imp.Sp, "cannot find import %q", imp.Path).Note("imports resolve relative to the importing file; \"std/...\" is built in")
				continue
			}
			k, prefix := key(ipath), ""
			if imp.Alias != "" {
				if _, dup := r.aliases[imp.Alias]; dup {
					dl.Errorf("E0005", imp.Sp, "module alias %q is already used in this file", imp.Alias)
					continue
				}
				p, ok := prefixOf[k]
				if !ok {
					p = imp.Alias
					for n := 2; usedPrefix[p]; n++ {
						p = fmt.Sprintf("%s%d", imp.Alias, n)
					}
					usedPrefix[p] = true
					prefixOf[k] = p
				}
				r.aliases[imp.Alias] = p
				prefix = p
			}
			if seen[prefix+"|"+k] {
				continue
			}
			seen[prefix+"|"+k] = true
			queue = append(queue, unit{f: syntax.Parse(ipath, data, dl), prefix: prefix})
		}
		r.file(f)
		if u.prefix != "" {
			f.Tests = nil
		}
		merged.Fns = append(merged.Fns, f.Fns...)
		merged.Structs = append(merged.Structs, f.Structs...)
		merged.Enums = append(merged.Enums, f.Enums...)
		merged.Globals = append(merged.Globals, f.Globals...)
	}
	Monomorphize(merged, dl)
	return merged
}

func key(p string) string {
	if strings.HasPrefix(p, "std/") {
		return p
	}
	abs, err := filepath.Abs(p)
	if err != nil {
		return p
	}
	return abs
}

func resolve(from, imp string) (string, []byte, bool) {
	if strings.HasPrefix(imp, "std/") {
		data, err := std.FS.ReadFile(strings.TrimPrefix(imp, "std/"))
		return imp, data, err == nil
	}
	p := imp
	if !filepath.IsAbs(p) {
		p = filepath.Join(filepath.Dir(from), imp)
	}
	data, err := os.ReadFile(p)
	return p, data, err == nil
}
