#!/usr/bin/env bash
# -----------------------------------------------------------------------
# Run iznik-app from the workspace, ready to set up ssh hosts.
#
# Checks that what the build needs is here, then runs `cargo app`, which
# builds the iznik-server for every Linux architecture that gets installed on
# ssh hosts (about a second each when nothing changed) and starts the
# application. Nothing is built at application launch; this is the build step.
#
# Both architectures, because a host is whatever it is — an arm64 laptop
# talking to an x86_64 workstation is ordinary — and a server built for the
# wrong one is one the application will refuse to install.
#
# Usage:
#   ./scripts/app/run.sh                                      # both Linux servers
#   ./scripts/app/run.sh --target aarch64-unknown-linux-musl  # just one
#
# Every missing piece is named with how to get it, and nothing runs until
# all of them are there. `cargo xtask doctor` checks the whole toolchain.
# -----------------------------------------------------------------------
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TRIPLES=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)

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

for triple in "${TRIPLES[@]}"; do
    machine="${triple%%-unknown-*}"
    command -v "${machine}-linux-musl-gcc" >/dev/null 2>&1 \
        || need "${machine}-linux-musl-gcc (links the server)" \
                "a musl cross-compiler or a shim over \`zig cc -target ${triple}\`; run: cargo xtask doctor"
done

if command -v rustup >/dev/null 2>&1; then
    installed="$(rustup target list --installed 2>/dev/null || true)"
    for triple in "${TRIPLES[@]}"; do
        printf '%s\n' "$installed" | grep -qx "$triple" \
            || need "the ${triple} Rust target" "run: rustup target add ${triple}"
    done
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
