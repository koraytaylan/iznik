---
id: adoption-boundary
title: "Adopt an inherited pseudoterminal master without breaking the unsafe rule"
workstream: "0001"
kind: task
depends_on: []
gated: false
touches:
  - crates/iznik-ffi/src/lib.rs
  - crates/iznik-server/Cargo.toml
  - crates/iznik-server/src/pty/spawn.rs
  - crates/iznik-server/tests/pty_spawn.rs
  - policy/lexicon/adoption-boundary.txt
  - regression/claims/adoption-boundary.toml
status: planned
merged_as: ""
---
# Adopt an inherited pseudoterminal master without breaking the unsafe rule

A pane is a pseudoterminal master descriptor the daemon opened. Preserving the
pane across a server replacement means the new incarnation owns that
descriptor without having opened it, which is an operation whose safety rests
on the caller's obligation — the one thing the workspace's unsafe rule exists
to keep in `iznik-ffi`.

**Steps:**

1. Decide the boundary, and record it: a safe `pty_adopt` in `iznik-ffi` used
   by the server through a thin wrapper (preferred), or an amendment to
   `AGENTS.md` §3.7 naming a second boundary with its reason. The plan's
   default is the first.
2. `crates/iznik-server/src/pty/spawn.rs`: `PtyProcess::adopt(master: RawFd,
   process_id: u32) -> Result<PtyProcess, PtyError>`, building the
   `portable-pty` master over the inherited descriptor through the boundary.
   The inherited descriptor is cleared of `CLOEXEC` by the caller that keeps it.
3. Write the tests in `crates/iznik-server/tests/pty_spawn.rs`: a descriptor
   opened here and adopted behaves as one that was spawned — it resizes, it
   writes and reads, and a child made over it is observed — and an adopted
   descriptor that is not open is refused rather than used.
4. Declare the claims in `regression/claims/adoption-boundary.toml`.

**Tests:**

- An adopted master resizes its terminal and its child sees the new size.
- A descriptor number that is not open is refused by name, not used.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test pty_spawn` passes every case above, `timeout 900 cargo xtask claims verify --task adoption-boundary` reports every claim proven.
