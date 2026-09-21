---
id: in-place-replacement
title: "Replace the daemon in place, and leave it alive when that fails"
workstream: "0001"
kind: task
depends_on:
  - adoption-boundary
  - adopted-state
gated: false
touches:
  - crates/iznik-server/src/daemon/adopt.rs
  - crates/iznik-server/src/daemon/mod.rs
  - crates/iznik-server/tests/daemon_lifecycle.rs
  - crates/iznik-client/src/bootstrap/mod.rs
  - policy/lexicon/in-place-replacement.txt
  - regression/claims/in-place-replacement.toml
  - regression/scenarios/remote-launch/upgrade-keeps-sessions.toml
status: planned
merged_as: ""
---
# Replace the daemon in place, and leave it alive when that fails

An `execv` rather than a stop and a start, so there is no window in which two
daemons hold the same masters and no window in which nobody does. A failed
`execv` leaves the calling process alive to say so; an adoption that refuses
rolls back to the staged old binary.

**Steps:**

1. `crates/iznik-server/src/daemon/adopt.rs`: `replace(paths, state, options,
   deadline) -> Result<(), AdoptError>` — stop accepting, drain connections,
   write `AdoptedState`, clear `CLOEXEC` on the masters, the listener and the
   lock, and `execv` the staged binary with `--adopt <state> <listener-fd>
   <lock-fd>`.
2. `crates/iznik-server/src/daemon/mod.rs`: the `--adopt` entry point, which
   takes the inherited listener and lock rather than acquiring them, rebuilds
   the registry from the state file and the inherited masters, and refuses with
   words and a status when the state does not decode, is another version, or
   names a master that is not open.
3. Keep the replaced binary as `<prefix>/bin/iznik-server.previous` until the
   new one answers `--version` correctly; on an adoption refusal, put it back
   and re-execute it.
4. `crates/iznik-client/src/bootstrap/mod.rs`: an upgrade that keeps sessions
   stages the new binary, asks the running daemon to adopt it over its own
   socket, and verifies the version that answers afterwards.
5. Write the tests: the daemon lifecycle case for `execv` failure leaving the
   daemon alive, an adoption refusal rolling back, and the scenario for a
   real host whose sessions outlive the replacement.
6. Declare the claims in `regression/claims/in-place-replacement.toml`.

**Tests:**

- An `execv` that cannot run the staged binary leaves the old daemon serving.
- An adoption that refuses leaves the previous binary in place and the host
  reaching its old sessions; a retry is possible.
- A real host's pane keeps its child, its sequence and its screen across the
  replacement, and a client reconnecting resumes byte-exact.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test daemon_lifecycle` passes the in-process cases, and `timeout 1800 cargo nextest run --package iznik-regression --test regression_scenarios --run-ignored all -E 'test(=scenario::in-place-replacement::upgrade-keeps-sessions)'` passes the container case.
