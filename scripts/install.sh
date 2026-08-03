#!/usr/bin/env sh
# Install resq. Safe to run repeatedly — exits early if already present.
#
# Works in two situations:
#   * inside a resq clone            -> builds from the working tree
#   * anywhere else                  -> builds from the private GitHub repo (needs git auth)
#
# Designed for a fresh Claude Code cloud sandbox, where nothing is installed yet.
#
#   curl -fsSL https://raw.githubusercontent.com/m0n01d/resq/main/scripts/install.sh | sh
#
# (that URL needs auth while the repo is private — see "Remote use" in the README)

set -eu

REPO="${RESQ_REPO:-https://github.com/m0n01d/resq}"
CARGO_BIN="$HOME/.cargo/bin"

log() { printf '  %s\n' "$*" >&2; }

# Already installed and on PATH? Nothing to do.
if command -v resq >/dev/null 2>&1; then
    log "resq already installed: $(command -v resq)"
    resq --version 2>/dev/null || true
    exit 0
fi

# Installed but not on PATH — the usual case right after a rustup install.
if [ -x "$CARGO_BIN/resq" ]; then
    log "resq present at $CARGO_BIN/resq but not on PATH"
    log "add this to your shell profile:"
    log '  export PATH="$HOME/.cargo/bin:$PATH"'
    exit 0
fi

# Rust toolchain.
if ! command -v cargo >/dev/null 2>&1; then
    if [ -x "$CARGO_BIN/cargo" ]; then
        PATH="$CARGO_BIN:$PATH"
        export PATH
    else
        log "no Rust toolchain found — installing rustup (this takes a minute)"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
        PATH="$CARGO_BIN:$PATH"
        export PATH
    fi
fi

# Build. Prefer the working tree when we are inside a clone — faster, and it picks up local edits.
if [ -f "Cargo.toml" ] && grep -q '^name = "resq"' Cargo.toml 2>/dev/null; then
    log "building from the current working tree"
    cargo install --path . --locked
elif [ -f "../Cargo.toml" ] && grep -q '^name = "resq"' ../Cargo.toml 2>/dev/null; then
    log "building from the parent working tree"
    cargo install --path .. --locked
else
    log "building from $REPO"
    log "(private repo: this fails without git credentials that can read it)"
    cargo install --git "$REPO" --locked
fi

if [ -x "$CARGO_BIN/resq" ]; then
    log "installed: $CARGO_BIN/resq"
    log 'if `resq` is not found, add: export PATH="$HOME/.cargo/bin:$PATH"'
else
    log "install finished but $CARGO_BIN/resq is missing — check the cargo output above"
    exit 1
fi
