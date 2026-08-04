# resq

A CLI for querying and editing ReScript files — like `jq` for ReScript.

A port of [elmq](https://github.com/caseyWebb/elmq) to ReScript. Designed as a next-gen LSP **for
agents and scripts, not editors**: token-efficient, structured output, write-safe mutation.

Targets **ReScript 12**.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/m0n01d/resq/main/scripts/install.sh | sh
export PATH="$HOME/.cargo/bin:$PATH"
```

Or from a clone: `cargo install --path .`

## Use

```sh
resq guide                          # full agent-facing reference
resq list src/Main.res              # module summary; nesting by indentation
resq get src/View.res make          # one declaration, decorators + doc comment included
resq grep 'pattern' src/            # regex search annotated with enclosing dot-path
resq refs src/Types.res msg         # every reference, project-wide
resq set decl F.res --name x --content 'let x = 1'
resq patch F.res update --old a --new b
resq rm decl F.res entry            # removes decorators + doc comment too
resq add open F.res Belt            # also: add alias, rm open
```

Declarations are addressed by **dot-path** — `helper`, `Inner.helper`, `Inner.Deep.helper`. A bare
name never matches a nested declaration; there is no implicit search.

## Write safety

Every write refuses a file that already has parse errors, re-parses its own output, and leaves the
file **byte-identical** on any failure. A failed `resq` write never leaves you worse off.

It also refuses on purpose in three places: a sibling `.resi` still declaring the name you're
removing, a multi-name binding where you didn't list every name, and `rm open` when it can't prove
an unqualified reference is unrelated (`--force` overrides). Errors print the fix.

Beyond parsing, `cargo test` runs the **real ReScript compiler** over resq's output — see
`SPEC.md` §7.1.

## Status

v1 covers reads and single-file writes. `mv`, `rename decl`, `move-decl` and the `variant` family
are deferred to v2. Known upstream grammar gaps are listed in `SPEC.md` §0.1.

MIT.
