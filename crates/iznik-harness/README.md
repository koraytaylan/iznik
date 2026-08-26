# iznik-harness

The bounded process runner, the deadline helpers, the container images, staging, the two-container fixture, and the scenario format, record and runner. No emulator and no product code, so `xtask` builds in seconds on a machine with nothing but a Rust toolchain.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `deadline` | The two deadline helpers every fixture wait is written with: a capped body and a polled condition, each naming what it waited for. | `gate-runner` (plan 0001) |
| `fixture` | The two-container fixture: per-run credentials, readiness under a cap, commands, faults, a measured start time, and orphans made impossible to keep. | `regression-fixture` (plan 0001) |
| `images` | The container images pinned by digest and tagged by content hash, built only when missing. | `regression-images` (plan 0001) |
| `process` | The one place a child process is spawned synchronously: in its own process group, under a deadline that ends the whole group and reports what the child said. | `gate-runner` (plan 0001) |
| `report` | The NDJSON record one scenario step produces. | `scenario-driver` (plan 0001) |
| `runner` | Running a scenario: stage, start the fixture, copy files, run each step in its container, evaluate expectations, enforce the budget, record the overhead. | `scenario-driver` (plan 0001) |
| `scenario` | The scenario format: TOML with a deadline on every step and a budget on the whole, every table denying unknown keys. | `scenario-driver` (plan 0001) |
| `staging` | Staging the three static musl binaries the containers run, keyed by content hash, a no-op when nothing changed. | `regression-fixture` (plan 0001) |

## Tests

Integration tests under `tests/` arrive with the tasks that fill the modules; there is no test module inside `src/`, here or anywhere in the workspace.
