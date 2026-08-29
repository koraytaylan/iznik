# iznik-cli

Developer plumbing: `probe`, `state`, `tail`, `benchmark`, `doctor` and `uninstall`, each printing one JSON object per line for a person or a script. Never a user interface: no screen, no interactivity. The one line that is not an object is the usage line a command answers `--help` with, which is a question about the command rather than an account of a host.

The binary is `iznik`, a thin dispatcher over this library: its first argument names the subcommand and the module that owns it, and `--help` lists them. Every subcommand answers `--help` for itself as well, with the flag anywhere in the line, so that asking what one takes never reaches a host.

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

Integration tests live under `tests/`; there is no test module inside `src/`, here or anywhere in the workspace.
