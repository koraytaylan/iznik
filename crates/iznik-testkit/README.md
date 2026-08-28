# iznik-testkit

The instruments that need the emulator or the server: the golden loader, the headless VT oracle, the pseudoterminal harness, the process metrics every ceiling is measured with, the fidelity corpus, the model generator, the protocol test client and the in-process stack.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `client` | The protocol test client: `iznik/1` over any duplex stream, the client the macOS application will resemble minus the surface. | `test-client` (plan 0004) |
| `corpus` | The fidelity corpus: the constructs that break naive terminal plumbing, each named, and the seeded generator for floods. | `fidelity-corpus` (plan 0001) |
| `generate` | The seeded generator of valid models, delta sequences and registry operations that three crates' tests share. | `deltas-and-reconciler` (plan 0003) |
| `golden` | The one JSONL golden loader every golden test uses, whose error names the file and the line, and the hex the goldens carry bytes in. | `frame-codec` (plan 0001) |
| `metrics` | Resident memory and CPU time of a process from `/proc`, the one implementation behind every ceiling. | `pty-harness` (plan 0001) |
| `pty` | The pseudoterminal harness: real processes on real pseudoterminals, read until quiet rather than until a clock. | `pty-harness` (plan 0001) |
| `stack` | The in-process stack: a daemon under a temporary runtime directory, in process or as a binary, told what a pane runs so a measurement does not depend on the machine, torn down on drop. | `integration-harness` (plan 0004) |
| `vt` | The headless VT oracle over `libghostty-vt`, with deterministic snapshots that can be committed as goldens. | `vt-oracle` (plan 0001) |

## Tests

Integration tests under `tests/` arrive with the tasks that fill the modules; there is no test module inside `src/`, here or anywhere in the workspace.
