// Package diag defines compiler diagnostics. Every diagnostic has a stable
// code, a precise source span and optional machine-applicable fixes, so that
// both humans and AI agents can act on it.
package diag

import (
	"encoding/json"
	"fmt"
	"io"
	"sort"
)

// Pos is a 1-based line/column position (columns count bytes).
type Pos struct {
	Line int `json:"line"`
	Col  int `json:"col"`
}

// Span is a half-open source range [Start, End).
type Span struct {
	File  string `json:"file"`
	Start Pos    `json:"start"`
	End   Pos    `json:"end"`
}

// Edit replaces the text in Span with Text.
type Edit struct {
	Span Span   `json:"span"`
	Text string `json:"text"`
}

// Fix is a suggested, machine-applicable change.
type Fix struct {
	Message string `json:"message"`
	Edits   []Edit `json:"edits"`
}

// Diagnostic is one compiler message.
type Diagnostic struct {
	Severity string   `json:"severity"` // "error" | "warning" | "note"
	Code     string   `json:"code"`     // stable, e.g. "E0201"
	Message  string   `json:"message"`
	Span     Span     `json:"span"`
	Notes    []string `json:"notes,omitempty"`
	Fixes    []Fix    `json:"fixes,omitempty"`
}

// List collects diagnostics.
type List struct {
	Items []*Diagnostic
}

// Errorf records an error and returns it so callers can attach fixes/notes.
func (l *List) Errorf(code string, sp Span, format string, args ...any) *Diagnostic {
	d := &Diagnostic{Severity: "error", Code: code, Message: fmt.Sprintf(format, args...), Span: sp}
	l.Items = append(l.Items, d)
	return d
}

// HasErrors reports whether any error was recorded.
func (l *List) HasErrors() bool {
	for _, d := range l.Items {
		if d.Severity == "error" {
			return true
		}
	}
	return false
}

// Note adds a note to the diagnostic.
func (d *Diagnostic) Note(format string, args ...any) *Diagnostic {
	d.Notes = append(d.Notes, fmt.Sprintf(format, args...))
	return d
}

// Fix adds a single-edit fix.
func (d *Diagnostic) Fix(msg string, sp Span, text string) *Diagnostic {
	d.Fixes = append(d.Fixes, Fix{Message: msg, Edits: []Edit{{Span: sp, Text: text}}})
	return d
}

func (l *List) sorted() []*Diagnostic {
	out := append([]*Diagnostic(nil), l.Items...)
	sort.SliceStable(out, func(i, j int) bool {
		a, b := out[i].Span, out[j].Span
		if a.File != b.File {
			return a.File < b.File
		}
		if a.Start.Line != b.Start.Line {
			return a.Start.Line < b.Start.Line
		}
		return a.Start.Col < b.Start.Col
	})
	return out
}

// WriteJSON writes one JSON object per line (JSON Lines), sorted by position.
func (l *List) WriteJSON(w io.Writer) {
	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(false)
	for _, d := range l.sorted() {
		_ = enc.Encode(d)
	}
}

// WriteText writes human-readable diagnostics.
func (l *List) WriteText(w io.Writer) {
	for _, d := range l.sorted() {
		fmt.Fprintf(w, "%s:%d:%d: %s[%s]: %s\n", d.Span.File, d.Span.Start.Line, d.Span.Start.Col, d.Severity, d.Code, d.Message)
		for _, n := range d.Notes {
			fmt.Fprintf(w, "  note: %s\n", n)
		}
		for _, f := range d.Fixes {
			fmt.Fprintf(w, "  fix: %s:", f.Message)
			for _, e := range f.Edits {
				fmt.Fprintf(w, " [%d:%d-%d:%d -> %q]", e.Span.Start.Line, e.Span.Start.Col, e.Span.End.Line, e.Span.End.Col, e.Text)
			}
			fmt.Fprintln(w)
		}
	}
}
