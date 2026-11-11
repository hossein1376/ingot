package diag

import (
	"sort"
	"strings"
)

// ApplyFixes applies the first fix of every diagnostic for file to src.
// Overlapping edits are skipped. It returns the new text and the number of
// fixes applied.
func ApplyFixes(src []byte, file string, ds []*Diagnostic) ([]byte, int) {
	lineStart := []int{0}
	for i, c := range src {
		if c == '\n' {
			lineStart = append(lineStart, i+1)
		}
	}
	offset := func(p Pos) int {
		if p.Line-1 >= len(lineStart) || p.Line < 1 {
			return -1
		}
		return lineStart[p.Line-1] + p.Col - 1
	}
	type edit struct {
		start, end int
		text       string
		fix        int
	}
	var edits []edit
	for i, d := range ds {
		if len(d.Fixes) == 0 {
			continue
		}
		for _, e := range d.Fixes[0].Edits {
			if e.Span.File != file {
				continue
			}
			s, t := offset(e.Span.Start), offset(e.Span.End)
			if s < 0 || t < s || t > len(src) {
				continue
			}
			edits = append(edits, edit{s, t, e.Text, i})
		}
	}
	sort.SliceStable(edits, func(i, j int) bool { return edits[i].start > edits[j].start })
	var b strings.Builder
	out := string(src)
	applied := map[int]bool{}
	limit := len(src) + 1
	for _, e := range edits {
		if e.end > limit {
			continue // overlaps an edit already applied
		}
		out = out[:e.start] + e.text + out[e.end:]
		limit = e.start
		applied[e.fix] = true
	}
	b.WriteString(out)
	return []byte(b.String()), len(applied)
}
