# resq — dispatch-ready subagent prompts

Baseline: `main` @ **`dd08c2a`** (wave 0 verified green: build + clippy `-D warnings` + tests).
Repo is local-only — **no remote**. Agents branch from local `main`; the conductor merges locally
and verifies by diff.

---

## SHARED PREAMBLE — prepend verbatim to every agent prompt

> You are working in `/Users/dwight/code/resq`, a Rust CLI that queries and edits ReScript source
> files. It is a port of `caseyWebb/elmq` ("jq for Elm") to ReScript.
>
> **Read `SPEC.md` in full before writing a line of code.** It is ground truth. §1 contains
> empirically verified tree-sitter node shapes — they were confirmed by parsing real ReScript, and
> they correct several things that `node-types.json` states misleadingly. Do not re-derive them and
> do not trust your priors about the grammar over §1.
>
> Set up your branch with exactly this:
> ```bash
> export PATH="$HOME/.cargo/bin:$PATH"
> cd /Users/dwight/code/resq
> git checkout -b <BRANCH> dd08c2a
> ```
> **Sanity-check your base before writing anything:**
> ```bash
> test -f SPEC.md && test -f src/cli.rs && grep -q "let_declaration" SPEC.md || { echo "BAD BASE"; exit 1; }
> ```
> If that fails, STOP and report — do not proceed.
>
> **Files you must NOT edit** (conductor-owned; editing them causes merge conflicts with sibling
> agents): `src/cli.rs`, `Cargo.toml`, `tests/fixtures/**`, `SPEC.md`, `WAVES.md`, `PROMPTS.md`.
> `src/main.rs`: you may replace **only** the single `unimplemented!()` line named for your agent —
> nothing else in that file.
>
> If you need a new dependency, STOP and ask the conductor — do not edit `Cargo.toml`.
>
> If you hit a bug that a sibling branch has already fixed, cherry-pick it with attribution rather
> than working around it.
>
> **Acceptance gate — your work is rejected unless all three pass from the repo root:**
> ```bash
> cargo build && cargo clippy --all-targets -- -D warnings && cargo test
> ```
> Note clippy runs with `-D warnings` and is strict (it rejects `map_or(false, …)` in favour of
> `is_some_and`). Run it yourself before reporting done.
>
> Your tests go in `tests/<your_module>.rs` and must exercise your command against
> `tests/fixtures/proj/`. Do not add fixtures — the shared fixture project already contains nested
> modules, destructuring lets, decorators, JSX, polyvars, unicode, a `.res`/`.resi` pair, and a
> deliberately broken file.
>
> Report back: what you implemented, what you deliberately did not, any place SPEC.md was wrong,
> and the exact output of the acceptance gate.

---

# WAVE 1 — serialized, 1 agent

Everything downstream imports these types. Wrong types here poison all of wave 2 at once, which is
why this is not parallelized.

## A1 — type spine · branch `a1-spine` · model **opus** · isolation worktree

**What.** Implement `src/lib.rs` (shared types), `src/parser.rs` (parsing + error location), and
`src/writer.rs` (atomic write + post-edit validation).

**Why.** Every other agent depends on these. `Declaration` in particular diverges from elmq in two
ways that must be right the first time (`names: Vec<String>`, and `decorators`).

**How.**

1. Read `SPEC.md` §1 (verified node shapes), §2 (write-safety invariant), §3.5–3.7, §3.9.
2. Read elmq's `src/parser.rs` (675 lines) for structure — it carries over directly:
   `https://raw.githubusercontent.com/caseyWebb/elmq/main/src/parser.rs`
3. `src/lib.rs` — define:
   ```rust
   pub struct ModulePath(pub Vec<String>);   // dot-path, SPEC §3.1. Display as "Inner.Deep.helper"
   pub enum BinderKind { Simple, Destructuring }
   pub enum DeclarationKind { Let, Type, Module, External, Include, Open }
   pub struct Declaration {
       pub names: Vec<String>,            // NOT `name` — destructuring binds many (SPEC §3.7)
       pub path: ModulePath,
       pub kind: DeclarationKind,
       pub binder_kind: BinderKind,
       pub decorators: Vec<String>,       // NEW vs elmq (SPEC §3.5) — siblings, not children
       pub type_annotation: Option<String>,
       pub doc_comment: Option<String>,
       pub start_line: usize,             // 1-indexed, INCLUDING decorators and doc comment
       pub end_line: usize,
   }
   pub struct FileSummary { /* module_line-equivalent, opens, aliases, declarations */ }
   ```
   All `Serialize`, with `skip_serializing_if = "Option::is_none"` on the optionals, matching elmq.
