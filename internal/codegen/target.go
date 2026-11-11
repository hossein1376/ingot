// Package codegen lowers type-checked Ingot to AArch64 assembly text for
// macOS (Mach-O, Apple arm64 ABI) or Linux (ELF, AAPCS64).
package codegen

import (
	"fmt"
	"strings"
)

// Target describes an output platform.
type Target struct {
	Name   string // "macos-arm64" | "linux-arm64"
	Darwin bool
}

var (
	MacOS = Target{Name: "macos-arm64", Darwin: true}
	Linux = Target{Name: "linux-arm64"}
)

// ParseTarget resolves a target name.
func ParseTarget(s string) (Target, error) {
	switch s {
	case "macos-arm64", "darwin-arm64", "macos":
		return MacOS, nil
	case "linux-arm64", "linux-aarch64", "linux":
		return Linux, nil
	}
	return Target{}, fmt.Errorf("unknown target %q (want macos-arm64 or linux-arm64)", s)
}

// Sym is the assembly name of a C-level symbol.
// Module-qualified names ("map.put") become "map__put", and generic
// instances ("sum[f64]", "Pair[*u8,i64]") get assembler-safe spellings.
func (t Target) Sym(name string) string {
	name = symReplacer.Replace(name)
	if t.Darwin {
		return "_" + name
	}
	return name
}

// Local returns an assembler-local label.
func (t Target) Local(name string) string {
	if t.Darwin {
		return "L" + name
	}
	return ".L" + name
}

// Page/PageOff are the adrp/add relocation operands for a symbol.
func (t Target) Page(sym string) string {
	if t.Darwin {
		return sym + "@PAGE"
	}
	return sym
}

func (t Target) PageOff(sym string) string {
	if t.Darwin {
		return sym + "@PAGEOFF"
	}
	return ":lo12:" + sym
}

// GotPage/GotPageOff load the address of an external symbol via the GOT.
func (t Target) GotPage(sym string) string {
	if t.Darwin {
		return sym + "@GOTPAGE"
	}
	return ":got:" + sym
}

func (t Target) GotPageOff(sym string) string {
	if t.Darwin {
		return sym + "@GOTPAGEOFF"
	}
	return ":got_lo12:" + sym
}

func (t Target) TextSection() string {
	if t.Darwin {
		return ".section __TEXT,__text,regular,pure_instructions"
	}
	return ".text"
}

func (t Target) CStringSection() string {
	if t.Darwin {
		return ".section __TEXT,__cstring,cstring_literals"
	}
	return ".section .rodata.str1.1,\"aMS\",@progbits,1"
}

func (t Target) DataSection() string {
	if t.Darwin {
		return ".section __DATA,__data"
	}
	return ".data"
}

var symReplacer = strings.NewReplacer(".", "__", "[", "_G_", "]", "_", ",", "_", "*", "P", "(", "_", ")", "_", "-", "_", ">", "_", "!", "_E_")
