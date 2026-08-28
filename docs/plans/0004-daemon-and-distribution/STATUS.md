# Plan 0004 — Daemon and Distribution — 🚧 In progress

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** 🚧 In progress.

- **Goal:** land the `iznik-server` daemon — paths, lock, socket, detach, idle shutdown, logging, multi-client connections, and the `--stdio` relay — the integration harness every later test uses, a committed performance baseline, and reproducible static artifacts for both Linux targets.
- **Root cause:** a session that lives only as long as a connection is a session a network blip destroys; only a process that outlives every client, reachable through a relay that starts it, makes reconnect the whole cost of a drop.
- **Approach:** a test client that speaks the real protocol over any duplex stream, landed first so nothing is hand-rolled twice; a connection loop proven over a socket pair before the daemon that accepts for it; a daemon indifferent to attachment, proven to survive the SSH session that started it inside the host container; a relay that is the one thing the bootstrap ever runs; an in-process stack so no test guesses a binary's path; and numbers, committed with the machine described.
- **Progress:** 1/9 tasks done; 0 blocked; 0 dropped.
- **Integration:** `planned`; run —; base `develop`; validation base —; mode —; final integration —.
- **Exceptions:** `test-client` records one: `TestClient::snapshot_request` is not in the architecture's list for that section. It is the send half of `snapshot`, and the golden sweep needs it — `snapshot` waits for the reply, while a fixture line is one frame and not a conversation; `snapshot` is written in terms of it.

  `client-connections` inherits an obligation from plan 0003's `multiplexer-assembly` review, recorded there and in `regression/claims/multiplexer-assembly.toml`: five fixes on paths an in-memory sink cannot reach — a delta returning to its queue rather than vanishing, a mark staying the only copy of itself, a subscription that fails before announcing its channel freeing the number outright, a pane that cannot answer detaching one pane instead of the connection, and a forgotten pane's queued marks going with it. A real link can refuse a frame; that is where each must be proven.
- **Outcome:** A daemon that survives disconnection and the shell that launched it, serves several clients at once with per-client focus and clean teardown, is reached through `iznik-server --stdio` over real SSH from the engine container, has committed numbers for latency, throughput, memory and startup, and ships as one reproducible static binary per Linux target.

_Last updated: 2026-08-26, against `develop` @ `2cc3de3`._
