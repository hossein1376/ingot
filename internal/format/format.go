// Package format implements `ingot fmt`: canonical layout that never changes
// the token sequence. It re-indents by bracket depth (4 spaces per level),
// strips trailing whitespace, collapses runs of blank lines and ends the file
// with one newline. Comments and intra-line spacing are kept as written, so
// the result is always safe to apply, and Source verifies this by re-lexing.
package format

import (
	"fmt"
	"strings"

	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/syntax"
)

// Source formats src. It returns an error if src does not lex cleanly.
func Source(file string, src []byte) ([]byte, error) {
	var dl diag.List
	toks := syntax.Lex(file, src, &dl)
	if dl.HasErrors() {
		return nil, fmt.Errorf("%s: cannot format a file with lexical errors (run ingot check)", file)
	}
	lines := strings.Split(strings.ReplaceAll(string(src), "\r\n", "\n"), "\n")
	// depth at the start of each line, and whether the line opens with a closer
	opens := make([]int, len(lines)+2)     // net brackets opened on each line
	leadClose := make([]int, len(lines)+2) // closers before any other token
	seen := make([]bool, len(lines)+2)     // a non-closer token was seen on the line
	inToken := make([]bool, len(lines)+2)  // line starts inside a multi-line token
	for _, t := range toks {
		if t.Kind == syntax.EOF {
			break
		}
		l := t.Pos.Line
		for k := t.Pos.Line + 1; k <= t.End.Line && k < len(inToken); k++ {
			inToken[k] = true
		}
		closer := t.Kind == syntax.Punct && (t.Text == "}" || t.Text == ")" || t.Text == "]")
		if t.Kind == syntax.Punct {
			switch t.Text {
			case "{", "(", "[", ".{", ".[":
				opens[l]++
			case "}", ")", "]":
				opens[l]--
			}
		}
		if closer && !seen[l] {
			leadClose[l]++
		} else {
			seen[l] = true
		}
	}
	var out []string
	depth, blank := 0, 0
	for i, line := range lines {
		ln := i + 1
		if inToken[ln] {
			out = append(out, line) // inside a string literal: keep verbatim
			depth += opens[ln]
			continue
		}
		text := strings.TrimRight(strings.TrimLeft(line, " \t"), " \t")
		if text == "" {
			blank++
			depth += opens[ln]
			continue
		}
		if blank > 0 && len(out) > 0 {
			out = append(out, "")
		}
		blank = 0
		d := depth - leadClose[ln]
		if d < 0 {
			d = 0
		}
		out = append(out, strings.Repeat("    ", d)+text)
		depth += opens[ln]
	}
	res := []byte(strings.Join(out, "\n") + "\n")
	// safety net: the token stream must be unchanged
	var dl2 diag.List
	after := syntax.Lex(file, res, &dl2)
	if dl2.HasErrors() || len(after) != len(toks) {
		return nil, fmt.Errorf("%s: internal formatter error (token stream changed); file left unchanged", file)
	}
	for i := range toks {
		if toks[i].Kind != after[i].Kind || toks[i].Text != after[i].Text {
			return nil, fmt.Errorf("%s: internal formatter error at token %d; file left unchanged", file, i)
		}
	}
	return res, nil
}
