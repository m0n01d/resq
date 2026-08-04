# resq

A CLI for querying and editing ReScript files — like `jq` for ReScript.

A port of [elmq](https://github.com/caseyWebb/elmq) to ReScript. Designed as a next-gen LSP **for
agents and scripts, not editors**: token-efficient, structured output, write-safe mutation.

Targets **ReScript 12**. Every sample below is real output, not illustrative.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/m0n01d/resq/main/scripts/install.sh | sh
export PATH="$HOME/.cargo/bin:$PATH"
```

Installs Rust first if `cargo` is missing. From a clone: `cargo install --path .`

---

## For AI coding agents

**Read this section, or run `resq guide`, before editing ReScript.**

resq exists so an agent can read and modify ReScript without pulling whole files into context or
hand-splicing text. Two rules cover most of the value:

1. **Prefer `resq get` over reading the file.** It returns exactly one declaration, with its
   decorators and doc comment, in a fraction of the tokens.
2. **Prefer `resq set decl` / `patch` / `rm decl` over rewriting the file.** Every write is
   validated; a failed write leaves the file byte-identical, so a bad edit costs you nothing.

### Wiring it into an agent

```sh
resq guide          # prints the full agent-facing reference to stdout
```

- **Claude Code** — add the install command and a short usage section to the project's `CLAUDE.md`,
  or pipe `resq guide` into a SessionStart hook.
- **Any other agent** (Cursor, Aider, Codex, …) — pipe `resq guide` into the system prompt or
  project instructions. resq is a plain CLI; anything that can shell out can drive it.

### The things agents get wrong

- **Address by dot-path.** `helper`, `Inner.helper`, `Inner.Deep.helper`. A bare name **never**
  matches a nested declaration — there is no implicit search, and ambiguity is an error rather than
  a guess.
- **Decorators and doc comments are part of the declaration.** `get` returns them and `rm decl`
  removes them. A `get` that dropped `@react.component` would hand you code that doesn't compile.
- **There is no `expose`/`unexpose`.** `.resi` interface files parse with the same grammar as
  `.res`, so edit them with the ordinary commands — point `set decl` / `rm decl` / `patch` at the
  `.resi` directly.
- **Refusals are informative, not obstacles.** When resq refuses, the error names the fix. Read it
  rather than reaching for `--force`.
- **`--format json` on every read command** when you want to parse rather than display.

---

## Reading

Read commands are tolerant: on a file with parse errors they warn on stderr and return whatever they
could recover.

### `resq list` — module summary

```sh
$ resq list src/Main.res
```
```
module Main  (20 lines)

opens:
  Belt

functions:
  main     L3-8
  shouted  L18

modules:
  Util  L10-16
    shout  L11
    Deep   L13-15
      answer  L14
```

Nesting is shown by indentation. `--docs` adds doc comments:

```sh
$ resq list src/Greeting.res --docs
```
```
module Greeting  (15 lines)

types:
  tone  L1-4
    /** How enthusiastic a greeting should be. */

functions:
  make            L6-11
    /** Render a greeting for `name` in the given `tone`. */
  defaultTone     L13
  internalSecret  L15
```

### `resq get` — one declaration, in full

```sh
$ resq get src/Main.res main
```
```rescript
/** Entry point for the demo. */
@genType
let main = () => {
  let msg = Greeting.make(~name="ReScript", ~tone=Greeting.Excited)
  Console.log(msg)
}
```

The doc comment and `@genType` come with it. Nested declarations use their dot-path:

```sh
$ resq get src/Main.res Util.Deep.answer
```
```rescript
let answer = 42
```

Several files at once: `resq get -f a.res foo -f b.res bar`.

### `resq grep` — search, annotated with the enclosing declaration

```sh
$ resq grep 'Greeting\.' src
```
```
src/Main.res:6:13:main:  let msg = Greeting.make(~name="ReScript", ~tone=Greeting.Excited)
src/Main.res:6:51:main:  let msg = Greeting.make(~name="ReScript", ~tone=Greeting.Excited)
src/Main.res:18:26:shouted:let shouted = Util.shout(Greeting.make(~name="world", ~tone=Greeting.defaultTone))
src/Main.res:18:61:shouted:let shouted = Util.shout(Greeting.make(~name="world", ~tone=Greeting.defaultTone))
```

Format is `file:line:col:declaration:text`. The dot-path annotation is the point — plain `rg`
already does the rest.

Matches inside comments and string literals are excluded by default; `--include-comments` and
`--include-strings` re-enable them (node-kind aware, not a regex hack). `--definitions` restricts to
declaration names; `--source` prints the whole enclosing declaration.

**Exit codes are part of the contract:** `0` matches, `1` no matches, `2` error.

### `resq refs` — every reference, project-wide

```sh
$ resq refs src/Greeting.res make
```
```
src/Greeting.res:7:5  definition  make  make
src/Greeting.resi:5:5  definition  make  make
src/Main.res:6:13  qualified   main  Greeting.make
src/Main.res:18:26  qualified   shouted  Greeting.make
```

Note it reports the `.resi` signature as a definition too — a rename has to edit both files, and
missing the interface is exactly the silent breakage `refs` exists to prevent.

Requires a project root (`rescript.json` or `bsconfig.json`). Classifications: `definition`,
`qualified`, `via-alias`, `unqualified-via-open`, `unqualified-shadowed`.

`refs` deliberately **over-reports rather than under-reports** — it matches by name without reading
module signatures, and flags shadowed hits rather than dropping them. Before a rename, a false
positive you can dismiss beats a missed use.

## JSON output

Every read command takes `--format json`.

```sh
$ resq list src/Main.res --format json
```
```json
{
  "module_name": "Main",
  "opens": ["Belt"],
  "aliases": [],
  "declarations": [
    {
      "paths": ["main"],
      "kind": "let",
      "binder_kind": "simple",
      "decorators": ["@genType"],
      "doc_comment": "/** Entry point for the demo. */",
      "start_line": 3,
      "end_line": 8
    },
    {
      "paths": ["Util.Deep.answer"],
      "kind": "let",
      "binder_kind": "simple",
      "start_line": 14,
      "end_line": 14
    }
  ]
}
```

`paths` is a list because one ReScript binding can bind several names — `let (a, b) = pair`.

`grep` and `refs` emit newline-delimited JSON, one object per hit:

```sh
$ resq refs src/Greeting.res make --format json
```
```json
{"column":5,"decl":"make","file":"src/Greeting.res","kind":"definition","line":7,"target":"Greeting.make","text":"make"}
{"column":13,"decl":"main","file":"src/Main.res","kind":"qualified","line":6,"target":"Greeting.make","text":"Greeting.make"}
```

## Writing

```sh
$ resq set decl src/Main.res --name farewell --content 'let farewell = (~name: string) => "Bye, " ++ name'
ok

$ resq patch src/Main.res farewell --old '"Bye, "' --new '"Farewell, "'
ok

$ resq rm decl src/Main.res main
ok

$ resq add alias src/Main.res Arr=Belt.Array
ok
```

Also `add open` and `rm open`. Content for `set decl` comes from `--content` or stdin.

### Write safety

Every write command:

1. refuses a file that already has parse errors,
2. re-parses the buffer it built and refuses if the result wouldn't parse,
3. leaves the file **byte-for-byte unchanged** on any failure,
4. prints `ok` on success.

A failed `resq` write never leaves you worse off — which is the reason to prefer it to `sed` or
hand-editing. Beyond parsing, `cargo test` runs the **real ReScript compiler** over resq's output
(see `SPEC.md` §7.1), so edits are verified to compile, not merely to parse.

### It refuses on purpose

```sh
$ resq rm decl src/Greeting.res defaultTone
Error: refusing to remove `defaultTone` from src/Greeting.res: src/Greeting.resi still declares it.
Remove the signature first (`resq rm decl src/Greeting.resi defaultTone`), then retry.
[exit 1]

$ resq patch src/Main.res shouted --old 'Greeting' --new 'G'
Error: resq patch: `--old` matches 2 times within `shouted` in src/Main.res; it must match exactly
once — narrow the search string
[exit 1]

$ resq set decl src/Main.res --name oops --content 'let oops = ('
Error: resq set decl: the given content does not parse at 1:1
[exit 1]

$ resq rm decl src/Bad.res broken
Error: refusing to edit src/Bad.res: file has pre-existing parse errors at 1:1
[exit 1]
```

Three refusals are deliberate design rather than limitations: a sibling `.resi` still declaring the
name; a multi-name binding where you didn't list every name (`let (a, b) = pair` — removing "just
`a`" would silently unbind `b`); and `rm open` when it can't prove an unqualified reference is
unrelated (`--force` overrides). Each error prints the corrective command.

## Known gaps

A few constructs don't parse under the pinned tree-sitter grammar — upstream bugs, all rare, all
listed in `SPEC.md` §0.1 with inverted tests so an upstream fix gets noticed:

`%replace.type(: T)` · negative bigint `-1n` · two consecutive trailing comments closing a module
block · local-open sugar `Types.(e)` / `Types.{…}` / `Types.[…]`

Read commands degrade gracefully on these; write commands refuse to touch the file, which is the
safe failure. A module referenced *only* through local-open sugar is invisible to `refs`.

Measured against the ReScript compiler repo (3,181 real `.res`/`.resi` files): **99.3% parse clean**
excluding `%replace.type`, which appears almost exclusively in deprecated stdlib shims.

## Status

v1 covers reads and single-file writes. `mv`, `rename decl`, `move-decl` and the `variant` family
are deferred to v2. `refs` does not follow `include` transitively — its largest gap.

MIT.