4. `src/parser.rs` — `parse`, `first_error_location` (returns 1-indexed `(line, col)` of the first
   ERROR node), `ensure_clean_parse(source, path)`. Plus **one documented predicate**
   `fn is_doc_comment(node, src) -> bool` implementing SPEC §1 finding 2 (`/**` prefix on a
   `block_comment`). Do not scatter that check.
5. **The critical helper** — `fn decl_span_with_attachments(node, src) -> (start_byte, start_line)`:
   walks **backwards** over contiguous preceding siblings collecting `decorator` and doc-comment
   `block_comment` nodes (SPEC §1 finding 1). Everything downstream (`get`, `rm decl`, `move-decl`)
   depends on this being correct. It must stop at the first blank-line gap or non-attachment
   sibling. Test it hard.
6. `src/writer.rs` — `atomic_write` (temp file + rename, same dir) and
   `validate_output(buffer, file, op) -> Result<()>` implementing SPEC §2 step 2 verbatim, with an
   error message naming file, operation, and first-ERROR `line:col`.

**Verification.** Beyond the gate: a test asserting `decl_span_with_attachments` on
`tests/fixtures/proj/src/Main.res` captures `/** Top-level entry point. */` **and** `@genType` for
`entry`; and a test asserting `validate_output` rejects the contents of
`tests/fixtures/broken.res`.

**Ranked hypotheses if the grammar fights you:**
1. You are matching on `let_binding` where the top-level node is `let_declaration` (SPEC §1). This
   is the most likely failure and it fails *silently* by matching nothing.
2. You are looking for decorators/doc comments as children. They are siblings (§1 finding 1).
3. You are matching records via `type_binding`'s `body:` field. Records are an unnamed child;
   only variants use `body:` (§1 finding 4).

---

# WAVE 2 — parallel, 5 agents

**Dispatch gate:** A1 merged; conductor re-ran the acceptance gate itself on the merged diff.
File-conflict scan: all five own disjoint modules and disjoint test files; the shared manifest is
conductor-owned and already pre-merged.

## A2 — `list` · `src/analysis.rs` · branch `a2-list` · **sonnet**

Implement `resq list`. Port elmq's `extract_summary`. **Must render nested modules** with
indentation (SPEC §3.1) and emit full dot-paths under `--format json`. `--docs` includes doc
comments. Read commands are **tolerant**: on a file with ERROR nodes, warn on stderr and print the
summary for the well-formed portions (SPEC §2) — verify against `tests/fixtures/broken.res`.
Group output by kind (`types:`, `functions:`, `modules:`, `externals:`) as elmq does.

## A3 — `get` · `src/extract.rs` · branch `a3-get` · **sonnet**

Implement `resq get`, both the bare positional and the `-f` grouped multi-file form. Resolve
declarations by **dot-path** (SPEC §3.1) — a bare name never matches a nested declaration; ambiguity
is an error, not a guess. Output **must include decorators and the doc comment**, using A1's
`decl_span_with_attachments` — a `get` of `View.res:make` that omits `@react.component` returns code
that does not compile, and that is the primary thing your tests must prove. Exit non-zero when a
requested path is not found.

## A4 — project model · `src/project.rs` · branch `a4-project` · **sonnet**

No user-facing command. Implement project-root discovery (nearest ancestor with `rescript.json`,
falling back to `bsconfig.json`; prefer the former), parse `sources` (honoring `subdirs`),
`namespace`, and `suffix`, and expose a source-file walker built on `ignore`/`walkdir`.
**Do not hardcode `src/`** — read it from config. Also expose `file_to_module_name(path)`
implementing ReScript's basename rule. A7 is blocked on this; keep the public API small and
documented.

## A5 — `grep` · `src/grep.rs` · branch `a5-grep` · **sonnet**

Port elmq's `src/grep.rs` (29KB) — largely mechanical. Source:
`https://raw.githubusercontent.com/caseyWebb/elmq/main/src/grep.rs`. Flags `-F`, `-i`,
`--include-comments`, `--include-strings`, `--definitions`, `--source`, `--format json`. Annotate
each match with its enclosing declaration dot-path. **Exit codes are part of the contract: 0 =
matches found, 1 = none, 2 = error.** By default, matches inside comments and string literals are
excluded — that's what `--include-comments`/`--include-strings` re-enable, and it requires node-kind
awareness, not plain regex.

## A6 — `.resi` interface management · `src/resi.rs` · branch `a6-resi` · **opus**

