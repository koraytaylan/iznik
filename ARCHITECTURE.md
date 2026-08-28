# Architecture

> iznik is a remote terminal system with a native macOS front end. This
> document is the design every plan under `docs/plans/` implements. Where a
> plan and this document disagree, this document is stale and the plan's
> documentation task fixes it — the code is never allowed to be the only
> record of a decision.

## 1. What iznik is

A native macOS terminal application connects to a remote host over the user's
own SSH configuration, installs `iznik-server` there if it is missing, and
attaches. Every pane is a real pseudoterminal on the remote host, rendered on
the Mac by a libghostty surface fed the pane's raw bytes. Sessions, tabs and
panes live in the server, so a dropped link, a closed laptop or a restarted
application costs a reconnect and nothing else.

This repository holds everything except the macOS application: the server,
the client engine the application links, the wire protocol between them, the
C ABI the application calls, and the test harness that proves all of it. The
application is built in its own repository against the contract that plan 0006
publishes.

### Non-goals

- **No terminal user interface.** There is exactly one user interface and it
  is the macOS application. Every `iznik` command prints structured text for a
  person or a script and never draws a screen.
- **No layout engine on the server.** The server records how a tab is
  arranged so that a reconnecting client can restore it; it never computes a
  cell size. The client owns geometry.
- **No second backend.** There is one server and one protocol. A capability
  tier for "the host has tmux but not iznik" is how a product grows three
  half-working paths; iznik installs its server or reports why it could not.

## 2. Topology

```text
macOS application
  └─ iznik-ffi (C ABI)
       └─ iznik-client ─── ssh <host> iznik-server --stdio ───┐
                                                              │ unix socket
                                                     iznik-server daemon
                                                       ├─ session registry
                                                       ├─ multiplexer (per client)
                                                       └─ panes
                                                            ├─ pseudoterminal + child
                                                            ├─ terminal mirror (libghostty-vt)
                                                            ├─ history ring
                                                            └─ shell-integration observer
```

One SSH channel per host carries every pane on that host. The `--stdio` relay
on the remote side is a thin bridge between the SSH channel and the daemon's
unix socket; the daemon treats it as one more local client. Nothing in the
daemon knows what SSH is.

## 3. Crates

| Crate | Kind | Role | Depends on |
|---|---|---|---|
| `iznik-protocol` | library | Framing, `iznik/1` messages, the session model, deltas and the reconciler. Pure: no I/O, no clock, no dependencies. | — |
| `iznik-link` | library | Frames over a duplex stream and streaming compression, written once for both ends and the test client. | `iznik-protocol` |
| `iznik-server` | library + binary `iznik-server` | The remote daemon: pseudoterminal ownership, terminal mirrors, history, sessions, multiplexing, resume. | `iznik-protocol`, `iznik-link` |
| `iznik-client` | library | The client engine: SSH transport, bootstrap, the client-side model and reducer, optimistic commands, multi-host management. | `iznik-protocol`, `iznik-link` |
| `iznik-ffi` | `cdylib` + `staticlib` | The C ABI over `iznik-client`. The only crate that may contain `unsafe`. | `iznik-client`, `iznik-protocol` |
| `iznik-cli` | binary `iznik` | Developer plumbing: `probe`, `state`, `tail`, `benchmark`, `doctor`, `uninstall`. Never a user interface. | `iznik-client`, `iznik-protocol` |
| `iznik-harness` | library | The bounded process runner, the deadline helpers, the two-container fixture, staging, and the scenario format and runner. No emulator and no product code, so `xtask` builds in seconds. | — |
| `iznik-testkit` | library | The golden loader, the headless VT oracle, the pseudoterminal harness, the fidelity corpus, the model generator, the protocol test client and the in-process stack. | `iznik-protocol`, `iznik-link`, `iznik-server`, `iznik-harness` |
| `iznik-regression` | library + binary `iznik-regression` | The scenario driver that runs inside a container and reports NDJSON, and the test binary that makes every scenario a nextest test. | `iznik-harness`, `iznik-testkit`, `iznik-server`, `iznik-client`, `iznik-protocol` |
| `xtask` | library + binary `xtask` | Gates, policy checks, the claims registry, images, staging, distribution, the header, the soak. | `iznik-harness` |

