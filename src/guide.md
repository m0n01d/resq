# resq — agent integration guide

`resq` queries and edits ReScript files structurally. It exists so an agent can read and modify
ReScript without loading whole files into context or hand-splicing text.

Targets **ReScript 12**. Requires `rescript.json` (or `bsconfig.json`) for project-wide commands.

## Addressing: dot-paths

Every declaration is addressed by a dot-path relative to the file root:

```
helper              top-level
Inner.helper        inside `module Inner`
Inner.Deep.helper   arbitrary depth
```

A **bare name never matches a nested declaration**. There is no implicit search — ambiguity is an
error, not a guess. This is deliberate: implicit search makes `rm` silently hit the wrong thing.

## Reading (tolerant — warns on parse errors, keeps going)

```sh
resq list src/Main.res              # module summary, nesting shown by indentation
resq list src/Main.res --docs       # include doc comments
resq get src/View.res make          # full source of one declaration
resq get -f a.res foo -f b.res bar  # several files at once
resq grep 'pattern' src/            # regex search, annotated with enclosing dot-path
resq refs src/Types.res msg         # every reference, project-wide
```

`get` output **includes decorators and the doc comment**. A `get` of a `@react.component` binding
returns the decorator too — otherwise what you get back would not compile.

`grep` excludes matches inside comments and string literals by default; `--include-comments` and
`--include-strings` re-enable them. Exit codes: `0` matches, `1` none, `2` error.

## Writing (paranoid — refuses rather than corrupts)

```sh
resq set decl src/Main.res --name helper --content 'let helper = x => x * 2'
echo 'let helper = x => x' | resq set decl src/Main.res --name helper
resq patch src/Main.res update --old 'count + 1' --new 'count + step'
resq rm decl src/Main.res entry
resq add open src/Main.res Belt
resq add alias src/Main.res Arr=Belt.Array
resq rm open src/Main.res Belt
```

Every write command:
1. refuses a file that already has parse errors,
2. re-parses the buffer it built and refuses if the result would not parse,
3. leaves the file **byte-for-byte unchanged** on any failure,
4. prints `ok` on success.

So a failed `resq` write never leaves you worse off. Errors name the file, the location, and
usually the exact command to fix it.

`rm decl` removes the declaration **with** its decorators and doc comment.

## Things that will surprise you

**There are no `expose` / `unexpose` commands.** ReScript's `.resi` interface files are optional and
parse with the same grammar as `.res`, so you edit them with the ordinary commands — point `set
decl` / `rm decl` / `patch` at the `.resi` directly.

**`rm decl` refuses when a sibling `.resi` still declares the name.** Removing only the `.res` side
leaves a project that does not compile. Remove the signature first, then the implementation.

**`rm decl` refuses on a multi-name binding unless you name every binding.** `let (a, b) = pair` is
one declaration; removing "just `a`" would silently unbind `b`. Pass both names.

**`rm open` refuses when the file has unqualified references it cannot attribute.** resq has no type
information, so it cannot prove an `open` is unused. Pass `--force` when you know better. It errs
toward refusing — a spurious refusal costs you a flag, a wrong removal costs you a broken build.

## Known gaps

A few constructs do not parse under the pinned grammar (upstream `tree-sitter-rescript` bugs):
`%replace.type(: T)`, negative bigint `-1n`, two consecutive trailing comments closing a module
block, and local-open sugar (`Types.(expr)`, `Types.{…}`, `Types.[…]`). Read commands degrade
gracefully; write commands refuse to touch such files. A module referenced *only* through local-open
sugar is invisible to `refs`.

`refs` over-reports rather than under-reports: it matches by name without reading module signatures,
and flags shadowed hits as `unqualified-shadowed` rather than dropping them. Before a rename, prefer
a false positive you can dismiss over a missed use. It does **not** follow `include` transitively —
that is its largest gap.
