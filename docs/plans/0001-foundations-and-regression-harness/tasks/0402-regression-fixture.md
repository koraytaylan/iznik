---
id: regression-fixture
title: "Regression Fixture"
workstream: "0004"
kind: task
depends_on:
  - regression-images
gated: false
touches:
  - "crates/iznik-harness/src/fixture.rs"
  - "crates/iznik-harness/src/staging.rs"
  - "crates/iznik-harness/tests/regression_fixture.rs"
  - "xtask/src/regression.rs"
  - "policy/lexicon/regression-fixture.txt"
status: planned
merged_as: ""
---
# Regression Fixture

The fixture starts the two containers on a private network with credentials that exist only inside them, waits for `sshd` under a cap, measures how long that took, hands tests a way to run commands and inject faults, and tears everything down. Five containers from the previous incarnation were found running sixteen hours after their tests died, so teardown-on-drop is not the promise: podman's own container timeout, an owner label and a reaper on start make an orphan impossible to keep.

**Steps:**

1. Implement `iznik_harness::staging::stage` — the three binaries under `--profile regression --target x86_64-unknown-linux-musl`, the content hash, the `bin/` and `distribution/` layout, the `IZNIK_STAGED` override, `STAGING_DEADLINE` (15 minutes) — and fill the `stage` and `reap` forms of the `xtask regression` subcommand.
2. Implement `crates/iznik-harness/src/fixture.rs` — `Fixture::start`, `FixtureOptions` with its four timing fields and their default constants, `exec`, `fault` with `Process::{Id, IdFile}`, `host_alias`, `elapsed_start`, `FixtureError`, teardown on drop — exactly as the architecture's `regression-fixture` section specifies: a per-run network, `--timeout` and the two labels on every container, the reaper on start, an ed25519 key pair generated inside the engine, host keys generated inside each host, `authorized_keys`, a generated `~/.ssh/config` naming containers by DNS name with `ConnectTimeout 3`, readiness through `wait_until` at `ssh_ready_interval`, and the staging directory mounted read-only at `/iznik`.
3. Write `crates/iznik-harness/tests/regression_fixture.rs`, every test `#[ignore]` and every wait in it under a second except the readiness cap itself.

**Tests:**

- Both containers are reachable: `exec` on the engine and on `host0` each return the container's hostname, running as uid 1000.
- Start time: a test named `fixture_start_latency`, so nextest runs it with the machine to itself, prints `elapsed_start` and asserts it under `FIXTURE_START_CEILING` with warm images.
- Real SSH with generated credentials only: from the engine, `ssh host0 hostname` succeeds with `BatchMode=yes`, and the engine has no agent socket and no key that the fixture did not generate.
- Two hosts: with `hosts = 2`, both aliases resolve by name and the two host containers are distinct.
- Faults: after `DisconnectNetwork { host0 }`, `ssh host0 true` from the engine fails within five seconds; after `ReconnectNetwork`, it succeeds again by name even though the address may have changed. After `PauseProcess` on a process that is printing, its output stops; after `ResumeProcess`, it continues; after `KillProcess` through an `IdFile` the process wrote, it is gone from the census.
- Teardown: after a fixture is dropped normally and after a test body panics, `podman ps -a` and `podman network ls` show nothing with the run's prefix.
- The reaper: a container started by hand with the fixture's labels and a dead owner pid is removed by the next `Fixture::start`; one whose owner is alive is left alone; `xtask regression reap` removes both.
- Readiness cap: with `sshd` deliberately not started and `ssh_ready_cap` set to two seconds, `start` fails with `SshNotReady` naming the alias within the cap, and tears down.
- Staging: `stage` is a no-op the second time, asserted by elapsed time; with `IZNIK_STAGED` naming a directory, no build is run, asserted by a `PATH` without `cargo`.

- **Done when:** `timeout 1200 cargo nextest run --package iznik-harness --test regression_fixture --run-ignored all` passes every case above and `timeout 3600 cargo xtask check` succeeds.
