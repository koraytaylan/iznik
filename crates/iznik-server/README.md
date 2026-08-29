# iznik-server

The remote daemon: pseudoterminal ownership, terminal mirrors, history, sessions, multiplexing and resume. Asynchronous end to end; the one module with blocking I/O is `pty::streams`, whose purpose is to hide it.

The binary `iznik-server` takes one of `--stdio`, `--daemon`, `--foreground`, `--stop` and `--version` as its first argument and hands the command line to the module that owns it; `--help` lists them, and each of them answers `--help` with what it takes. The four the `daemon` module owns share two flags: `--idle-shutdown-seconds`, which shortens the interval after which a daemon with no panes and no clients exits, and `--program`, which says what a pane runs rather than leaving it to whoever's login shell is on the machine. `tests/readme_commands.rs` asks every command this file names, so a name here that no binary answers to is a failing test.

What it costs is measured, not asserted in prose: `benches/baseline.rs` prints the table in [baseline.md](../../docs/notes/baseline.md), and `tests/regression_baseline.rs` includes that same file so one definition of each figure is both printed and held to a ceiling.

What this daemon speaks is written out in [protocol.md](../../docs/notes/protocol.md): every discriminant, every byte layout, and the subscribe, resume, credit and compression rules a second implementation needs.

How `libghostty-vt` 0.2.1 behaves under the mirror — the query routing, why the observer parses OSC itself, that `max_scrollback` is a byte budget, and what the formatter reproduces exactly and where its reconstruction has edges — is recorded, each with the test that established it, in [terminal-mirror.md](../../docs/notes/terminal-mirror.md).

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `connection` | One accepted stream: the handshake, the dispatch of every control message, and the single writer that owns the order of frames on the wire. | `client-connections` (plan 0004) |
| `daemon` | The daemon: runtime paths, the single-instance lock, the accept loop, idle shutdown, logging, and the `--daemon`, `--foreground`, `--stop` and `--version` entry points. | `daemon-lifecycle` (plan 0004) |
| `daemon::idle` | The idle interval after which a daemon with no panes and no clients exits. | `daemon-lifecycle` (plan 0004) |
| `daemon::lock` | The exclusive non-blocking lock that enforces a single daemon instance and names the holder. | `daemon-lifecycle` (plan 0004) |
| `daemon::logging` | A size-capped rotating log file behind `tracing-subscriber`, because a full disk is worse than no logs. | `daemon-lifecycle` (plan 0004) |
| `daemon::socket` | The unix socket the daemon listens on: a stale file removed and rebound once the lock is held. | `daemon-lifecycle` (plan 0004) |
| `history` | Per-pane history rings indexed by absolute sequence, and the shared budget that evicts from the least recently focused pane first. | `history-ring` (plan 0002) |
| `history::ring` | The ring itself: append, range and copy by absolute sequence, without allocating per chunk in steady state. | `history-ring` (plan 0002) |
| `multiplexer` | One multiplexer per client connection: the pump that carries every subscribed pane over one link under credit windows. | `multiplexer-assembly` (plan 0003) |
| `multiplexer::channel` | The channel table that hands out pane channels and holds a released one until the client acknowledges it, and the per-channel cursor. | `channel-multiplexer` (plan 0003) |
| `multiplexer::credit` | Credit windows in bytes: the focused pane's larger window, refills, consumption, and the stale threshold. | `channel-multiplexer` (plan 0003) |
| `multiplexer::scheduler` | One scheduling round: the focused cursor first, then round-robin, a cursor at zero credit skipped, a lagging background cursor marked stale. | `multiplexer-assembly` (plan 0003) |
| `pane` | The pane: pseudoterminal, mirror, history ring and mark observer held together by one VT task, behind one interface. | `pane-assembly` (plan 0002) |
| `pty` | Pseudoterminal ownership: spawning the login shell in its own session and turning its blocking descriptor into async streams. | `pty-spawn` (plan 0002) |
| `pty::spawn` | Opening a pseudoterminal pair and spawning the program in its own session, with the environment a pane runs under and faithful exit statuses. | `pty-spawn` (plan 0002) |
| `pty::streams` | The one module where blocking I/O exists: dedicated threads that turn the pseudoterminal descriptor into an async output stream and an input queue. | `pty-streams` (plan 0002) |
| `relay` | `iznik-server --stdio`: the bridge between the standard streams and the daemon's socket, starting the daemon on first use. | `stdio-relay` (plan 0004) |
| `resume` | The one place that decides what a subscription starts with, contiguous bytes from the ring or the screen as truth, as a pure function over what the ring holds. | `resume-and-replay` (plan 0003) |
| `session` | The session registry: the authoritative host model, the panes behind it, and the numbered deltas every change emits. | `session-registry` (plan 0003) |
| `session::commands` | Applying a session command to the registry: validation against the model first, then the operation, answered exactly once. | `session-commands` (plan 0003) |
| `session::registry` | The registry's operations and their delta order, the ingestion of pane marks, sizes and exits, and the debug-only validation after every operation. | `session-registry` (plan 0003) |
| `terminal` | The terminal mirror thread and the emulator every pane's bytes are fed into. | `terminal-mirror` (plan 0002) |
| `terminal::marks` | The shell-integration observer: OSC 133, OSC 7, titles and alternate-screen switches recognized as bytes pass, across any chunk boundary, without touching them. | `shell-integration-marks` (plan 0002) |
| `terminal::mirror` | The mirror thread's `LocalSet`, the `libghostty-vt` terminal behind each pane, and the policy that answers a program's queries only while nobody is subscribed. | `terminal-mirror` (plan 0002) |
| `terminal::screen` | The screen serializer: the mirror's state as VT bytes exact at a sequence, the primary screen remembered across an alternate-screen switch, and the size cap. | `screen-serializer` (plan 0002) |

## Tests

Integration tests live under `tests/`; there is no test module inside `src/`, here or anywhere in the workspace. Two of them are not ordinary: `regression_baseline.rs` includes `benches/baseline.rs` and is ignored by default, because every case builds and measures a real daemon; `readme_commands.rs` runs every command the READMEs name with `--help`.
