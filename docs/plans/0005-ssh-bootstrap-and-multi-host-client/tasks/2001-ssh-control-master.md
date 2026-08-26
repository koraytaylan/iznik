---
id: ssh-control-master
title: "SSH Control Master"
workstream: "0020"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-client/src/transport/mod.rs"
  - "crates/iznik-client/src/transport/ssh.rs"
  - "crates/iznik-client/tests/ssh_control_master.rs"
  - "crates/iznik-regression/src/step/transport.rs"
  - "regression/claims/ssh-control-master.toml"
  - "regression/scenarios/ssh-control-master/**"
  - "policy/lexicon/ssh-control-master.txt"
status: planned
merged_as: ""
---
# SSH Control Master

The system `ssh`, with the user's configuration authoritative and iznik adding only the options it owns: a persistent master so the second command is fast, keepalives so a dead link is noticed, and a classification of failures so a person is told which host, which stage, and what went wrong — a changed host key most of all. The `unix:` alias is decided here too, so that everything local reaches a daemon without SSH.

**Steps:**

1. Implement `crates/iznik-client/src/transport/mod.rs` and `ssh.rs` — `ClientRuntimePaths`, `Transport` with `for_alias`, `SshOptions` with every timing a field, `SshTransport`, `SshChild`, `close_master`, `classify`, `SshError` and the named constants — exactly as the architecture's `ssh-control-master` section specifies.
2. Implement `crates/iznik-regression/src/step/transport.rs` — `[steps.transport]` with `alias`, `command`, `connect_timeout_seconds`, `expect`, `check_master` and `close_master`.
3. Write `crates/iznik-client/tests/ssh_control_master.rs` for the pure parts, and the scenarios under `regression/scenarios/ssh-control-master/` — `connects`, `reuses-the-master`, `unreachable` (an alias whose `HostName` is an address on the fixture's network that nothing answers, with `connect_timeout_seconds = 2`), `authentication-refused` (an alias whose `IdentityFile` is a second key the scenario generates), `host-key-changed` (a step that rewrites `host0`'s entry in `known_hosts`) — run from the engine, each with a budget under a minute.
4. Declare this task's claims in `regression/claims/ssh-control-master.toml`.

**Tests:**

- Argument purity: the constructed argument vector contains the alias, the owned options with the values from `SshOptions`, and the command, and none of `User`, `Port`, `IdentityFile`, `ProxyJump`, `HostName` or any `-l`, `-p`, `-i`, `-J` flag.
- `for_alias`: `unix:/run/iznik/server.sock` is `Local` with that path; any other alias is `Ssh`.
- Control path: two aliases produce distinct control paths, each under 104 bytes.
- Classification: captured `ssh` outputs for a refused connection, an unresolvable name, a denied key, a changed host key and a failing remote command each classify to the expected variant with the host and the detail.
- In the container: a command over `host0` succeeds; a second command is faster than the first and `ssh -O check` reports a live master; `close_master` ends it. The unanswered address fails as `Unreachable` within the two-second timeout; the wrong key fails as `AuthenticationFailed`; the rewritten `known_hosts` entry fails as `HostKeyChanged`.

- **Done when:** `timeout 600 cargo nextest run --package iznik-client --test ssh_control_master` passes every pure case, `timeout 900 cargo xtask claims verify --task ssh-control-master` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
