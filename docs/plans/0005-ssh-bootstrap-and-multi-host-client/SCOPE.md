# Scope — Plan 0005

> Reach any host the user can already reach, put the server there, and hold several hosts at once without letting one bad link touch the others — with a pane keeping its identity and its bytes across every reconnect.

## Why this plan

The daemon from plan 0004 is complete and stranded: it speaks over a unix socket to whatever runs `iznik-server --stdio`. This plan builds the engine that runs it from a Mac — the client library the macOS application links.

The transport decision is settled and worth restating because it constrains everything: iznik shells out to the system `ssh` with `ControlMaster` rather than embedding an SSH library. A user's `~/.ssh/config` is where their `ProxyJump` chain, their `Match` blocks, their bastion, their hardware token and their organization's certificates already live; reimplementing that surface means reimplementing it wrong for the first user whose setup is interesting. Connectivity is not where iznik spends its novelty budget.

Bootstrap follows the pattern every remote-development tool converges on — probe, upload what is missing, verify, launch, never again — and the details that decide whether it feels good are unglamorous: an unwritable home directory, an unexpected architecture, a dropped link mid-upload, a host without `tic`. They are specified as tests here rather than discovered by users. Because the daemon is the sessions, the client never replaces it silently: an upgrade is explicit, and a daemon holding panes refuses it with the count.

Multi-host is a v1 requirement, so identity is global from the first commit. The property to protect: a pane keeps its `iznik://<host>/<pane>` address and its bytes across a host reconnect, because the server persists and the client resumes from the sequence it holds. Get that wrong and every network blip clears someone's scrollback. All of it is proven from the engine container — a machine with no agent, no keys and no iznik — against two host containers over real SSH, through real link drops.

## In scope

- **0020 — SSH Transport.** The `ssh` process with `ControlMaster` and `ControlPersist`, socket path hygiene, the user's configuration honored untouched, classified failures, the `unix:` alias for a local daemon, and one channel carrying `iznik/1` over `iznik-server --stdio` with application-level liveness so a dead link is noticed in seconds, not minutes.
- **0021 — Bootstrap.** The one-round-trip probe with prefix fallback; the `xterm-ghostty` terminfo source; the chunked, verified, atomic upload of the server binary and the terminfo; launch through the relay, the explicit upgrade policy, and a clean uninstall.
- **0022 — Client Model.** The client-side model with subscriptions and cursors and no I/O, the reducer that applies the server's messages with the protocol's reconciler and routes by host, and the optimistic commands — local application of unambiguous commands, pending until confirmed, rolled back on refusal or timeout — all pure, all landed before the manager that calls them.
- **0023 — Multi-Host.** Global identity, a per-host state machine with backoff and jitter tested as a table, and a manager holding several hosts concurrently where one host's failure is invisible to the others.
- **0024 — End to End.** The whole stack from the engine container against two hosts through induced link drops, driven by the regression driver's `manager` step.
- **0025 — Documentation.** What iznik puts on a host and how to take it off, and the crate documentation brought into line.

## Out of scope

The C ABI, the byte-pipe surface a libghostty consumer feeds, and the published client contract are plan 0006. No macOS code is written here or anywhere in this repository. Predictive local echo stays excluded: the sequence numbers keep it possible, and implementing it before a real client can measure it would be guesswork. Interactive authentication prompts are the application's to render; this plan passes an askpass program through and proves the fixture's key-based path.
