# Plan 0005 — SSH Bootstrap and Multi-Host Client — 🚧 In progress

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** 🚧 In progress.

- **Goal:** connect the client engine to remote hosts over the user's own SSH configuration, bootstrap the server and terminfo where they are missing with an explicit upgrade policy, and hold several hosts concurrently behind a client-side model with optimistic commands — proven end to end from a bare engine container against two hosts through link drops.
- **Root cause:** the daemon speaks only to its relay, there is no client-side model in front of it, and multi-host identity cannot be retrofitted after a single-host model has shipped.
- **Approach:** shell out to `ssh` with `ControlMaster` so `~/.ssh/config` keeps working unchanged; probe, upload, verify, launch and never again, each stage driven from the engine container by a step of its own; never replace a daemon silently; key everything on global identity so a pane survives a host reconnect and the client resumes rather than rebuilds; and make every interval a field so a link-drop proof takes seconds.
- **Progress:** 1/13 tasks done; 0 blocked; 0 dropped.
- **Integration:** `planned`; run —; base `develop`; validation base —; mode —; final integration —.
- **Exceptions:** `ssh-control-master` records two. `SshTransport::arguments` is public beyond the architecture's list, because what the vector *lacks* is the property this task exists to hold — every option a person could have written in their own configuration — and asserting an absence needs the value, not a process. `SshTransport::spawn` and `close_master` are not `async`: neither awaits anything, since `tokio::process::Command::spawn` is synchronous and what a caller waits for is the child; making them `async` would have been a signature that promised a suspension point there is none of.

  Its `HostKeyChanged` says "the host key was not accepted" rather than "is not the one remembered", because `ssh` prints `Host key verification failed.` both for a key that changed and for one that was never known under strict checking. The architecture's variants are fixed and both are the host's key rather than the network; the detail carried is `ssh`'s own last line, which says which it was, and the alarming sentence is conditioned on the first.
- **Outcome:** Several remote hosts held at once over ordinary SSH, each bootstrapped automatically from a machine that had nothing, each surviving link drops without losing pane identity or bytes, with unambiguous commands applying instantly and reconciling against the server.

_Last updated: 2026-08-28, against `develop`._
