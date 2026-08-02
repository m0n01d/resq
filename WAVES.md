# resq — subagent dispatch plan

Conductor-owned. Agents read `SPEC.md`; the conductor drives this file.

Discipline checklist is from `code/CLAUDE.md` § "Sub-agent orchestration". **Tick every item at
dispatch time, not retro time.**

---

## Wave 0 — conductor only (no agents) — ✅ COMPLETE @ `dd08c2a`

**Outcome: GO.** `tree-sitter 0.26` links cleanly against grammar `v6.0.0` — the foundational risk
is retired, no version pinning needed. rustc 1.97.1 installed. Build + clippy `-D warnings` + tests
all green. The empirical grammar probe corrected five things in `SPEC.md` §1 that would each have
cost an agent significant time; A1's discovery work is therefore already done, and A1 is now
implementation-only.


Rationale: house rule — *"Rebase the conductor branch on latest main; confirm build + tests pass
(catches harness rot early)."* There is no harness yet, so building one is prerequisite work, and
building it with agents would mean 5 agents racing on `Cargo.toml` and `cli.rs`.

- [ ] `rustup` install (**blocked — no cargo/rustc on this machine; needs user go-ahead**)
- [ ] `git init`, `cargo init --name resq`
- [ ] `Cargo.toml` — full dependency set, mirroring elmq: `anyhow`, `clap` (derive), `ignore`,
      `regex`, `serde`, `serde_json`, `thiserror`, `tree-sitter = "0.26"`, `walkdir`,
      `tree-sitter-rescript = { git = "...", tag = "v6.0.0" }`; dev-dep `tempfile`
- [ ] **Smoke test the foundational risk**: parse a non-trivial `.res` file and assert
      `!tree.root_node().has_error()`. This is the go/no-go for the whole project — it proves
      `tree-sitter 0.26` links against the grammar's `tree-sitter-language 0.1`. If this fails,
      **nothing else dispatches** until it's resolved (pin `tree-sitter` down to `0.25`).
- [ ] `src/cli.rs` — **all** v1 clap definitions (§4 of SPEC), every handler `unimplemented!()`.
      Pre-merging the shared manifest is the ternpike lesson: agents never touch this file.
- [ ] `src/main.rs` — dispatch table, stubs only
- [ ] `tests/fixtures/` — one shared fixture ReScript project. Must contain, at minimum:
      nested modules 2 deep, a destructuring `let`, `@react.component` and `@genType` decorators,
      a `switch` with a guard, an `external`, an `open`, a module alias, a doc comment, a
      polymorphic variant, JSX, a unicode string literal, **and** a matched `.res`/`.resi` pair.
      Plus `rescript.json`. Plus one deliberately-broken file for write-safety tests.
- [ ] Confirm `cargo build && cargo clippy -- -D warnings && cargo test` is green **before** wave 1

---

## Wave 1 — type spine (1 agent, serialized)

**Why serialized:** `lib.rs` + `parser.rs` are imported by every downstream module. A wrong shared
type poisons all of wave 2 simultaneously. This is the one place parallelism is actively harmful.

### A1 — `parser.rs`, `lib.rs`, `writer.rs` · model: **opus** · isolation: worktree

Architectural judgment (type design under an unfamiliar grammar) — opus per the model-matching rule.

**Deliverables**
1. `src/lib.rs` — `Declaration { names: Vec<String>, path: ModulePath, kind: DeclarationKind,
   binder_kind, decorators: Vec<String>, type_annotation: Option<String>, doc_comment: Option<String>,
   start_line, end_line }`, `DeclarationKind`, `FileSummary`, `ModulePath` (dot-path, §3.1).
   Note the deliberate divergences from elmq: `names` is a `Vec` (§3.7) and `decorators` is new (§3.5).
