# iznik-link

Frames over a duplex stream and streaming compression, written once for both product ends and the test client, so the sentence "the two copies are held to the same corpus" never has to be written.

Compression is negotiated in `Hello` and engaged from a plain link's parts once both ends advertised it. The one subtlety is the leftover: a plain reader reads in blocks, so by the time it has parsed the peer's `Hello` it has usually read some of the peer's first compressed bytes too, and those must be handed to `compressed` rather than dropped. [protocol.md §12](../../docs/notes/protocol.md) states the rule and the two measured numbers the capability is kept for.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `compression` | Streaming zstd under a framed link, primed with the protocol's dictionary and engaged from a plain link's parts once both `Hello`s agree. | `stream-compression` (plan 0003) |
| `framed` | Frames over any duplex byte stream: one vectored write per frame out, a resuming decoder in, split halves, and the parts a compression layer is built from. | `framed-link` (plan 0001) |

## Tests

Integration tests under `tests/` arrive with the tasks that fill the modules; there is no test module inside `src/`, here or anywhere in the workspace.
