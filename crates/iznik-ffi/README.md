# iznik-ffi

The C ABI over `iznik-client`, and the one crate in this workspace that may contain `unsafe`: a facade whose every function is safe to call from any thread, whose every callback arrives on one dedicated thread, and whose header is generated and golden-pinned.

**What an application is built against is [`docs/CLIENT.md`](../../docs/CLIENT.md), not this file.** That document is the contract — the threading rules, the ownership rules, the credit protocol, what a reconnection obliges, how query responses are answered, what the marks carry, and a worked example — and where it and this implementation disagree, it wins and this is the bug. A test holds the two together: every name in the contract is in `include/iznik.h` and the reverse, and every obligation the header states is in the contract in the same words.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `error` | `iznik_error`: a code, the layer it came from, and a message the application may display verbatim. | `ffi-surface` (plan 0006) |
| `model` | The events carried to the application in `iznik/1`'s own encoding, so there is one format, not two. | `ffi-surface` (plan 0006) |
| `pane` | The pane byte pipe: attach, detach, the output callback that hands bytes straight to a surface, mandatory credit, input that is one message per call, resize and focus. | `pane-byte-pipe` (plan 0006) |
| `shape` | What turns one event of iznik's own into one of the application's: its kind, its host, its bytes, and the numbers that say which pane, place, generation and command. This crate's own, not part of the ABI. | `ffi-surface` (plan 0006) |

## The header

`include/iznik.h` is generated from this crate by `cargo xtask header` and committed. A signature that changes changes the header in the same commit, where a reviewer sees it; a case regenerates it and compares byte for byte, another edits one signature in a copy of the tree to be sure the comparing works, and a third compiles the header alone as C11 under `-Wall -Wextra -Werror` and again as C++.

## Tests

Integration tests under `tests/` arrive with the tasks that fill the modules; there is no test module inside `src/`, here or anywhere in the workspace. `ffi_surface.rs` and `pane_byte_pipe.rs` call the `extern "C"` functions the way a C program does, against a daemon on this machine through a `unix:` alias — which is why this crate is built as an `rlib` beside the `cdylib` and `staticlib` an application links. `xtask/tests/fixtures/ffi/smoke.c` is the C program itself, compiled against the generated header and the static archive and run against a real daemon.

## What the events say

Snapshots and changes are the host's own account of itself, decoded with the protocol's own reader — never a change this client is showing ahead of the host. An application that wants one on the screen before the host has agreed to it applies that change itself and puts it back when the `CommandResult` for its number says the host refused; `iznik_client` keeps a model of its own for the programs written against it in Rust, and what crosses this boundary is the host's.

## Ownership

Every buffer iznik passes to a callback is valid for that callback and no longer; an application that needs one afterwards copies it while it has it. Every buffer an application passes in is read before the call returns, so it may be freed as soon as it does. An error's message is iznik's, and stays valid until the next call on the same thread. There is one exception to the copying rule and it is the pane output path, where the bytes go straight into a surface.

A context is not a buffer: what an application passes to `iznik_set_event_callback` or `iznik_pane_attach` is held until it says otherwise, and iznik never copies or frees it. Saying otherwise is `iznik_pane_detach`, another `iznik_pane_attach` over the same pane, a new event callback, or `iznik_client_free` — and each of those waits for a callback that is already running before it answers, so the moment one returns the context may be freed. The one call an application may not make from inside a callback is `iznik_client_free`, which waits for the thread the callback is on.