Dependencies point one way: protocol ← link ← server/client ← ffi/cli, and
the test crates sit beside them. `iznik-protocol` has no dependencies at all
because both ends of every link and the C ABI carry its encoding; a crate that
everything trusts stays small enough to read in an afternoon. `iznik-harness`
and `xtask` never depend on the emulator, because the tool whose job is to say
that Zig is missing cannot itself need Zig to compile.

Every crate root includes its own `README.md` as crate documentation
(`#![doc = include_str!("../README.md")]`), so the file a person opens on
GitHub and the page `cargo doc` renders are one text.

## 4. The `iznik/1` protocol

Spoken between `iznik-client` and `iznik-server` over one duplex byte stream —
an SSH channel, a unix socket, or an in-memory pipe in tests. Defined in
`iznik-protocol`, pinned by golden fixtures, and hand-encoded in little-endian
so a wrong byte is a failing test rather than a corrupted terminal.

This section says what the protocol is for. **Every discriminant, byte layout
and rule a second implementation needs is in
[protocol.md](docs/notes/protocol.md)**, which the macOS repository reads
instead of the Rust, and which a test holds to the source constant by
constant.

### 4.1 Framing

A frame is a 4-byte little-endian payload length, a 1-byte **channel**, and
the payload. Payloads are at most 1 MiB; a larger length is a protocol error,
not an allocation. Channel `0` carries structured control messages. Channels
`1..=255` carry pane output as **raw bytes**: the payload *is* the terminal
data and is never deserialized, re-encoded or copied on the way past. This is
the one performance decision the whole system rests on.

### 4.2 Control messages

Client to server:

| Message | Purpose |
|---|---|
| `Hello { protocol_version, client_version, capabilities }` | First message on every connection. |
| `SnapshotRequest` | Ask for the complete host model. |
| `Command { command_id, payload }` | A session command (§5.3), answered by `CommandResult`. |
| `Subscribe { pane }`, `Unsubscribe { pane }` | Begin or end delivery of a pane's output. |
| `Resume { pane, from_sequence }` | Subscribe and continue from a byte position the client already holds. |
| `ScreenRequest { pane }` | Ask for the pane's current screen as VT bytes. |
| `Credit { channel, bytes }` | Return flow-control credit for a pane channel. |
| `ChannelReleased { channel }` | Acknowledge a `PaneDetached`, allowing the channel number to be reused. |
| `Input { pane, bytes }` | Keystrokes, paste, and the client emulator's own query responses. |
| `Resize { pane, columns, rows }` | Set a pane's size. |
| `Focus { pane }` | Which pane this client is looking at, for scheduling priority. |
| `Ping` | Liveness. |

Server to client:

| Message | Purpose |
|---|---|
| `Hello { protocol_version, server_version, capabilities }` | Handshake reply. |
| `Snapshot { generation, payload }` | The complete host model. |
| `Delta { generation, payload }` | One change to the model, numbered. |
| `CommandResult { command_id, payload }` | `Applied { generation, created }` or `Rejected { code, message }`. |
| `PaneChannel { pane, channel, sequence }` | The channel a subscribed pane's output flows on, and the byte position the stream starts at. |
| `PaneDetached { pane, channel }` | Output for the pane has stopped on that channel. |
| `Screen { pane, sequence, columns, rows, bytes }` | The pane's screen and scrollback as VT bytes, exact at `sequence`. |
| `Mark { pane, sequence, kind }` | A shell-integration event, fully typed: prompt, command start, command end with status, working directory, title, alternate-screen switch. |
| `Pong`, `Error { code, message }` | Liveness and refusal. |

Capabilities are a bit set exchanged in `Hello`; unknown bits are preserved,
not dropped, so a newer peer round-trips its own advertisement intact. The
protocol version is a value the handshake refuses on mismatch; the codec never
guesses across versions.

### 4.3 Sequence numbers and resume

Every pane has one absolute byte sequence counting from its creation. A
`PaneChannel` names the position its stream starts at, and output frames on
that channel are contiguous from there, so the client always knows exactly
which byte it holds. A reconnecting client sends `Resume { from_sequence }`;
if the server's history ring still covers that position it continues exactly,
and if it does not the server sends a `Screen` — the terminal's true state at
a named sequence — and continues from that. **`Screen` is the only
resynchronization mechanism.** There is no "desync" marker for the client to
interpret: whenever the server cannot deliver contiguous bytes, it delivers
truth instead.

### 4.4 Geometry

