package diag

import "sort"

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
	// Fixes are taken whole, in diagnostic order: a fix is accepted only if
	// none of its edits overlap an edit of an already accepted fix, so a
	// multi-edit fix is never half-applied. Rejected fixes are left for the
	// next round (ingot fix re-checks and repeats).
	overlaps := func(a, b edit) bool {
		return (a.start < b.end && b.start < a.end) || a.start == b.start
	}
	var accepted []edit
	applied := 0
	for i, d := range ds {
		if len(d.Fixes) == 0 {
			continue
		}
		var mine []edit
		ok := true
		for _, e := range d.Fixes[0].Edits {
			if e.Span.File != file {
				continue
			}
			s, t := offset(e.Span.Start), offset(e.Span.End)
			if s < 0 || t < s || t > len(src) {
				ok = false
				break
			}
			mine = append(mine, edit{s, t, e.Text, i})
		}
		for _, m := range mine {
			for _, a := range accepted {
				if overlaps(m, a) {
					ok = false
				}
			}
		}
		if !ok || len(mine) == 0 {
			continue
		}
		accepted = append(accepted, mine...)
		applied++
	}
	sort.SliceStable(accepted, func(i, j int) bool { return accepted[i].start > accepted[j].start })
	out := string(src)
	for _, e := range accepted {
		out = out[:e.start] + e.text + out[e.end:]
	}
	return []byte(out), applied
}