2. `src/parser.rs` — `parse`, `first_error_location`, `ensure_clean_parse`. Port from
   [elmq's `src/parser.rs`](https://github.com/caseyWebb/elmq/blob/main/src/parser.rs) (675 lines) —
   read it first, the structure carries over directly.
3. `src/writer.rs` — `atomic_write` + post-edit re-parse validation implementing §2 exactly.
4. ~~Empirical grammar findings~~ — **done by the conductor in wave 0.** All findings are folded
   into `SPEC.md` §1 (doc-comment node kind, `%todo`, `type_binding` shapes, nested module
   reachability, `let_declaration` wrapper, decorator sibling placement). A1 implements against §1
   and does not re-derive it.

**Verification**: `cargo build && cargo clippy -- -D warnings && cargo test`, plus a test asserting
every fixture file in `tests/fixtures/` parses without ERROR nodes — **including** the polyvar, JSX,
and unicode files (§3.10 round-trip requirement).

**Ranked hypotheses if the grammar fights you** (highest-leverage prompt content per house rule):
1. Version skew — `tree-sitter 0.26` vs the grammar's `0.25` dev-dep. Symptom: link error or
   `set_language` returning `LanguageError`. Fix: pin `tree-sitter = "0.25"` in `Cargo.toml`,
   report to conductor.
2. External-scanner state — the C scanner tracks paren depth and newline significance. Symptom:
   correct-looking files fail to parse only when embedded in a larger buffer. Fix: always parse
   whole files, never fragments, except through a dedicated fragment-validation helper.
3. Node-name drift between `v6.0.0` and `main`. Symptom: queries match nothing. Fix: re-read
   `node-types.json` **at the pinned tag**, not `main`.

---

## Wave 2 — reads (4 agents, parallel)

**Dispatch gate:** A1 merged, `SPEC-ADDENDA.md` exists, conductor has re-run the test suite itself.
**Prompts for this wave are drafted but NOT final** — each must be updated with A1's verified node
names before dispatch. Dispatching them with guessed node kinds is the predictable failure mode.

File-conflict scan: all four own disjoint modules and disjoint test files. `cli.rs`/`main.rs`/
`Cargo.toml` are conductor-owned, so the shared-manifest hazard is already neutralized.

| Agent | Module | Command | Model | Notes |
|---|---|---|---|---|
| A2 | `analysis.rs` | `list` | sonnet | Must render **nested** modules (§3.1). Port elmq `extract_summary`. |
| A3 | `extract.rs` | `get` | sonnet | Dot-path resolution; decorators + doc comment must travel (§3.5). |
| A4 | `project.rs` | *(no command)* | sonnet | `rescript.json`/`bsconfig.json` discovery, `sources`/`namespace`/`suffix`, source walking. Blocks A7. |
| A5 | `grep.rs` | `grep` | sonnet | Port elmq `src/grep.rs` (29KB). Largely mechanical — regex + enclosing-decl annotation. Exit codes 0/1/2. |

**A6 was cut.** `expose`/`unexpose` do not survive the port — SPEC §3.3. The `.resi` sync guard it
was going to export moved into A9's write path, which also removes wave 3's only cross-agent
dependency.

---

## Wave 3 — refs + writes (3 agents, parallel)

**Dispatch gate:** wave 2 merged, conductor test run green.

| Agent | Module | Command | Model | Depends on |
|---|---|---|---|---|
| A7 | `refs.rs` | `refs` | **opus** | A4 |
| A8 | `imports.rs` | `add open`, `add alias`, `rm open` | sonnet | A1 |
| A9 | `edit.rs` | `set decl`, `patch`, `rm decl` | opus | A1 |

With A6 cut, these three are fully independent of each other.

**A7 is the hardest task in the project and must be prompted as such.** SPEC §3.2: unlike elmq,
`refs` gets **no import list to prune candidate files** — ReScript modules are globally visible, so
every source file is a candidate and per-file `open`/alias scope must be resolved. Do not let an
agent assume this is a straight port of elmq's `refs.rs`; call the divergence out explicitly in the
prompt with the §3.2 text inlined.

**A9** owns the `.resi` sync guard directly (§3.3): `rm decl` must refuse when removal would orphan
an entry in a sibling `.resi`.

---

## Deferred to v2 (explicitly out of scope — not silently dropped)

`mv`, `rename decl`, `move-decl`, `add variant`, `rm variant`, `variant cases`, `set let`,
`set case`, `rm let`, `rm case`, `rm arg`, `rename let`, `rename arg`, `add arg`.

These are where elmq's mass actually is (`variant.rs` is 94KB, `move_decl.rs` 48KB — together ~38%
of the codebase). Sizing v2 needs the v1 read model validated first, per the user's scope decision.

---

## Pre-dispatch checklist (per wave)

From `code/CLAUDE.md`, run down this list before **every** wave:

1. [ ] Conductor branch current; `cargo build && cargo test` green
2. [ ] Each failure reproduced by the conductor once, actual error text pasted into the prompt
3. [ ] Model matched to scope (sonnet mechanical / opus architectural)
4. [ ] Wave grouped by **file-conflict surface**, not by suspected root cause
5. [ ] Exact `git checkout -b X origin/Y` snippet in each prompt — never prose
6. [ ] Each agent sanity-checks its base (`test -f src/parser.rs`) before writing; STOP on failure
7. [ ] "If a sibling branch already fixed a bug you hit, cherry-pick with attribution"
8. [ ] Push verified by SHA comparison against `origin/<branch>`
9. [ ] Conductor re-runs tests on the merged diff — **trust the diff, not the agent's report**