The client owns size. It sends `Resize`, the server sets the pseudoterminal
and its mirror, and the resulting `PaneResized` delta is what every client —
including the sender — renders. When two clients look at one pane the last
`Resize` wins and both observe it; the contract makes that policy explicit
rather than pretending a pseudoterminal can have two sizes.

## 5. The server

### 5.1 Anatomy of a pane

A pane is four things held together by one event stream:

- **A pseudoterminal and its child.** Spawned in its own session with a
  controlling terminal, the user's login shell, a working directory, and
  `TERM=xterm-ghostty` when the bootstrap installed the terminfo (else
  `xterm-256color`). Exit status is reported faithfully, signal deaths
  included. Output is read on an async task and never blocks anything.
- **A terminal mirror** — a `libghostty-vt` terminal fed every byte, the same
  engine the macOS application renders with. It exists so the server can
  answer "what does this pane look like right now" without replaying history:
  the `Screen` message is the mirror's state formatted as VT sequences, and
  because both ends run the same emulator, applying it to a fresh surface
  reproduces the mirror exactly. When a program is in the alternate screen,
  the serialized screen is the primary screen as it was at the moment of the
  switch, the switch itself, then the live alternate screen — the pane's byte
  scanner recognizes the switch and the primary is serialized before it is
  fed — so leaving the alternate screen on the client reveals what it reveals
  on the server.
- **A history ring** of the last N bytes indexed by absolute sequence, so a
  hot reconnect resumes without a repaint. Bounded per pane and in total;
  eviction takes from the least recently focused pane first.
- **A shell-integration observer** that recognizes OSC 133 prompt marks, OSC 7
  working-directory reports and title changes as bytes pass, never modifying,
  delaying or reordering them, and emits `Mark` events. It handles sequences
  split across reads, which is the bug every naive implementation of this has.

**The mirror thread.** A `libghostty-vt` terminal is `!Send`, so every mirror
lives on one dedicated thread running a `LocalSet`, where each pane's VT task
appends to the ring, feeds the mirror and observes marks; the rest of the
daemon asks that thread for a screen or a resize over a channel and never
touches a terminal. One thread suffices: the emulator parses faster than any
link delivers, and chunk-sized task polls keep a flooding pane from starving
its neighbors' mirrors.

**Query responses.** Programs query their terminal — cursor position, device
attributes, colors. The mirror sees every query: the binding delivers the
responses it composes itself through `on_pty_write` and asks the embedder to
answer device attributes, XTVERSION, ENQ, size reports and color-scheme
queries through effects of their own. The mirror answers all of them only
when **no client is subscribed** to the pane, so a program running unattended is not
left waiting; a subscribed client's emulator answers through `Input`, because
only the real terminal knows its real colors and capabilities. When several
clients subscribe to one pane, each answers; the contract documents that.

**Confirmed, not assumed.** How `libghostty-vt` 0.2.1 actually behaves — which
queries it answers through `on_pty_write` and which through dedicated effects,
that its `osc::Parser` panics on untrusted input so the observer parses OSC
itself, that `max_scrollback` is a byte budget rather than a line count, that
dimensions must be read live because a program can resize itself, and what the
`Format::Vt` formatter reproduces exactly and where its reconstruction has edges
— is recorded, each with the test that established it, in
[terminal-mirror.md](docs/notes/terminal-mirror.md).

### 5.2 Sessions

The server holds a **host model**: sessions, each holding ordered tabs, each
holding a layout tree of panes. Identity is stable and position is derived:
every id is minted once by the server, never reused, and `index` is a field.
The layout tree is `Split { direction, children with integer weights }` or
`Leaf(pane)` — enough to restore an arrangement, deliberately not enough to
compute a cell size. It is kept normalized, and the last of those rules is
load-bearing: a split's weights are divided by the factor they share, without
which the product a flattening multiplies through accumulates until it
saturates and the same arrangement is two unequal trees. Focus is per client
and is not model state.

Every change to the model is a numbered delta emitted directly by the
operation that caused it; the model's generation advances by one per delta. A
client that misses a generation asks for a snapshot rather than guessing, and
the property the client relies on — a snapshot followed by deltas converges on
the next snapshot — is tested by fuzzing in `iznik-protocol`.

### 5.3 Session commands

`CreateSession`, `RenameSession`, `CloseSession`, `CreateTab`, `RenameTab`,
`CloseTab`, `ReorderTabs`, `CreatePane { tab, placement, columns, rows,
working_directory }`, `ClosePane`, `MovePane`, `SetLayout { tab, layout }`.
Every command names stable identity, is validated against the model's
invariants before anything is spawned, and is answered exactly once. A pane
always runs the user's login shell; when its child exits the pane is removed
and the delta carries the exit status.

