#!/usr/bin/env bash
# -----------------------------------------------------------------------
# Run iznik-app from the workspace, ready to set up ssh hosts.
#
# Checks that what the build needs is here, then runs `cargo app`, which
# builds the iznik-server that gets installed on ssh hosts (about a second
# when nothing changed) and starts the application. Nothing is built at
# application launch; this is the build step.
#
# Usage:
#   ./scripts/app/run.sh                                   # this machine's architecture
#   ./scripts/app/run.sh --target aarch64-unknown-linux-musl  # other servers too
#
# Every missing piece is named with how to get it, and nothing runs until
# all of them are there. `cargo xtask doctor` checks the whole toolchain.
# -----------------------------------------------------------------------
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MACHINE="$(uname -m)"
TRIPLE="${MACHINE}-unknown-linux-musl"

missing=0
need() {
    # need <description> <how to get it>
    printf 'missing: %s\n    %s\n' "$1" "$2" >&2
    missing=$((missing + 1))
}

command -v cargo >/dev/null 2>&1 \
    || need "cargo" "install rustup: https://rustup.rs"

command -v zig >/dev/null 2>&1 \
    || need "zig (builds the terminal emulator)" "see CONTRIBUTING.md §1, or run: cargo xtask doctor"

command -v "${MACHINE}-linux-musl-gcc" >/dev/null 2>&1 \
    || need "${MACHINE}-linux-musl-gcc (links the server)" \
            "a musl cross-compiler or a shim over \`zig cc -target ${MACHINE}-linux-musl\`; run: cargo xtask doctor"

if command -v rustup >/dev/null 2>&1; then
    if ! (cd "$ROOT" && timeout 60 rustup target list --installed) | grep -qx "$TRIPLE"; then
        need "the ${TRIPLE} Rust target" "run: rustup target add ${TRIPLE}"
    fi
fi

if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ]; then
    need "a display (neither WAYLAND_DISPLAY nor DISPLAY is set)" \
         "run this from a desktop session"
fi

if [ "$missing" -gt 0 ]; then
    printf '%d thing(s) missing; nothing was built.\n' "$missing" >&2
    exit 1
fi

cd "$ROOT"
exec cargo app "$@"
