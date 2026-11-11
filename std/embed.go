// Package std embeds Ingot's standard library sources into the compiler,
// so `import "std/par.ig";` works without any installation step.
package std

import "embed"

// FS holds the *.ig files of the standard library.
//
//go:embed *.ig
var FS embed.FS