### 5.4 Multiplexing and scheduling

One connection carries every pane a client subscribes to, so without
arbitration a `cat` of a large file starves the pane the user is typing into.
The multiplexer is a scheduler: each pane channel has a credit window in
bytes that the client refills as it consumes — 256 KiB for a background pane,
1 MiB for the focused one, and at most 64 KiB in any single frame, so a
keystroke echo waits behind at most one frame per active pane. The focused
pane is served first; the rest are served round-robin from a moving place. A
channel at zero credit is skipped, never waited on. A background pane that
falls more than 4 MiB behind is marked stale: the server stops streaming it,
keeps its history, and sends a `Screen` when the client returns to it. It
buffers no pane bytes of its own — the history ring *is* the queue, and a
subscription is a cursor into it.

Acceptance is a measured latency figure under a flood, not an adjective: with
one pane flooding at line rate and another echoing keystrokes, both
subscribed, a thousand keystroke-to-echo round trips report a 99th percentile
under **25 ms**, the figure past which a person stops feeling a terminal as
immediate. The same case asserts that at least a mebibyte of the flood was
carried alongside them, so the number cannot be earned by an idle link.

### 5.5 Daemon lifecycle

The daemon listens on `$XDG_RUNTIME_DIR/iznik/server.sock`, falling back to
`$TMPDIR/iznik-<user_id>/server.sock`, enforces a single instance with an
exclusive lock on a sibling file rather than by socket existence, detaches
from the shell that launched it, exits after a configurable idle interval with
no panes and no clients, and logs to a size-capped rotating file.
`iznik-server --stdio` connects to the daemon, starting it if needed, and
relays bytes between its standard streams and the socket.

What landed says all of that in five entry points — `--stdio`, `--daemon`,
`--foreground`, `--stop`, `--version` — each of which answers `--help`, and in
two flags every measurement and every test uses: `--idle-shutdown-seconds`,
which shortens the idle interval so a test can watch a daemon go, and
`--program`, which says what a pane runs so a figure does not depend on whose
login shell was on the machine.

Three details are worth writing down because each was a bug before it was a
rule. The lock is asked *who holds it* through a second, shared-mode open, so
a refusal names a process id rather than a path; a start that meets that probe
mid-flight asks again after a pause rather than failing. Releasing unlinks
before dropping, so the file a later daemon flocks is never one already gone.
And the log is written through the runtime — a `tracing-subscriber` layer
formats each event into a line and one task owns the file — because §3.6
forbids this crate the standard library's blocking streams, which is what that
crate's own file writer wants.

The relay is asymmetric, and has to be. A client's end of input is a
half-close: the write half of the socket is shut down and whatever the daemon
answers is still delivered. The daemon's end is the relay's end. Copying both
directions and waiting for both to finish — which is what `copy_bidirectional`
does — hangs on a dead daemon while a parked read of standard input never
returns.

The daemon *is* the sessions: replacing its binary ends them. The client
therefore never upgrades a server silently. A protocol-version mismatch is
reported; a newer bundled server is offered as an upgrade the application
must ask for explicitly, and a daemon with live panes refuses it with the
count. Hot upgrade by descriptor passing is deferred, not forgotten.

### 5.6 Compression

Streaming zstd over the whole connection, negotiated in `Hello`, with a
dictionary trained on a committed corpus of real terminal output so the first
kilobytes of a session are not the expensive ones. One context per connection,
not per frame, so the window spans frames; a flush after every frame, so
nothing waits in a buffer for a frame that may never come.

Committed numbers decide whether it stays on. Against the fidelity corpus the
378-byte dictionary takes 316 bytes of payload to 101, a ratio of **3.129**
against the 3.0 the capability is kept for; as the link actually frames that
corpus — seventeen frames, a flush after each — it costs 299 bytes against
411, a saving of **1.375**. Both are asserted together so neither can be
quoted alone. The latency cost is **740 ns** added at the 99th percentile for
a one-byte frame in process, against a 1 ms ceiling.

The handshake's one subtlety is the leftover: by the time a plain reader has
parsed the peer's `Hello` it has usually read some of the peer's first
compressed bytes too, and those bytes must be handed to the compressed stream
as its first input rather than dropped.

## 6. The client engine

### 6.1 Transport

