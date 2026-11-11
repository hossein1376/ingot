// Package sema implements Ingot's type checker. It resolves names, assigns a
// type to every expression and computes C-compatible layouts.
package sema

import (
	"fmt"
	"strings"
)

// Kind classifies types.
type Kind int

const (
	Void Kind = iota
	Bool
	Int
	Float
	Ptr
	Array
	Slice
	Struct
	Fn
	Vec
	NullT // type of the `null` literal before it meets a pointer type
)

// Field is a struct field with its byte offset.
type Field struct {
	Name   string
	Type   *Type
	Offset int64
}

// Type describes an Ingot type. Primitive and struct types are unique
// pointers; composite types compare structurally via Identical.
type Type struct {
	Kind     Kind
	Name     string // primitives, structs
	Size     int64
	Align    int64
	Signed   bool
	Elem     *Type // Ptr, Array, Slice, Vec
	Len      int64 // Array length, Vec lanes
	Fields   []Field
	Params   []*Type
	Ret      *Type
	Variadic bool
}

var (
	TVoid = &Type{Kind: Void, Name: "void", Size: 0, Align: 1}
	TBool = &Type{Kind: Bool, Name: "bool", Size: 1, Align: 1}
	TI8   = &Type{Kind: Int, Name: "i8", Size: 1, Align: 1, Signed: true}
	TI16  = &Type{Kind: Int, Name: "i16", Size: 2, Align: 2, Signed: true}
	TI32  = &Type{Kind: Int, Name: "i32", Size: 4, Align: 4, Signed: true}
	TI64  = &Type{Kind: Int, Name: "i64", Size: 8, Align: 8, Signed: true}
	TU8   = &Type{Kind: Int, Name: "u8", Size: 1, Align: 1}
	TU16  = &Type{Kind: Int, Name: "u16", Size: 2, Align: 2}
	TU32  = &Type{Kind: Int, Name: "u32", Size: 4, Align: 4}
	TU64  = &Type{Kind: Int, Name: "u64", Size: 8, Align: 8}
	TF32  = &Type{Kind: Float, Name: "f32", Size: 4, Align: 4, Signed: true}
	TF64  = &Type{Kind: Float, Name: "f64", Size: 8, Align: 8, Signed: true}
	TNull = &Type{Kind: NullT, Name: "null", Size: 8, Align: 8}
)

func vec(name string, elem *Type, lanes int64) *Type {
	return &Type{Kind: Vec, Name: name, Size: 16, Align: 16, Elem: elem, Len: lanes}
}

var (
	TF32x4 = vec("f32x4", TF32, 4)
	TF64x2 = vec("f64x2", TF64, 2)
	TI32x4 = vec("i32x4", TI32, 4)
	TI64x2 = vec("i64x2", TI64, 2)
	TU8x16 = vec("u8x16", TU8, 16)
)

var primitives = map[string]*Type{}

func init() {
	for _, t := range []*Type{TVoid, TBool, TI8, TI16, TI32, TI64, TU8, TU16, TU32, TU64, TF32, TF64, TF32x4, TF64x2, TI32x4, TI64x2, TU8x16} {
		primitives[t.Name] = t
	}
}

// PtrTo returns *elem.
func PtrTo(elem *Type) *Type { return &Type{Kind: Ptr, Size: 8, Align: 8, Elem: elem} }

// SliceOf returns []elem. Layout: { ptr: *elem, len: i64 }.
func SliceOf(elem *Type) *Type { return &Type{Kind: Slice, Size: 16, Align: 8, Elem: elem} }

// ArrayOf returns [n]elem.
func ArrayOf(n int64, elem *Type) *Type {
	return &Type{Kind: Array, Size: n * elem.Size, Align: elem.Align, Elem: elem, Len: n}
}

// Identical reports type identity.
func Identical(a, b *Type) bool {
	if a == b {
		return true
	}
	if a == nil || b == nil || a.Kind != b.Kind {
		return false
	}
	switch a.Kind {
	case Ptr, Slice:
		return Identical(a.Elem, b.Elem)
	case Array:
		return a.Len == b.Len && Identical(a.Elem, b.Elem)
	case Fn:
		if len(a.Params) != len(b.Params) || a.Variadic != b.Variadic || !Identical(a.Ret, b.Ret) {
			return false
		}
		for i := range a.Params {
			if !Identical(a.Params[i], b.Params[i]) {
				return false
			}
		}
		return true
	}
	return false // primitives and structs are unique
}

func (t *Type) String() string {
	switch t.Kind {
	case Ptr:
		return "*" + t.Elem.String()
	case Slice:
		return "[]" + t.Elem.String()
	case Array:
		return fmt.Sprintf("[%d]%s", t.Len, t.Elem)
	case Fn:
		var ps []string
		for _, p := range t.Params {
			ps = append(ps, p.String())
		}
		if t.Variadic {
			ps = append(ps, "...")
		}
		s := "fn(" + strings.Join(ps, ", ") + ")"
		if t.Ret != TVoid {
			s += " -> " + t.Ret.String()
		}
		return s
	}
	return t.Name
}

// IsScalarInt reports types held in general-purpose registers.
func (t *Type) IsGPR() bool {
	switch t.Kind {
	case Bool, Int, Ptr, Fn, NullT:
		return true
	}
	return false
}

// IsFPR reports types held in SIMD&FP registers.
func (t *Type) IsFPR() bool { return t.Kind == Float || t.Kind == Vec }

// IsAggregate reports types handled by address (memory).
func (t *Type) IsAggregate() bool {
	return t.Kind == Array || t.Kind == Slice || t.Kind == Struct
}

// IsNumeric reports int or float.
func (t *Type) IsNumeric() bool { return t.Kind == Int || t.Kind == Float }
