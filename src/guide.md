# resq — agent integration guide

`resq` queries and edits ReScript files structurally. It exists so an agent can read and modify
ReScript without loading whole files into context or hand-splicing text.

Targets **ReScript 12**. Requires `rescript.json` (or `bsconfig.json`) for project-wide commands.

## The two rules that carry most of the value

1. **Prefer `resq get <file> <path>` over reading the file.** It returns exactly one declaration —
   with its decorators and doc comment — in a fraction of the tokens.
2. **Prefer `resq set decl` / `patch` / `rm decl` over rewriting the file.** Every write is
   validated and fails closed, so a bad edit costs you nothing: the file is left byte-identical.

When resq refuses, **read the error** — it names the corrective command. Reaching for `--force`
without reading it is how you break a build.

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

## Trailing comments travel with the declaration

A declaration owns the comments that follow it on its own last line. This is a `//` comment, or a
`/* */` comment that is not a `/**` doc comment. These commands respect it:

- `get` includes it.
- `patch` can edit text inside it.
- `set decl` replaces it along with the declaration.
- `rm decl` and `rm open` remove it, and leave no orphan comment behind.

A comment on the next line belongs to no declaration. A `/** doc */` comment right after a
declaration, even on the same line, belongs to the next declaration. It never belongs to the one
before it.

A `;` right after a declaration, on the same line, belongs to the declaration too. Any comment
after that `;`, on the same line, still belongs to the declaration.

## `set decl --before` / `--after`: insert next to an anchor

By default, `set decl` adds a new declaration at the end of its module. Pass `--before <path>` or
`--after <path>` to put it next to an existing declaration instead.

ReScript needs a name defined before its use. If an earlier declaration already uses the new name,
pass `--before` with that declaration as the anchor:

```sh
resq set decl src/Main.res --name helper --content 'let helper = x => x * 2' --before main
```

This adds `helper` right before `main`. The anchor must be in the same module as the new name.
`--before` keeps the anchor's own decorators and doc comment attached to the anchor. `--after`
keeps the anchor's own trailing comment attached to the anchor, and inserts past it.

When `--name` already exists in the module, both flags refuse. This check looks at every name the
new content binds, not only `--name` itself. `()` and `_` bind no name, so they are exempt. Plain
`set decl` without them replaces an existing declaration in place, so `--before` and `--after`
only add a new one.

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

**A whole-pattern `let () = …` or `let _ = …` has a real address.** The address is the literal
text `()` or `_`. Quote it in the shell. Inside a module, address it the same way: `Inner.()`. Pass
`--before` or `--after` on `set decl` to always add a new one of these. Omit both, and the command
refuses when the module already has one. Pass `--name '()'` (or `--name '_'`) instead, to replace
the existing one.

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

Anonymous bindings add a few more gaps:

- Two `let () = …` bindings in one module are ambiguous, and so are two `let _ = …` bindings.
  `get`, `patch`, `rm decl`, and `set decl --name` all refuse them. Use a text edit instead. `set
  decl --before`/`--after` can still add a third past the ambiguous pair.
- A pattern with no name, other than the whole-pattern `()` or `_`, still has no address. `let
  (_, _) = pair` is one example.
- A top-level expression that is not a binding has no address. `main()` on its own line is one
  example.
- `refs` on `()` or `_` returns nothing at all, not even its own definition. `grep --definitions`
  is different: it returns one definition row, keyed by the literal text `()` or `_`.