`iznik-client` shells out to the system `ssh` with `ControlMaster` and
`ControlPersist` and a control path under its own runtime directory. The
user's `~/.ssh/config` is authoritative: `ProxyJump`, `Match` blocks, agents,
certificates and bastions keep working because iznik never reimplements them.
Failures are classified — unreachable, authentication refused, host key
changed, remote command failed — because "connection failed" tells a person
nothing and a changed host key demands an alarming, specific message.

A host alias of the form `unix:<path>` names a daemon socket on this machine
and is reached with no SSH at all. It is what every in-process test, the C
smoke program and the plumbing commands use, and it is the one alias form
iznik interprets itself; everything else is handed to `ssh` untouched.

### 6.2 Bootstrap

Probe in one round trip (`uname`, installed server version, writable prefix,
runtime directory, presence of `tic`), upload the server binary and the
`xterm-ghostty` terminfo source over the same channel with SHA-256
verification and an atomic rename, launch or adopt the daemon, negotiate. A
matching version skips the upload, which is the common case and must be
fast. `iznik uninstall <host>` removes everything it put there.

### 6.3 Model, reducer and optimistic commands

The client holds one host model per host, applies the server's deltas with the
same reconciler the protocol crate tests, and routes by `HostId` first. A
command whose local effect is unambiguous — rename, close, reorder, focus — is
applied locally at once, recorded as pending, and confirmed or rolled back by
the authoritative delta. Creation waits one round trip, because inventing a
placeholder id to reconcile later is more flicker than waiting.

### 6.4 Multi-host

`HostId` is the user's SSH alias. A pane is addressed globally as
`iznik://<host>/<pane>`, and it keeps that address across a reconnect because
the server persists and the client reconciles rather than rebuilding. Each
host has its own connection state machine with backoff and jitter; one host's
failure, slowness or bootstrap is invisible to the others.

## 7. The C ABI

`iznik-ffi` exposes a single-threaded facade: every callback arrives on one
dedicated thread, never concurrently, never re-entrantly with respect to a
call the application is making; every `iznik_*` function is safe from any
thread, and a call from inside a callback is permitted. Buffers handed to a
callback are valid for the callback's duration; buffers passed in are copied.
Pane output is delivered by callback straight into the application's surface,
and flow control is mandatory: the application returns credit as its surface
consumes. The header is generated and golden-pinned, so an accidental ABI
change fails a test here rather than crashing somebody else's application.
The full contract is the document plan 0006 publishes.

## 8. How it is proven

- **The VT oracle** (`iznik-testkit::vt`) wraps `libghostty-vt`, so every
  terminal-semantics assertion runs headless against the emulator the
  application renders with, and snapshots are deterministic text that can be
  committed as goldens.
- **The pseudoterminal harness** drives real processes on real
  pseudoterminals with `read_until_quiet`, so tests assert on content and
  never on wall-clock timing.
- **The two-container fixture**: `iznik-host` (a root `sshd` and an
  unprivileged login user, as a real host is arranged) and `iznik-engine` (as
  bare as a freshly installed machine — no `ssh-agent`, no `~/.ssh`) on a
  private Podman network, with per-run credentials that exist only inside the
  containers. Images carry no toolchain; the developer's machine builds static
  musl binaries under the `regression` profile that are mounted in read-only.
  Its start time is a measured number with a ceiling, and an orphaned
  container cannot outlive its run: podman's own timeout, an owner label and
  a reaper on start see to that.
- **Every scenario is a test.** Scenarios are declarative TOML with a
  mandatory deadline on every step, a budget on the whole, and fault-injection
  steps for link drops and stalled processes; a `harness = false` test binary
  registers one nextest test per scenario, so scenarios run four at a time
  under nextest's deadlines and one filter runs one scenario by hand.
- **The claims registry**: every task that asserts runtime behavior declares
  its claims as data in `regression/claims/<task-id>.toml`, each proven by a
  scenario or by a named test; `xtask claims verify` runs the proofs of the
  tasks a branch changes and fails a branch that changes product code without
  declaring claims; `xtask claims coverage` runs everything and fails on a
  claim without a passing proof or a proof for a claim that does not exist. A
  `test` proof is allowed only where a container adds nothing, and it says
  why.
- **Time is a parameter.** Every interval, deadline, cap and backoff is a
  field of an options struct whose default is the named constant; the product
  uses the default and a test shortens it. An in-process test finishes in
  under five seconds, and nextest says so when one does not.
