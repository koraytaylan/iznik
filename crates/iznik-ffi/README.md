# iznik-ffi

The C ABI over `iznik-client`, the one crate that may contain `unsafe`: a single-threaded facade whose every callback arrives on one dedicated thread, whose every function is safe from any thread, and whose header is generated and golden-pinned.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `error` | `iznik_error`: a code, the layer it came from, and a message the application may display verbatim. | `ffi-surface` (plan 0006) |
| `model` | The events carried to the application in `iznik/1`'s own encoding, so there is one format, not two. | `ffi-surface` (plan 0006) |
| `pane` | The pane byte pipe: attach, detach, the output callback that hands bytes straight to a surface, mandatory credit, input, resize and focus. | `pane-byte-pipe` (plan 0006) |

## Tests

Integration tests under `tests/` arrive with the tasks that fill the modules; there is no test module inside `src/`, here or anywhere in the workspace.
