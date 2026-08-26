# iznik-protocol

Framing, the `iznik/1` control messages, the session model, deltas and the reconciler. Pure: no I/O, no clock, no dependencies, and every encoding pinned by a golden fixture that is the contract.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `capabilities` | The capability bit set exchanged in `Hello`, whose unknown bits are preserved rather than dropped. | `control-messages` (plan 0001) |
| `command` | The session commands, their outcomes and rejection codes, encoded as the `Command` and `CommandResult` payloads. | `session-command-codec` (plan 0003) |
| `delta` | The numbered deltas, each the smallest thing that can happen to the host model, encoded as the `Delta` payload. | `deltas-and-reconciler` (plan 0003) |
| `dictionary` | The committed zstd dictionary trained on the fidelity corpus, which primes the compressed link from its first frame. | `stream-compression` (plan 0003) |
| `frame` | The frame codec: a little-endian payload length, a channel byte and the payload, with a decoder that resumes across any read boundary. | `frame-codec` (plan 0001) |
| `identity` | The `Copy` newtypes every message names a thing by: pane, tab, session and command ids, generations and byte sequences. | `control-messages` (plan 0001) |
| `message` | The control messages on channel 0 in both directions, the error codes and the mark kinds, and the rule that pane output on every other channel is never parsed. | `control-messages` (plan 0001) |
| `model` | The host model: sessions holding ordered tabs holding a normalized layout tree of panes, its invariants, and the `Snapshot` payload encoding. | `model-types` (plan 0003) |
| `reconcile` | Applying a numbered delta to a host model: exactly the next generation, every invariant checked before the first mutation. | `deltas-and-reconciler` (plan 0003) |

## Tests

Integration tests under `tests/` arrive with the tasks that fill the modules; there is no test module inside `src/`, here or anywhere in the workspace.
