# Plan 0005 — SSH Bootstrap and Multi-Host Client — 📋 Planned

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** 📋 Planned.

- **Goal:** connect the client engine to remote hosts over the user's own SSH configuration, bootstrap the server and terminfo where they are missing with an explicit upgrade policy, and hold several hosts concurrently behind a client-side model with optimistic commands — proven end to end from a bare engine container against two hosts through link drops.
- **Root cause:** the daemon speaks only to its relay, there is no client-side model in front of it, and multi-host identity cannot be retrofitted after a single-host model has shipped.
- **Approach:** shell out to `ssh` with `ControlMaster` so `~/.ssh/config` keeps working unchanged; probe, upload, verify, launch and never again, each stage driven from the engine container by a step of its own; never replace a daemon silently; key everything on global identity so a pane survives a host reconnect and the client resumes rather than rebuilds; and make every interval a field so a link-drop proof takes seconds.
- **Progress:** 0/13 tasks done; 0 blocked; 0 dropped.
- **Integration:** `planned`; run —; base `develop`; validation base —; mode —; final integration —.
- **Exceptions:** — (coordinator-owned blocked/dropped reasons are recorded here).
- **Outcome:** Several remote hosts held at once over ordinary SSH, each bootstrapped automatically from a machine that had nothing, each surviving link drops without losing pane identity or bytes, with unambiguous commands applying instantly and reconciling against the server.

_Last updated: 2026-08-26, against `develop` @ `2cc3de3`._
