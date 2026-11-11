package driver

import (
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
func Load(path string, src []byte, dl *diag.List) *syntax.File {
	root := syntax.Parse(path, src, dl)
	seen := map[string]bool{key(path): true}
	queue := []*syntax.File{root}
	merged := &syntax.File{Name: path, Tests: root.Tests}
	for len(queue) > 0 {
		f := queue[0]
		queue = queue[1:]
		merged.Fns = append(merged.Fns, f.Fns...)
		merged.Structs = append(merged.Structs, f.Structs...)
		merged.Globals = append(merged.Globals, f.Globals...)
		for _, imp := range f.Imports {
			ipath, data, ok := resolve(f.Name, imp.Path)
			if !ok {
				dl.Errorf("E0003", imp.Sp, "cannot find import %q", imp.Path).Note("imports resolve relative to the importing file; \"std/...\" is built in")
				continue
			}
			if seen[key(ipath)] {
				continue
			}
			seen[key(ipath)] = true
			queue = append(queue, syntax.Parse(ipath, data, dl))
		}
	}
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
