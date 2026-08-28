# iznik-ffi

The C ABI over `iznik-client`, the one crate that may contain `unsafe`: a single-threaded facade whose every callback arrives on one dedicated thread, whose every function is safe from any thread, and whose header is generated and golden-pinned.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `error` | `iznik_error`: a code, the layer it came from, and a message the application may display verbatim. | `ffi-surface` (plan 0006) |
| `model` | The events carried to the application in `iznik/1`'s own encoding, so there is one format, not two. | `ffi-surface` (plan 0006) |
| `pane` | The pane byte pipe: attach, detach, the output callback that hands bytes straight to a surface, mandatory credit, input that is one message per call, resize and focus. | `pane-byte-pipe` (plan 0006) |

## Tests

Integration tests under `tests/` arrive with the tasks that fill the modules; there is no test module inside `src/`, here or anywhere in the workspace. `ffi_surface.rs` calls the `extern "C"` functions the way a C program does, against a daemon on this machine through a `unix:` alias — which is why this crate is built as an `rlib` beside the `cdylib` and `staticlib` an application links.

## Ownership

Every buffer iznik passes to a callback is valid for that callback and no longer; an application that needs one afterwards copies it while it has it. Every buffer an application passes in is read before the call returns, so it may be freed as soon as it does. An error's message is iznik's, and stays valid until the next call on the same thread. There is one exception to the copying rule and it is the pane output path, where the bytes go straight into a surface.
