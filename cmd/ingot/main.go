// Command ingot is the Ingot compiler driver.
package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/hossein1376/ingot/internal/codegen"
	"github.com/hossein1376/ingot/internal/diag"
	"github.com/hossein1376/ingot/internal/driver"
	"github.com/hossein1376/ingot/internal/sema"
)

const version = "0.1.0-dev"

const usage = `ingot ` + version + ` - a low-overhead systems language for AI agents (AArch64 / Apple Silicon)

usage:
  ingot check FILE            type-check only
  ingot asm   FILE            print AArch64 assembly
  ingot build FILE [-o OUT]   compile to a native executable
  ingot run   FILE [-- ARGS]  compile and run (Linux targets run under qemu on x86 hosts)
  ingot test  FILE            compile and run the file's test blocks
  ingot builtins              list @builtins
  ingot version

flags:
  --target macos-arm64|linux-arm64   (default: host OS; macos-arm64 on macOS)
  --diag text|json                   diagnostics format; json = one object per line
  --unchecked                        disable bounds and division checks
  -o FILE                            output path
`

func main() { os.Exit(run(os.Args[1:])) }

func run(args []string) int {
	if len(args) == 0 {
		fmt.Fprint(os.Stderr, usage)
		return 2
	}
	cmd := args[0]
	switch cmd {
	case "version", "--version":
		fmt.Println("ingot", version)
		return 0
	case "builtins":
		for _, name := range sortedKeys(sema.Builtins) {
			fmt.Println(sema.Builtins[name])
		}
		return 0
	case "help", "-h", "--help":
		fmt.Print(usage)
		return 0
	}
	cfg := driver.Config{Target: driver.DefaultTarget(), Safe: true}
	diagFmt, out, file := "text", "", ""
	var progArgs []string
	rest := args[1:]
	for i := 0; i < len(rest); i++ {
		a := rest[i]
		val := func() string {
			if k := strings.IndexByte(a, '='); k >= 0 {
				return a[k+1:]
			}
			i++
			if i < len(rest) {
				return rest[i]
			}
			return ""
		}
		switch {
		case a == "--":
			progArgs = rest[i+1:]
			i = len(rest)
		case strings.HasPrefix(a, "--target"):
			t, err := codegen.ParseTarget(val())
			if err != nil {
				fmt.Fprintln(os.Stderr, "ingot:", err)
				return 2
			}
			cfg.Target = t
		case strings.HasPrefix(a, "--diag"):
			diagFmt = val()
		case a == "--unchecked":
			cfg.Safe = false
		case a == "-o":
			out = val()
		case strings.HasPrefix(a, "-"):
			fmt.Fprintf(os.Stderr, "ingot: unknown flag %s\n%s", a, usage)
			return 2
		default:
			file = a
		}
	}
	if file == "" {
		fmt.Fprint(os.Stderr, "ingot: missing FILE\n"+usage)
		return 2
	}
	src, err := os.ReadFile(file)
	if err != nil {
		fmt.Fprintln(os.Stderr, "ingot:", err)
		return 2
	}
	cfg.Tests = cmd == "test"
	var dl diag.List
	asm, ok := driver.Compile(file, src, cfg, &dl)
	if diagFmt == "json" {
		dl.WriteJSON(os.Stdout)
	} else {
		dl.WriteText(os.Stderr)
	}
	if !ok {
		return 1
	}
	switch cmd {
	case "check":
		return 0
	case "asm":
		if out != "" {
			return writeOrFail(out, asm)
		}
		fmt.Print(asm)
		return 0
	case "build", "run", "test":
	default:
		fmt.Fprintf(os.Stderr, "ingot: unknown command %q\n%s", cmd, usage)
		return 2
	}
	base := strings.TrimSuffix(filepath.Base(file), filepath.Ext(file))
	asmPath := filepath.Join(driver.BuildDir(), base+"-"+cfg.Target.Name+".s")
	if rc := writeOrFail(asmPath, asm); rc != 0 {
		return rc
	}
	if out == "" {
		out = base
		if cmd != "build" {
			out = filepath.Join(driver.BuildDir(), base+"-"+cfg.Target.Name)
		}
	}
	if err := driver.Link(asmPath, out, cfg.Target); err != nil {
		fmt.Fprintln(os.Stderr, "ingot:", err)
		return 1
	}
	if cmd == "build" {
		return 0
	}
	runner, err := driver.Runner(cfg.Target)
	if err != nil {
		fmt.Fprintln(os.Stderr, "ingot:", err)
		return 1
	}
	abs, _ := filepath.Abs(out)
	argv := append(append(runner, abs), progArgs...)
	c := exec.Command(argv[0], argv[1:]...)
	c.Stdin, c.Stdout, c.Stderr = os.Stdin, os.Stdout, os.Stderr
	if err := c.Run(); err != nil {
		if ee, ok := err.(*exec.ExitError); ok {
			return ee.ExitCode()
		}
		fmt.Fprintln(os.Stderr, "ingot:", err)
		return 1
	}
	return 0
}

func writeOrFail(path, s string) int {
	if err := os.WriteFile(path, []byte(s), 0o644); err != nil {
		fmt.Fprintln(os.Stderr, "ingot:", err)
		return 1
	}
	return 0
}

func sortedKeys(m map[string]string) []string {
	var ks []string
	for k := range m {
		ks = append(ks, k)
	}
	for i := 1; i < len(ks); i++ {
		for j := i; j > 0 && ks[j] < ks[j-1]; j-- {
			ks[j], ks[j-1] = ks[j-1], ks[j]
		}
	}
	return ks
}
