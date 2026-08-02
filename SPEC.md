# resq — a CLI for querying and editing ReScript files

> Port of [caseyWebb/elmq](https://github.com/caseyWebb/elmq) (`jq for Elm`) to ReScript.
> Same thesis: a next-gen LSP **for agents and scripts, not editors**. Token-efficient,
> structured output, write-safe mutation.

**This file is ground truth for every subagent on this project. Read it before writing a line.**
Where this file and `elmq`'s source disagree, **this file wins** — the divergences are deliberate
and are listed in §3.

---

## 0. Verified facts (do not re-derive, do not assume otherwise)

These were confirmed against live sources on 2026-08-01. If you believe one is wrong, **stop and
report** rather than working around it.

| Fact | Value |
|---|---|
| **Target language version** | **ReScript 12.3.0** — latest stable (13.x is alpha-only). Verified: all 22 v12 constructs in `tests/rescript12_syntax.rs` parse cleanly under the pinned grammar, including `dict{}`, bigint `42n`, regex literals, tagged templates, `@tag`/`@unboxed`, variant spread, async/await, and optional record fields. **Do not target v11 semantics.** |
| Grammar | `rescript-lang/tree-sitter-rescript`, tag **`v6.0.0`**, MIT, actively maintained |
| Grammar on crates.io | **No.** Not published. Use a git dependency. |
| Grammar on npm | **No.** Not published. |
| Rust bindings | **Yes** — `bindings/rust/{lib.rs,build.rs}`, exports `LANGUAGE: LanguageFn` and `NODE_TYPES` |
| Generated files committed | **Yes** — `src/parser.c`, `src/scanner.c`, `src/node-types.json` are all in-tree, so the git dep builds |
| External scanner | C, stateful. Handles significant newlines, nested comments, template-string interpolation, paren depth, `list{`/`dict{`, decorators |
| Named node types | 159 |
| elmq's tree-sitter version | `0.26`; grammar dev-deps `0.25.2` but links via `tree-sitter-language = "0.1"`, so **0.26 is expected to work** — Agent A1 must confirm this empirically as its first act |
| Local toolchain | Node 26, npm 11.12.1, clang 17, git 2.50, rustc 1.97.1 |
| **Real-world corpus** | Validated against 3,181 `.res`/`.resi` files from `rescript-lang/rescript`. On real library code (`packages/`): **260/288 parse clean**, and 26 of the 28 failures are one construct. Excluding it: **99.3%**. See §0.1. |

### 0.1 Known upstream grammar gaps — do NOT try to fix these in resq

Three constructs fail to parse under the pinned grammar. All three are **upstream bugs in
`tree-sitter-rescript`**, encoded as inverted tests in `tests/known_grammar_gaps.rs`.

| Construct | Example | Where it shows up |
|---|---|---|
| `%replace.type` with a bare type payload | `migrate: %replace.type(: Map.t)` | ReScript 12.3 deprecation-migration attribute. 26 files, ~all deprecated `Js_*` shims |
| Negative bigint literal | `-1n` (`42n` and `-1` both parse) | 1 file |
| Two+ consecutive trailing comments closing a module block | `{ let x = 1  /* a */  /* b */ }` | 1 file |

**resq handles these correctly by design and needs no workaround.** The write-safety invariant
(§2) refuses to edit a file that does not parse, so the failure mode is "resq declines to touch
this file", never "resq corrupts it". Read commands warn and continue. An agent that tries to
special-case these in resq is doing the wrong thing.

`%replace.type` is a construct the ReScript team uses to mark stdlib deprecations; it is
essentially absent from application code, which is why the practical clean rate is far above the
raw 90%.

Dependency line:

```toml
tree-sitter-rescript = { git = "https://github.com/rescript-lang/tree-sitter-rescript", tag = "v6.0.0" }
```

---

## 1. Verified grammar node shapes

**Empirically verified** by parsing real ReScript with `tree-sitter 0.26` + grammar `v6.0.0`
(conductor, wave 0). These are actual s-expression outputs, not readings of `node-types.json` —
where the two disagreed, the parse won. **Use these names. Do not guess node kinds — a wrong kind
fails silently by matching nothing.**

```
source_file
  └ let_declaration        ← TOP-LEVEL WRAPPER. NOT `let_binding`.
      └ let_binding        pattern: <25 kinds>  (required, multiple)
                           body:    expression  (absent in .resi signatures)
                           children: type_annotation?
  └ type_declaration
      └ type_binding       name: type_identifier
                           body: (variant_type ...)      ← variants USE the `body:` field
                           <unnamed child> (record_type) ← records DO NOT. Asymmetric. Trap.
                           <unnamed child> (type_identifier) ← alias
                           <name only>                    ← abstract `type t` (.resi)
  └ module_declaration
      └ module_binding     name: module_identifier | type_identifier   (required)
                           definition: (block …)               ← module with a body
                           definition: (module_identifier_path) ← ALIAS `module M = Belt.Array`
                           signature: block | functor | module_expression
  └ external_declaration   children: value_identifier, type_annotation, string
  └ open_statement         child: module_identifier  (`module_expression` is a supertype)
  └ include_statement      child: module_identifier
  └ decorator              children: decorator_identifier, decorator_arguments?
  └ block_comment          ← both `/* … */` AND `/** doc … */`
  └ line_comment           ← `// …`

switch_expression   children: <scrutinee expression>, switch_match+
switch_match        pattern: <pattern kinds>          (required, multiple)
                    <unnamed child> (guard …)         ← optional, sits between pattern and body
                    body: (sequence_expression …)     (required)

function            parameters: (formal_parameters …) ← multi-arg / zero-arg
                    parameter:  (value_identifier)    ← SINGULAR field for `x => …`. Trap.
```

### Findings that bite — read all five

1. **Decorators and doc comments are SIBLINGS of the declaration, not children.**
   `/** Doc */ @genType @react.component let make = …` parses as four sibling nodes:
   `(block_comment) (decorator) (decorator) (let_declaration)`.
   → `get` must walk **backwards** from the declaration collecting contiguous preceding
     `decorator` / `block_comment` siblings. `rm decl` must delete them too, or it orphans a
     decorator onto the next declaration and breaks the file. `move-decl` must carry them.
     This is the single most error-prone part of the port.

2. **There is no doc-comment node.** `/** doc */` and `/* block */` are both `block_comment`.
   Distinguish by **text prefix** (`starts_with("/**")`) on the node's source text. Encode this as
   one documented predicate in `parser.rs` — do not scatter the check.

3. **`%todo` parses cleanly** as `extension_expression`, both bare (`%todo`) and with a message
   (`%todo("later")`). Confirmed available — use it as the stub body (§3.8). No fallback needed.

4. **Records vs variants sit differently under `type_binding`** (see the asymmetry above). Matching
   only on the `body:` field silently misses every record type.

5. **`.resi` files parse with the same nodes** — a signature `let make: (~name: string) => X` is a
   `let_declaration > let_binding` with a `type_annotation` child and **no `body:`**. Abstract
   `type t` is a `type_binding` with only `name:`. So `resi.rs` needs no separate grammar path;
   presence/absence of `body:` is what distinguishes an interface from an implementation.

Also: `let_binding.pattern` is **multiple and required** — destructuring yields several names from
one binding (§3.7). `module_binding.name` may be a `type_identifier`, not just `module_identifier`.

---

## 2. What resq is

Same command philosophy as elmq: read commands are tolerant (warn and continue on parse errors),
write commands are paranoid (refuse broken input, reject broken output, never leave a corrupt file).

### Write-safety invariant (non-negotiable, ported verbatim from elmq's `write-safety` spec)

Every command that mutates a `.res`/`.resi` file MUST:

1. Parse the input file. If `tree.root_node().has_error()`, **abort non-zero before writing any
   bytes**, naming the file and the `line:col` of the first ERROR node.
2. Construct the modified buffer, **re-parse it**, and if the re-parse has an ERROR node, **abort
   non-zero and leave the on-disk file byte-for-byte unchanged**, naming the file, the attempted
   operation, and the first ERROR location in the would-be output.
3. Only then `atomic_write`.

This catches both bad user input and bugs in our own splicing. Multi-file commands validate each
file independently and **may produce partial writes** — files `1..N-1` stay written, file `N` is
untouched, `N+1..` are not processed. **No cross-file transactional staging layer.** Read commands
(`list`, `get`, `grep`, `refs`) are explicitly exempt and keep tolerant behavior.

All write commands print `ok` on success.

---

## 3. Elm → ReScript semantic mapping

This section is the actual intellectual content of the port. **Every divergence below is
deliberate.**

### 3.1 The module model — dot-path addressing (most important decision)

| Elm | ReScript |
|---|---|
| One module per file, **flat** top-level decls | File basename = module name, **plus arbitrarily nested in-file modules** |
| `module Foo exposing (..)` header | No header. `src/Foo.res` *is* module `Foo` |

**Decision:** every declaration is addressed by a **dot-path relative to the file root**:

```
resq get src/Foo.res helper            # top-level
resq get src/Foo.res Inner.helper      # nested one deep
resq get src/Foo.res Inner.Deep.helper # arbitrary depth
```

- A bare name **never** matches a nested declaration. No implicit search — ambiguity is an error.
- `list` renders nesting with indentation and emits the full path in `--format json`.
- Every command taking `<NAME>` takes a **path**. This is uniform; no exceptions.

Rationale: implicit search makes `rm`/`rename` silently hit the wrong declaration in files with
nested modules, which is common in ReScript. Explicit paths cost the agent a few tokens and
eliminate a whole class of destructive mistakes.

### 3.2 Imports → `open` / module aliases / nothing

Elm's `import` has **no direct analog**. In ReScript, every module in the project is globally
visible by basename; `Foo.bar` needs no declaration at all. Access is via:

| Form | Meaning |
|---|---|
| *(nothing)* | `Foo.bar` — always available, requires no statement |
| `open Foo` | brings `bar` into unqualified scope |
| `module F = Foo` | alias, `F.bar` |
| `include Foo` | splices contents into the current module |

**Consequences:**

- `elmq add import` → **`resq add open`** and **`resq add alias`**. There is no "import list".
- `resq rm open` must be conservative: removing an `open` can break unqualified references. It MUST
  scan the file for identifiers that would newly fail to resolve and refuse (or warn loudly) — this
  is not a pure text deletion like Elm's.
- **`refs` cannot consult an import list to narrow candidates.** elmq's `refs` uses imports to skip
  files; ReScript has no such filter, so `refs` must scan every source file in the project and
  resolve `open`/alias scopes per-file. This makes `refs.rs` *harder* than elmq's, not easier.
  Budget accordingly.

### 3.3 `exposing (...)` → `.resi` — **deliberately NOT a command surface**

| Elm | ReScript |
|---|---|
| `module Foo exposing (a, b)` | sibling `Foo.resi` listing signatures |
| `module Foo exposing (..)` | **no `.resi` file at all** (everything public) |

**Decision: resq has no `expose`/`unexpose` commands.** This is a deliberate divergence from elmq,
not an omission. Three reasons:

1. **Elm's exposing list is mandatory; `.resi` is optional and usually absent.** Every Elm module
   header carries an exposing list, so elmq's `expose`/`unexpose` sit on the critical path of every
   file. Most ReScript application modules have no `.resi` at all. Ported literally, the two
   commands would be a no-op on most files and a hard error on most files.
2. **`.resi` parses with the same nodes as `.res`** (§1 finding 5), so `set decl`, `patch`, and
   `rm decl` already operate on a `.resi` file directly. The commands would add no capability —
   only a wrapper around a construct ReScript doesn't have.
3. Materializing a missing `.resi` (elmq's `exposing (..)` auto-expansion analog) requires
   *inferred type signatures*, which tree-sitter cannot compute. Anything we synthesized from syntax
   alone would silently misstate the module's public API.

**What survives is a write-path invariant, not a feature:**

> **`.resi` sync guard.** If a `.res` file has a sibling `.resi`, any command that removes a
> declaration MUST check whether the `.resi` still names it. If so the removal would produce a
> project that does not compile, and the command MUST refuse with a message naming the orphaned
> entries. This has no elmq analog — it is new work, and it belongs inside the write path.

Editing a `.resi` is done the ordinary way: point `set decl` / `rm decl` / `patch` at the `.resi`
file. Read commands work on `.resi` too, for free.

### 3.4 Project config: `elm.json` → `rescript.json` / `bsconfig.json`

Commands needing project scope (`refs`, `mv`, `rename decl`, `move-decl`) require a project root:
the nearest ancestor directory containing **`rescript.json`** (v11+) or **`bsconfig.json`** (v10).
Prefer `rescript.json` when both exist.

Fields that matter: `sources` (which dirs to walk, honoring `subdirs`), `namespace` (changes how
modules are referenced), `suffix`. **Read these from the file — do not hardcode `src/`.**

### 3.5 Decorators — new concept, no Elm analog

`@react.component`, `@genType`, `@deriving(accessors)`, `@val`, `@module("path")`, `@warning("-8")`.

Decorators attach to a declaration and **MUST travel with it** on `get`, `set`, `rm`, and
`move-decl` — structurally the same role Elm doc comments play in elmq's `Declaration` struct, and
they must be handled with the same care. A `get` that drops `@react.component` returns something
that does not compile.

### 3.6 Type annotations are inline, not a separate line

Elm:
```elm
update : Msg -> Model -> Model
update msg model = ...
```

ReScript:
```rescript
let update = (msg: msg, model: model): model => ...
```

There is **no separate annotation line to keep in sync**. This *simplifies* `set decl` (elmq's
sig/body coordination logic largely disappears) but changes `add arg --type`, which must splice the
annotation into the parameter list rather than rewrite a standalone signature line.

`Declaration.type_annotation` remains in the struct but is populated from the `type_annotation`
child node where present, and is `None` for un-annotated bindings.

### 3.7 One binding can bind many names

`let (a, b) = pair` and `let {x, y} = record` produce **multiple names from one `let_binding`**.
elmq's `Declaration { name: String }` cannot represent this.

**Decision:** `Declaration.names: Vec<String>` (not `name: String`), plus
`Declaration.binder_kind: Simple | Destructuring`. `get`/`rm`/`rename` on a destructuring binding:
`get` returns the whole binding; `rm` removes the whole binding (refusing if other bound names are
still referenced); `rename` on one name of a destructuring pattern is **supported** and rewrites
only that binder plus its references.

### 3.8 `case ... of` → `switch { | pat => }`

Node kinds confirmed: `switch_expression` / `switch_match` (§1). Site-key generation, `variant
cases`, and `add variant` branch insertion port directly, modulo node names.

**Stub body:** Elm's `Debug.todo "X"` has no direct equivalent. Use **`%todo`** — **verified in wave
0** to parse cleanly in grammar `v6.0.0` as `extension_expression`, both bare and with a message
(`%todo("Reset")`). No fallback needed.

### 3.9 Declaration kinds

elmq's `DeclarationKind` (`Function | Type | TypeAlias | Port`) becomes:

```rust
enum DeclarationKind { Let, Type, Module, External, Include, Open }
```

`External` replaces `Port`. `Module` and `Include` are new (nesting, §3.1). ReScript does not
distinguish `Type` from `TypeAlias` at the syntax level the way Elm does — one `type_declaration`
covers both; expose the distinction only if `type_binding` makes it cleanly available.

### 3.10 Out of scope for v1 (state explicitly; do not silently skip)

Polymorphic variants (`#foo`), functors, first-class modules (`module_pack`), and JSX-aware
refactoring. These must **parse and round-trip losslessly** — resq must never corrupt a file
containing them — but no command needs to reason about them semantically. `refs` on a polyvar
should report "unsupported", not silently return zero results.

---

## 4. Command surface — v1 scope

Scope decision: **reads + single-file writes.** Project-wide refactors (`mv`, `rename decl`,
`move-decl`, `add/rm variant`) are deferred to v2 and are **not** in any wave below.

### Reads (tolerant)
| Command | Notes |
|---|---|
| `resq list <file>...` | `--docs`, `--format json`. Renders module nesting. |
| `resq get <file> <path>...` | `-f` multi-file form. Includes decorators + doc comment. Non-zero if not found. |
| `resq grep <pattern> [path]` | `-F`, `-i`, `--include-comments`, `--include-strings`, `--definitions`, `--source`, `--format json`. Exit 0/1/2. |
| `resq refs <file> [<path>]` | Requires project root. **Scans all sources** (§3.2). |
| `resq guide` | Prints agent integration doc. |

### Writes (paranoid — all obey §2)
| Command | Notes |
|---|---|
| `resq set decl <file> [--name] [--content\|stdin]` | Upsert at a dot-path. |
| `resq patch <file> <path> --old --new` | Exact match, once, within declaration scope. |
| `resq rm decl <file> <path>...` | Removes decl + decorators + doc comment. Enforces the `.resi` sync guard (§3.3). |
| `resq add open <file> <Module>...` | Ordered insert. |
| `resq add alias <file> <Name>=<Module>` | |
| `resq rm open <file> <Module>...` | Conservative — see §3.2. |

There are deliberately no `expose`/`unexpose` commands — see §3.3. `.res` and `.resi` are both
edited with the commands above.

---

## 5. Module layout

Mirrors elmq so its source can be read side-by-side.

```
resq/
  Cargo.toml              # conductor-owned
  rust-toolchain.toml     # conductor-owned
  src/
    main.rs               # conductor-owned: dispatch only, no logic
    cli.rs                # conductor-owned: ALL clap definitions (see §6 conflict note)
    lib.rs                # A1: shared types — Declaration, DeclarationKind, FileSummary, ModulePath
    parser.rs             # A1: parse, ensure_clean_parse, first_error_location, node-kind constants
    writer.rs             # A1: atomic_write, post-edit re-parse validation
    analysis.rs           # A2: summary extraction (list)
    extract.rs            # A3: declaration extraction by dot-path (get)
    project.rs            # A4: rescript.json discovery, source walking, module graph
    grep.rs               # A5: regex search w/ enclosing-decl annotation
    refs.rs               # A7: reference resolution
    imports.rs            # A8: open/alias management
    edit.rs               # A9: set decl / patch / rm decl, incl. the .resi sync guard (§3.3)
    guide.md              # conductor-owned
  tests/
    fixtures/             # conductor-owned: shared fixture project (see §6)
```

---

## 6. Shared-surface hazards (read before dispatching anything)

Per `code/CLAUDE.md` § sub-agent orchestration — "parallel-safe requires **truly disjoint files**."
The following are shared and are therefore **conductor-owned**. No agent may edit them:

- **`src/cli.rs`** — every command adds a subcommand here. This is the classic shared-manifest
  conflict. The conductor pre-merges **all** clap definitions for the full v1 surface *before* wave 1
  dispatches, each wired to an `unimplemented!()` stub. Agents fill in their handler in their own
  module and never touch `cli.rs`.
- **`src/main.rs`** — dispatch table, same treatment.
- **`Cargo.toml`** — dependency additions get batched by the conductor.
- **`tests/fixtures/`** — a single shared fixture ReScript project. Built by the conductor in wave 0
  so agents don't each invent incompatible fixtures. Agents add test *files* under `tests/` named
  for their module (`tests/analysis.rs`, etc.) — disjoint by construction.

`src/lib.rs` and `src/parser.rs` are the type spine that everything imports. They are **wave 1,
single agent, serialized** — not parallelized — because a wrong shared type poisons every downstream
agent simultaneously.

---

## 7. Verification

Every agent's work is accepted only if, from the repo root:

```bash
cargo build && cargo clippy -- -D warnings && cargo test
```

passes, **and** the agent's own module has tests exercising its command against
`tests/fixtures/`. The conductor re-runs this independently before merging — per house rule,
**trust the diff, not the agent's report.**