- **Committed numbers**: latency, throughput and memory baselines live in
  [`docs/notes/baseline.md`](docs/notes/baseline.md) with the machine
  described; tests assert generous ceilings so the regression test survives a
  noisy machine. One file defines how each figure is taken — a benchmark that
  prints the table for a person, included by a test binary that holds the same
  measurement to its ceiling — so a number in the document and a number in a
  gate cannot drift apart.
- **Artifacts are proven, not assumed.** `xtask distribution --target <triple>`
  is held to what a bootstrap needs: the ELF program headers are read to
  establish that no loader is named, two builds of one commit are compared
  byte for byte, the manifest's digest is recomputed from the file, and the
  x86_64 artifact is run inside the host container, which is the only place
  its static linking is really tested. What needs a Mac to build says so with
  `platform = "darwin"` and reports as deferred rather than as proven.
- **Deadlines everywhere.** No test, gate, scenario step, fixture wait or
  spawned process runs without a bound. A hang is a failure that names what
  was running, never a wait.
- **The documents are held to the binaries.** A README that names a command
  nothing answers to still reads well, so nothing but a test notices.
  `readme_commands` — one in `xtask`, one in `iznik-server`, one in
  `iznik-regression` — asks each binary which commands it routes, holds the
  documents to naming every one of them and inventing none, and runs each with
  `--help` under a deadline. Which is also why every subcommand answers
  `--help` rather than doing its work when asked what it takes.

The engineering rules that every line is held to — naming, literals, size,
documentation, the lint set, the dependency allowlist — are in
[`CONTRIBUTING.md`](CONTRIBUTING.md). They are enforced by gates, not by
habit.

## 9. Decisions

| Decision | Why |
|---|---|
| Own server rather than driving tmux's control mode or patching a third-party multiplexer. | Both put a second terminal emulator, a second layout authority and a second key handler between the shell and the surface, and every feature iznik cares about — exact resume, credit-based scheduling, shell-integration marks, faithful exit statuses — becomes a workaround. The server iznik needs is a pseudoterminal holder with a session registry, not a multiplexer. |
| `libghostty-vt` on the server. | A reconnecting client needs a screen, not a byte replay. Running the same emulator on both ends makes `Screen` exact by construction and gives the server titles, working directories and query handling for free. |
| The system `ssh` binary, not an SSH library. | The user's SSH configuration is where their proxies, keys, hardware tokens and certificates already live. Reimplementing it means reimplementing it wrong for the first interesting setup. |
| Hand-written little-endian codec with golden fixtures. | The protocol crate has no dependencies, the fixtures are the contract, and both ends and the C ABI carry one encoding. |
| Client-owned geometry. | A pseudoterminal has one size; the client that renders it knows its font metrics. Two authorities for one number is how they disagree. |
| No floating panes, stacked panes, zoom state or search API in v1. | Nothing on the server needs them; the application can zoom locally. Anything that must be shared between clients is added when a client asks for it. |
| `Screen` as the single resynchronization mechanism. | One path to test instead of two, and the client never has to interpret a hole. |
| Panes always run the login shell and close on exit. | The one behavior everybody expects; holding an exited pane open is a feature to add when a client needs it. |
| Predictive local echo deferred. | The sequence numbers keep it possible; implementing it before a real client can measure it is guesswork. |
| No terminal-side attach command in v1. | A raw single-pane passthrough for use from a phone is cheap and wanted, and it waits until the daemon it attaches to exists. |
| Every mirror on one dedicated thread. | The emulator's handles are `!Send`; a per-pane task on the multi-threaded runtime would not compile, a per-pane thread is waste, and one `LocalSet` thread parses faster than any link delivers. |
| `iznik-link` and `iznik-harness` as crates of their own. | Framing and compression are needed identically by three parties and the protocol crate cannot carry them; the gate runner and fixture must build without the emulator or `cargo xtask doctor` cannot report that Zig is missing. |
| Scenarios are nextest tests. | One runner, one deadline mechanism, one report, free parallelism, and a scenario is run by hand with a filter instead of a bespoke command. |
| A `regression` profile beside `release`. | The container proofs need optimized code on every task; a fat-LTO release build takes minutes and only a shipped artifact is worth it. |
| Time is a parameter. | The previous incarnation's suites were slow because tests waited on constants; a test that shortens a field costs nothing and a constant nobody can shorten costs minutes per run. |
