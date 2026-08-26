---
id: daemon-lifecycle
title: "Daemon Lifecycle"
workstream: "0016"
kind: task
depends_on:
  - client-connections
gated: false
touches:
  - "crates/iznik-server/src/daemon/**"
  - "crates/iznik-server/tests/daemon_lifecycle.rs"
  - "regression/claims/daemon-lifecycle.toml"
  - "regression/scenarios/daemon-lifecycle/**"
  - "policy/lexicon/daemon-lifecycle.txt"
status: planned
merged_as: ""
---
# Daemon Lifecycle

The process that lives on the server. It runs where a locked-down host allows, refuses to run twice, replaces a dead predecessor's socket and respects a live one, detaches from the shell that launched it, goes away when it has nothing to do, and never fills a disk with logs. The property the whole resume story rests on — a daemon started by an SSH command survives the SSH session ending — is proven inside the host container over real SSH.

**Steps:**

1. Implement `crates/iznik-server/src/daemon/` — `RuntimePaths`, `DaemonOptions` with every timing a field, `Lock`, `serve` with the accept loop over `connection::serve`, the `--daemon`, `--foreground`, `--stop` and `--version` entry points, idle shutdown, `logging` — exactly as the architecture's `daemon-lifecycle` section specifies, filling the dispatcher's stubs.
2. Write `crates/iznik-server/tests/daemon_lifecycle.rs` with `env!("CARGO_BIN_EXE_iznik-server")`, `TestClient`, and `--idle-shutdown-seconds 1` wherever idleness is observed.
3. Declare this task's claims in `regression/claims/daemon-lifecycle.toml` and author the scenarios under `regression/scenarios/daemon-lifecycle/` — `survives-the-ssh-session`, `single-instance`, `idle-shutdown` — run from the engine over SSH against the staged binary, each with a budget of 60 seconds or less.

**Tests:**

- Paths: with `XDG_RUNTIME_DIR` set the runtime directory is under it; without it, under `TMPDIR`; the directory is created with mode `0700`.
- Single instance: a second `--foreground` against the same paths fails with `Held` naming the first's process id.
- Socket hygiene: a stale socket file with no lock holder is replaced and the daemon starts; a socket whose daemon is alive is left alone and the second start fails.
- Detach: `--daemon` returns within `socket_ready_cap` with the socket connectable, the daemon has a different session id from its parent, and it is still alive after the parent has exited and after the pseudoterminal the parent was launched on has been closed.
- Idle shutdown: started with `--idle-shutdown-seconds 1`, with no panes and no clients the daemon exits 0 within three seconds, removing its socket and lock; with a pane it does not; the default is `IDLE_SHUTDOWN`, asserted from the parsed options rather than by waiting.
- Logs: after writing past the cap, the log file is rotated and the total on disk is under twice the cap.
- `--version` prints exactly one line with the crate version and the protocol version.
- `--stop` ends a running daemon within the cap and removes its socket and lock; against no daemon it exits non-zero saying so.
- In the container: `ssh host0 /iznik/bin/iznik-server --daemon` returns; after the SSH session has ended, `ssh host0 test -S <socket>` and a process census both find the daemon; a second start over SSH reports the holder; started with `--idle-shutdown-seconds 1`, the daemon exits on its own within the step's deadline.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test daemon_lifecycle` passes every case above, `timeout 900 cargo xtask claims verify --task daemon-lifecycle` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
