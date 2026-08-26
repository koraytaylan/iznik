# iznik-cli

Developer plumbing: `probe`, `state`, `tail`, `benchmark`, `doctor` and `uninstall`, each printing one JSON object per line for a person or a script. Never a user interface: no screen, no interactivity.

The binary is `iznik`, a thin dispatcher over this library: its first argument names the subcommand and the module that owns it, and `--help` lists them.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `benchmark` | `iznik benchmark <host>`: the keystroke round trip against a host, printed as a distribution. | `plumbing-commands` (plan 0006) |
| `doctor` | `iznik doctor <host>`: one JSON artifact that says which of five layers is wrong, with secrets redacted by construction. | `diagnostics-bundle` (plan 0006) |
| `output` | The one place the binary writes: one hand-built JSON object per line. | `plumbing-commands` (plan 0006) |
| `probe` | `iznik probe <host>`: the bootstrap's probe of a host, printed. | `plumbing-commands` (plan 0006) |
| `state` | `iznik state <host>`: the host model as the server holds it, printed. | `plumbing-commands` (plan 0006) |
| `tail` | `iznik tail <host> <pane>`: a pane's bytes as they arrive, until a signal. | `plumbing-commands` (plan 0006) |
| `uninstall` | `iznik uninstall <host>`: everything the bootstrap put on a host, removed. | `plumbing-commands` (plan 0006) |

## Tests

Integration tests under `tests/` arrive with the tasks that fill the modules; there is no test module inside `src/`, here or anywhere in the workspace.
