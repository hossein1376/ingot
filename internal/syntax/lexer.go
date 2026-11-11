// Package syntax contains the Ingot lexer, AST and parser.
package syntax

import (
	"fmt"

	"github.com/hossein1376/ingot/internal/diag"
)

// Kind is a token kind.
type Kind int

const (
	EOF Kind = iota
	Ident
	Int
	Float
	String  // "..."  -> []u8
	CString // c"..." -> *u8
	Builtin // @name
	Punct   // operators and delimiters; text in Tok.Text
	Keyword
)

var keywords = map[string]bool{
	"fn": true, "pub": true, "extern": true, "struct": true, "let": true, "var": true,
	"const": true, "if": true, "else": true, "while": true, "for": true, "in": true,
	"return": true, "break": true, "continue": true, "true": true, "false": true,
	"null": true, "as": true, "test": true, "import": true, "enum": true, "switch": true, "defer": true, "try": true, "inline": true, "catch": true,
}

// Tok is a token.
type Tok struct {
	Kind Kind
	Text string // raw text; for strings, the decoded value
	Pos  diag.Pos
	End  diag.Pos
}

func (t Tok) String() string {
	switch t.Kind {
	case EOF:
		return "end of file"
	case String, CString:
		return fmt.Sprintf("string %q", t.Text)
	}
	return fmt.Sprintf("%q", t.Text)
}

// longest first
var puncts = []string{
	"<<=", ">>=", "...",
	"->", "=>", "==", "!=", "<=", ">=", "&&", "||", "<<", ">>", "+=", "-=", "*=", "/=", "%=",
	"&=", "|=", "^=", "..", ".*", ".{", ".[",
	"+", "-", "*", "/", "%", "&", "|", "^", "!", "~", "<", ">", "=", "(", ")", "{", "}",
	"[", "]", ",", ";", ":", ".",
}

// Lex tokenizes src. Errors are reported to dl.
func Lex(file string, src []byte, dl *diag.List) []Tok {
	var toks []Tok
	line, col := 1, 1
	i := 0
	adv := func(n int) {
		for k := 0; k < n; k++ {
			if src[i] == '\n' {
				line++
				col = 1
			} else {
				col++
			}
			i++
		}
	}
	span := func(s, e diag.Pos) diag.Span { return diag.Span{File: file, Start: s, End: e} }
	for i < len(src) {
		c := src[i]
		if c == ' ' || c == '\t' || c == '\r' || c == '\n' {
			adv(1)
			continue
		}
		if c == '/' && i+1 < len(src) && src[i+1] == '/' {
			for i < len(src) && src[i] != '\n' {
				adv(1)
			}
			continue
		}
		start := diag.Pos{Line: line, Col: col}
		switch {
		case isAlpha(c) || c == '@':
			j := i + 1
			for j < len(src) && (isAlpha(src[j]) || isDigit(src[j])) {
				j++
			}
			text := string(src[i:j])
			if text == "c" && j < len(src) && src[j] == '"' {
				adv(1)
				s, ok := lexString(src, &i, &line, &col)
				if !ok {
					dl.Errorf("E0002", span(start, diag.Pos{Line: line, Col: col}), "unterminated or invalid string literal")
				}
				toks = append(toks, Tok{CString, s, start, diag.Pos{Line: line, Col: col}})
				continue
			}
			adv(j - i)
			k := Ident
			if text[0] == '@' {
				k = Builtin
				if len(text) == 1 {
					dl.Errorf("E0001", span(start, diag.Pos{Line: line, Col: col}), "expected builtin name after '@'")
				}
			} else if keywords[text] {
				k = Keyword
			}
			toks = append(toks, Tok{k, text, start, diag.Pos{Line: line, Col: col}})
		case isDigit(c):
			j := i
			kind := Int
			if c == '0' && j+1 < len(src) && (src[j+1] == 'x' || src[j+1] == 'b') {
				j += 2
				for j < len(src) && (isHex(src[j]) || src[j] == '_') {
					j++
				}
			} else {
				for j < len(src) && (isDigit(src[j]) || src[j] == '_') {
					j++
				}
				// a '.' followed by a digit makes a float; ".." is a range
				if j+1 < len(src) && src[j] == '.' && isDigit(src[j+1]) {
					kind = Float
					j++
					for j < len(src) && (isDigit(src[j]) || src[j] == '_') {
						j++
					}
				}
				if j < len(src) && (src[j] == 'e' || src[j] == 'E') {
					kind = Float
					j++
					if j < len(src) && (src[j] == '+' || src[j] == '-') {
						j++
					}
					for j < len(src) && isDigit(src[j]) {
						j++
					}
				}
			}
			text := string(src[i:j])
			adv(j - i)
			toks = append(toks, Tok{kind, text, start, diag.Pos{Line: line, Col: col}})
		case c == '"':
			s, ok := lexString(src, &i, &line, &col)
			if !ok {
				dl.Errorf("E0002", span(start, diag.Pos{Line: line, Col: col}), "unterminated or invalid string literal")
			}
			toks = append(toks, Tok{String, s, start, diag.Pos{Line: line, Col: col}})
		default:
			matched := ""
			for _, p := range puncts {
				if i+len(p) <= len(src) && string(src[i:i+len(p)]) == p {
					matched = p
					break
				}
			}
			if matched == "" {
				adv(1)
				dl.Errorf("E0001", span(start, diag.Pos{Line: line, Col: col}), "unexpected character %q", c)
				continue
			}
			adv(len(matched))
			toks = append(toks, Tok{Punct, matched, start, diag.Pos{Line: line, Col: col}})
		}
	}
	toks = append(toks, Tok{Kind: EOF, Pos: diag.Pos{Line: line, Col: col}, End: diag.Pos{Line: line, Col: col}})
	return toks
}

// lexString decodes a string starting at src[*i] == '"'.
func lexString(src []byte, i, line, col *int) (string, bool) {
	var out []byte
	*i++
	*col++
	for *i < len(src) {
		c := src[*i]
		if c == '\n' {
			return string(out), false
		}
		*i++
		*col++
		if c == '"' {
			return string(out), true
		}
		if c == '\\' {
			if *i >= len(src) {
				return string(out), false
			}
			e := src[*i]
			*i++
			*col++
			switch e {
			case 'n':
				out = append(out, '\n')
			case 't':
				out = append(out, '\t')
			case 'r':
				out = append(out, '\r')
			case '0':
				out = append(out, 0)
			case '\\', '"', '\'':
				out = append(out, e)
			case 'x':
				if *i+2 > len(src) || !isHex(src[*i]) || !isHex(src[*i+1]) {
					return string(out), false
				}
				out = append(out, hexVal(src[*i])<<4|hexVal(src[*i+1]))
				*i += 2
				*col += 2
			default:
				return string(out), false
			}
			continue
		}
		out = append(out, c)
	}
	return string(out), false
}

func isAlpha(c byte) bool { return c == '_' || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') }
func isDigit(c byte) bool { return c >= '0' && c <= '9' }
func isHex(c byte) bool   { return isDigit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F') }
func hexVal(c byte) byte {
	switch {
	case isDigit(c):
		return c - '0'
	case c >= 'a':
		return c - 'a' + 10
	}
	return c - 'A' + 10
}
