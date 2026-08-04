# resq

Rust CLI for structurally reading/editing ReScript `.res`/`.resi` files (port of elmq). `cargo test` runs a compile gate against a copy of `rescript-hello` — keep that repo compiling.

## Shared conventions

Workspace-wide conventions (language choice, ReScript rules, `resq`, sub-agent orchestration, PR rules) live in the private repo [`m0n01d/claude-conventions`](https://github.com/m0n01d/claude-conventions). On the Mac they auto-load via `~/code/CLAUDE.md`; **a cloud sandbox does not see them** — fetch before starting work:

```sh
gh repo clone m0n01d/claude-conventions /tmp/conventions 2>/dev/null || git clone https://github.com/m0n01d/claude-conventions /tmp/conventions
cat /tmp/conventions/CLAUDE.md
```

If the clone fails (sandbox credentials may be scoped to this repo only), continue with this file — the critical rules for this project are inlined below.