Implement `resq expose` and `resq unexpose`. **Read SPEC §3.3 carefully — the asymmetry is the whole
task:**
- `expose` with **no** sibling `.resi` → **no-op + advisory on stderr, exit 0** (everything is
  already public).
- `unexpose` with **no** sibling `.resi` → **hard error, exit non-zero**, pointing at
  `rescript-editor-analysis createInterface`. Do **not** synthesize a `.resi` — signatures require
  inferred types that tree-sitter cannot produce, and a wrong `.resi` silently changes the module's
  public API.

Per SPEC §1 finding 5, `.resi` parses with the same nodes as `.res`; an interface item is a
`let_declaration` whose `let_binding` has a `type_annotation` and **no `body:`**.

Also export the API that A9 consumes:
```rust
pub fn sync_check(res_path: &Path, removed: &[String]) -> Result<Vec<String>>
```
returning `.resi` entries that would be orphaned by removing `removed` from the `.res`. Keep this
signature stable — A9 is written against it.

---

# WAVE 3 — parallel, 3 agents

**Dispatch gate:** wave 2 merged; conductor re-ran the acceptance gate itself.

## A7 — `refs` · `src/refs.rs` · branch `a7-refs` · **opus** · depends on A4

**This is the hardest task in the project, and it is NOT a straight port of elmq's `refs.rs`.**
Inline of SPEC §3.2:

> Elm's `import` has no analog. In ReScript every module in the project is globally visible by
> basename; `Foo.bar` needs no declaration at all. elmq's `refs` uses each file's import list to
> skip files that cannot possibly reference the target. **That filter does not exist here.**

So: every source file in the project is a candidate, and you must resolve per-file scope —
`open Foo` (makes `bar` referenceable unqualified), `module F = Foo` (adds `F.bar`), `include Foo`,
and plain qualified `Foo.bar`. Use A4's `project.rs` for the file walk and module naming; honor
`namespace` from config.

Report each reference with file, `line:col`, the enclosing declaration dot-path, and a
classification. Requires a project root; error clearly if there isn't one.

Per SPEC §3.10: `refs` on a **polymorphic variant** must report "unsupported" explicitly — it must
not silently return zero results, which reads as "no references" and is actively dangerous before a
rename.

## A8 — `open`/alias management · `src/imports.rs` · branch `a8-imports` · **sonnet**

Implement `resq add open`, `resq add alias`, `resq rm open`. Ordered insertion near existing opens
at the top of the file. Alias syntax is `<Name>=<Module>` (e.g. `Arr=Belt.Array`) and per SPEC §1 it
parses as a `module_declaration` whose `module_binding.definition` is a `module_identifier_path` —
not a distinct alias node.

**`rm open` is the dangerous one and must be conservative** (SPEC §3.2): removing an `open` can
break unqualified references elsewhere in the file. Before removing, scan the file for identifiers
that would no longer resolve, and refuse with a clear message listing them unless `--force` was
passed. Deleting the line as plain text is wrong.

All three obey the write-safety invariant (SPEC §2) via A1's `writer.rs`.

## A9 — single-file writes · branch `a9-writes` · **opus** · depends on A1, A6

Implement `resq set decl`, `resq patch`, `resq rm decl`. Put handlers in a new `src/edit.rs`.

- `set decl` — upsert at a dot-path; replace if present, append if new. Content from `--content` or
  stdin (exactly one). If the content parses to a name, it must match `--name`.
- `patch` — exact find-and-replace **scoped to the named declaration**, matching exactly once;
  error on zero or multiple matches.
- `rm decl` — remove the declaration **plus its decorators and doc comment**, via A1's
  `decl_span_with_attachments`. Getting this wrong orphans a decorator onto the next declaration and
  silently breaks the file — make it a test. Clean up excess blank lines.
- `rm decl` must call A6's `sync_check()` and refuse (or report) when removal would orphan a `.resi`
  entry (SPEC §3.3).

Every one of these obeys SPEC §2 in full: refuse broken input, re-parse the output buffer, leave the
file byte-for-byte unchanged on failure, print `ok` on success. Test each failure mode against
`tests/fixtures/broken.res` and by feeding deliberately malformed `--content`.

---

## Conductor merge protocol

For every returned branch, in order:

1. `git diff main..<branch> --stat` — confirm only the expected files changed, and that `cli.rs` /
   `Cargo.toml` / fixtures are untouched.
2. Re-run `cargo build && cargo clippy --all-targets -- -D warnings && cargo test` **myself** on the
   merged result. Trust the diff, not the agent's report.
3. Confirm the agent's claimed tests actually exist and actually assert something.
4. Merge to `main`, re-run the gate, then open the next wave's dispatch gate.
