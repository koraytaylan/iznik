# iznik-link

Frames over a duplex stream and streaming compression, written once for both product ends and the test client, so the sentence "the two copies are held to the same corpus" never has to be written.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `compression` | Streaming zstd under a framed link, primed with the protocol's dictionary and engaged from a plain link's parts once both `Hello`s agree. | `stream-compression` (plan 0003) |
| `framed` | Frames over any duplex byte stream: one vectored write per frame out, a resuming decoder in, split halves, and the parts a compression layer is built from. | `framed-link` (plan 0001) |

## Tests

Integration tests under `tests/` arrive with the tasks that fill the modules; there is no test module inside `src/`, here or anywhere in the workspace.
