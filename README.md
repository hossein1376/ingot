# tree-sitter-ingot

Tree-sitter grammar for the [Ingot](https://github.com/hossein1376/ingot) language.
Derived from `docs/grammar.md` in the compiler repo. It is a highlighting-grade grammar:
it parses the MVP language (it accepts a superset in ambiguous corners, and it does not
enforce semantic rules such as `E02xx` rejections) and reports precise positions for
syntax errors. The compiler remains the source of truth for diagnostics.

Every `.ingot` file in `conformance/` parses without `ERROR` nodes, except the three
`expect: reject` files whose syntax is the error.

## Layout

- `grammar.js` — grammar source
- `src/parser.c` — generated parser (committed; Zed compiles this directly)
- `queries/` — editor queries: `highlights.scm`, `brackets.scm`, `outline.scm`, `indents.scm`
- `package.json`, `tree-sitter.json` — tree-sitter CLI metadata

## Regenerating the parser

```sh
npm install
npx tree-sitter generate
find ../conformance -name '*.ingot' -print0 | xargs -0 npx tree-sitter parse --quiet
```

## Zed

The `zed-ingot/` directory in the main compiler repo is the companion Zed extension.
See `zed-ingot/README.md`.
