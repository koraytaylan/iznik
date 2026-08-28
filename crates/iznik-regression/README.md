# iznik-regression

The scenario driver that runs inside a container, executes one step under its deadline and reports one NDJSON record, and the test binary that makes every scenario under `regression/scenarios/` a nextest test.

The binary `iznik-regression step` reads one step as TOML on standard input and prints one NDJSON record, and `--help` names that one subcommand — as does `step --help`, which answers rather than waiting on standard input for a step that is not coming. `tests/regression_scenarios.rs` is the `harness = false` test binary that registers one ignored test per scenario file.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `step` | The step dispatcher: one step read as TOML on standard input, executed in its own process group under its deadline, reported as one NDJSON record. | `scenario-driver` (plan 0001) |
| `step::bootstrap` | `[steps.bootstrap]`: bootstrap, upgrade and uninstall against a host, including the older-looking shim. | `remote-launch` (plan 0005) |
| `step::channel` | `[steps.channel]`: the remote channel and its liveness under a dead link. | `remote-channel` (plan 0005) |
| `step::client` | `[steps.client]`: a protocol client over a process that speaks `iznik/1`, with reassembly checked through the oracle. | `integration-harness` (plan 0004) |
| `step::manager` | `[steps.manager]`: a host manager inside the engine container driving several hosts end to end. | `end-to-end-ssh` (plan 0005) |
| `step::pane` | `[steps.pane]`: a pane driven inside the host container, with its screen reproduction checked through the oracle. | `fidelity-suite` (plan 0002) |
| `step::probe` | `[steps.probe]`: the host probe and expectations over its fields. | `host-probe` (plan 0005) |
| `step::run` | The `run` step: a shell command in the container that names it. | `scenario-driver` (plan 0001) |
| `step::transport` | `[steps.transport]`: the SSH transport against the fixture's host, with its classified failures. | `ssh-control-master` (plan 0005) |
| `step::upload` | `[steps.upload]`: the artifact upload and the terminfo compilation on the host. | `payload-upload` (plan 0005) |

## Tests

Integration tests under `tests/` arrive with the tasks that fill the modules; there is no test module inside `src/`, here or anywhere in the workspace. `readme_commands.rs` runs every command this file names with `--help`, so a name here that no binary answers to is a failing test.
