---
id: pty-spawn
title: "PTY Spawn"
workstream: "0006"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-server/src/pty/mod.rs"
  - "crates/iznik-server/src/pty/spawn.rs"
  - "crates/iznik-server/tests/pty_spawn.rs"
  - "regression/claims/pty-spawn.toml"
  - "policy/lexicon/pty-spawn.txt"
status: done
merged_as: ""
---
# PTY Spawn

A pane is a program on a real pseudoterminal in its own session with a controlling terminal — job control, `SIGWINCH`, and Ctrl-C all depend on that — with the environment a ghostty-rendered terminal deserves and an exit status that tells the truth about signal deaths. The product always spawns the login shell; the tests spawn `sh`, because a prompt that draws itself asynchronously is not a fixture.

**Steps:**

1. Implement `crates/iznik-server/src/pty/mod.rs` and `spawn.rs` — `Program`, `SpawnOptions`, `spawn`, `PtyProcess` with kill-on-drop, `ExitStatus`, `Signal`, `PtyError` — exactly as the architecture's `pty-spawn` section specifies, over `portable-pty`, with every environment string a named constant.
2. Write `crates/iznik-server/tests/pty_spawn.rs`, every child `sh` unless the case says otherwise and every read a `read_until_quiet` with a sub-second quiet interval.
3. Declare this task's claims in `regression/claims/pty-spawn.toml` as `test` proofs, each with the `because` that a pseudoterminal is a kernel object identical on the host container and here; the musl binary's behavior is covered by `fidelity-suite`.

**Tests:**

- The login shell: with `Program::LoginShell`, `echo $0` prints a name beginning with `-`, and the shell is the current user's shell from the password database. This is the one test that spawns it.
- Session and controlling terminal: `ps -o sid= -o tty= -p $$` from `sh` shows the child as its own session leader on the pseudoterminal's device.
- Environment: `TERM` is `xterm-ghostty` and `TERMINFO` names the directory when `terminfo_directory` is given, `TERM` is `xterm-256color` and `TERMINFO` is absent when it is not; `COLORTERM` is `truecolor`; `TERM_PROGRAM` is `iznik`.
- Working directory: `pwd` prints the requested directory; a missing directory fails with `WorkingDirectory` naming the path and spawns nothing.
- A program that does not exist fails with `Spawn` naming the path and spawns nothing.
- Resize: after `resize(100, 40)`, `stty size` prints `40 100`.
- Exit statuses: `exit 3` yields `Exited(3)`; a child killed with `Kill` yields `Signalled(Kill)`; a `Hangup` to `sh` ends it.
- Drop kills: dropping a `PtyProcess` whose child is `sleep 30` leaves no such process, asserted by a census.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test pty_spawn` passes every case above, `timeout 900 cargo xtask claims verify --task pty-spawn` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
