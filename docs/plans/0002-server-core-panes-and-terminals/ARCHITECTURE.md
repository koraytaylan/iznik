# Architecture — Plan 0002

> The concrete deltas, by symbol. The system is described in the root [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) §5.1; the rules and the way of working are in [`CONTRIBUTING.md`](../../../CONTRIBUTING.md). Read both before your first task. Every task here declares its claims in `regression/claims/<task-id>.toml` and proves them under `xtask claims verify`; a `test` proof carries its `because`.

## What is known about the emulator, and what is still confirmed

The plan was reviewed against `libghostty-vt` 0.2.1 as pinned, so these are facts, not hopes:

- A `Terminal` is **`!Send`**. Every mirror therefore lives on the mirror thread described under `terminal-mirror`; a pane task on the multi-threaded runtime would not compile.
- `Terminal::new(Options { cols, rows, max_scrollback })` takes the scrollback limit in **lines**; `vt_write`, `resize`, `title()`, `pwd()`, `scrollback_rows()`, `cursor_x()`/`cursor_y()`, `active_screen()` and `grid_ref` exist as named.
- Effects are opt-in closures: `on_pty_write` (responses the terminal composes itself, such as cursor position reports and DECRQM), `on_device_attributes`, `on_xtversion`, `on_enquiry`, `on_size` and `on_color_scheme` (queries the embedder must answer), `on_title_changed`, `on_pwd_changed`, `on_bell`, `on_clipboard_write`. Without a registered effect the terminal silently ignores the sequence. The safe crate forbids shared user data; state a closure needs is an `Rc<RefCell<…>>` it captures.
- `Formatter::new(terminal, options)` formats the **active screen only**, and with no selection the entire screen including scrollback. There is no way to format the inactive screen, in the safe crate or the C API. `FormatterOptions` toggles cursor, style, hyperlink, modes, scrolling region, tabstops, keyboard, protection, charsets, palette and working directory individually.
- `osc::Parser` understands OSC 133 semantic prompts, OSC 7 (`ReportPwd`), OSC 0/2 titles and OSC 8 hyperlinks, byte by byte with an explicit terminator.

Two things the tasks still confirm and record in module documentation with the crate version: exactly which queries arrive through `on_pty_write` and which through the dedicated effects (`terminal-mirror`), and that the formatter's scrollback output reproduces exactly through a fresh terminal (`screen-serializer`). The acceptance tests are written against the property — an exact reproduction, a response that arrives — not against the mechanism.

## 0006 — Pseudoterminal Ownership

### `pty-spawn`

`crates/iznik-server/src/pty/mod.rs` and `spawn.rs`:

- `pub struct SpawnOptions { pub program: Program, pub columns: u16, pub rows: u16, pub working_directory: Option<PathBuf>, pub terminfo_directory: Option<PathBuf> }` with `pub enum Program { LoginShell, Command { path: PathBuf, arguments: Vec<String> } }`. The daemon's registry always passes `LoginShell` — the shell from the password database for the current user, with `argv[0]` prefixed by `-` so it initializes as a login shell — and that is the product rule. `Command` exists because a test that spawns the developer's login shell is a test whose prompt draws itself asynchronously: tests spawn `sh`, `cat` and scripts, one test spawns the login shell to prove the rule, and a spawn that must fail names a path that does not exist.
- `pub fn spawn(options: &SpawnOptions) -> Result<PtyProcess, PtyError>` opens a pseudoterminal pair through `portable-pty` and spawns the program in its own session with the slave as its controlling terminal, in `working_directory` when given. The environment is inherited from the daemon with these set: `TERM=xterm-ghostty` and `TERMINFO=<terminfo_directory>` when `terminfo_directory` is given, else `TERM=xterm-256color`; `COLORTERM=truecolor`; `SHELL`; and `TERM_PROGRAM=iznik`. Named constants carry every one of those strings.
- `pub struct PtyProcess` with `process_id() -> u32`, `resize(&self, columns, rows) -> Result<(), PtyError>`, `signal(&self, signal: Signal) -> Result<(), PtyError>` (`Signal::{Hangup, Terminate, Kill}`), and `wait(&mut self) -> Result<ExitStatus, PtyError>`; `pub enum ExitStatus { Exited(i32), Signalled(Signal) }` — a signal death is reported as the signal, never as a fake code. Dropping a `PtyProcess` whose child still runs sends `Kill`: nothing this crate spawns outlives the value that owns it.
- `pub enum PtyError { Open { source }, Spawn { program, source }, WorkingDirectory { path, source }, Resize { source }, Signal { source }, Wait { source } }`.

`portable-pty` owns the `fork`/`exec` unsafe; this crate stays `#![forbid(unsafe_code)]`. `spawn` blocks for the duration of a fork and exec, which is milliseconds; it is called from the command path, never from a pump.

### `pty-streams`

`crates/iznik-server/src/pty/streams.rs` is the one module in the server where blocking I/O exists, and the policy gate names it: its whole job is to turn the pseudoterminal's blocking descriptor into async streams on dedicated blocking threads.

- `pub fn streams(process: &PtyProcess) -> Result<(OutputStream, InputHandle), PtyError>`.
- `pub struct OutputStream` — `pub async fn next(&mut self) -> Option<Vec<u8>>` yielding chunks of at most `READ_CHUNK_LENGTH` (64 KiB) until the child's end of the pseudoterminal closes. The reader thread pushes into a bounded channel of `OUTPUT_CHANNEL_CHUNKS` (16); a consumer that falls behind blocks the reader, which blocks the child's writes — the same backpressure a slow terminal applies, confined to this pane and its child.
- `pub struct InputHandle` — `pub fn write(&self, bytes: Vec<u8>) -> Result<(), InputError>` enqueues one message for the writer thread, which writes each message whole before taking the next, so bytes submitted in one call are never interleaved with another caller's. The queue is bounded by `MAXIMUM_PENDING_INPUT_BYTES` (8 MiB); beyond it `write` returns `InputError::Backlog { pending }` and drops nothing — a paste into a stalled program is refused audibly, not discarded silently.

## 0007 — Terminal Mirror

### `terminal-mirror`

`crates/iznik-server/src/terminal/mod.rs` and `mirror.rs`:

- **The mirror thread.** `pub struct MirrorThread` with `start() -> Result<MirrorThread, MirrorError>` spawns one OS thread running a `tokio::task::LocalSet` on a current-thread runtime, and `spawn(&self, build: impl FnOnce() -> F + Send + 'static)` where `F: Future<Output = ()> + 'static` ships a *constructor* to that thread, which builds the `!Send` future there and spawns it locally. Every pane's VT task is spawned this way; the `Terminal` it owns never leaves the thread. `MirrorThread` is created once by the daemon and handed to the registry; a test creates its own.
- `pub struct Mirror` over a `libghostty_vt::Terminal` created with `MIRROR_SCROLLBACK_ROWS` (10 000) lines of scrollback: `new(columns, rows) -> Result<Mirror, MirrorError>`, `feed(&mut self, bytes: &[u8])`, `resize(&mut self, columns, rows)`, `columns()`, `rows()`, `title() -> String`, `working_directory() -> Option<String>`, `in_alternate_screen() -> bool`.
- **The response policy.** `set_subscriber_count(&mut self, count: usize)` and `take_pending_responses(&mut self) -> Vec<u8>`: the mirror registers `on_pty_write`, `on_device_attributes`, `on_xtversion`, `on_enquiry`, `on_size` and `on_color_scheme`, composes the answer for each of the embedder-side queries (a fixed primary and secondary device attributes string, `iznik-server <crate version>` for XTVERSION, an empty answerback, the mirror's own size in cells with zero pixels, and dark for the color scheme — all named constants), and collects everything into a pending buffer that the pane writes to the pseudoterminal only while the subscriber count is zero. With a subscriber, the collected bytes are discarded: the subscriber's emulator answers, because only the real terminal knows its real colors and capabilities.
- `on_bell`, `on_clipboard_write`, `on_title_changed` and `on_pwd_changed` are not registered: bells and clipboard writes are the client's to act on, and titles and directories reach the model through the mark observer, which knows the byte position of each.

### `screen-serializer`

`crates/iznik-server/src/terminal/screen.rs`:

- `pub fn serialize(mirror: &Mirror, sequence: Sequence) -> Result<SerializedScreen, ScreenError>` producing `pub struct SerializedScreen { pub sequence: Sequence, pub columns: u16, pub rows: u16, pub bytes: Vec<u8>, pub dropped_rows: usize }` through the binding's formatter in VT mode with cursor, style, hyperlink, modes, scrolling region, tabstops, keyboard, kitty keyboard, charsets, protection and working directory emitted, and the palette **not** emitted — the server's palette is not the client's.
- **The alternate screen.** The formatter cannot see an inactive screen, so `pub struct ScreenState { primary_at_switch: Option<PrimarySnapshot { bytes: Vec<u8>, switch: Vec<u8> }> }` remembers the primary screen at the moment a program switched away from it: the pane's byte scanner recognizes `CSI ? 47 h`, `CSI ? 1047 h` and `CSI ? 1049 h`, the pane feeds the mirror up to that sequence, calls `ScreenState::entering_alternate(&mut self, mirror, switch_bytes)` — which serializes the primary while it is still active — and then feeds the rest; `leaving_alternate` clears it. `ScreenState::serialize(&self, mirror, sequence)` then produces, while the mirror is in the alternate screen, the remembered primary, the recorded switch sequence, then the live alternate screen — so a program leaving the alternate screen on the client reveals what it reveals on the server.
- `bytes.len()` is at most `MAXIMUM_SCREEN_BYTES` (768 KiB, below the frame maximum): the serializer drops the oldest scrollback rows until the output fits — by formatting a selection that starts lower — and reports how many in `dropped_rows`.
- The acceptance property, and the reason this module can be trusted: feeding `bytes` into a fresh `Vt` of `columns × rows` produces a snapshot equal to the mirror's, scrollback and cursor included.

### `shell-integration-marks`

`crates/iznik-server/src/terminal/marks.rs`:

- `pub struct MarkObserver` with `observe(&mut self, sequence: Sequence, bytes: &[u8]) -> Vec<MarkEvent>` — a pass-through scanner that never modifies, delays or reorders bytes, recognizes OSC 133 `A`/`B`/`C`/`D;<status>`, OSC 7 `file://<host><path>`, OSC 0/2 titles terminated by BEL or ST, and the three alternate-screen entry and exit sequences, across chunk boundaries at any byte. The framing scan — where an OSC or CSI begins and ends — is this module's; the OSC grammar is ghostty's: a completed OSC payload is handed to `libghostty_vt::osc::Parser` and its `CommandType` decides the event, so there is one implementation of what `133;D;1` means.
- `pub struct MarkEvent { pub sequence: Sequence, pub length: usize, pub kind: MarkKind }` where `MarkKind` is `iznik_protocol::message::MarkKind`, the wire type, including `AlternateScreen { entered }`; `sequence` is the absolute position of the sequence's first byte and `length` its byte length, so the pane can split a chunk exactly at an alternate-screen switch.
- An OSC longer than `MAXIMUM_OSC_LENGTH` (4 KiB) is abandoned without an event; the bytes still pass. This observer is the single source of marks, titles and working directories for the model; the mirror's own title and directory serve only `Screen` reproduction.
- `crates/iznik-testkit/assets/shell-integration.bash` is a minimal `bash` rc file — `PS1='$ '`, a `PROMPT_COMMAND` and a `DEBUG` trap that emit the four OSC 133 marks and an OSC 7 report — so every test and scenario that needs a shell with integration runs `bash --rcfile <asset>` and none depends on the developer's shell configuration.

## 0008 — History and Pane

### `history-ring`

`crates/iznik-server/src/history/mod.rs` and `ring.rs`:

- `pub struct PaneHistory` with `new(capacity: usize)`, `append(&mut self, bytes: &[u8]) -> Sequence` (returns the sequence of the first appended byte; sequences are absolute from the pane's creation), `range(&self, from: Sequence) -> Result<HistorySlices<'_>, HistoryError>` yielding at most two contiguous slices, `copy_range(&self, from: Sequence, maximum: usize, into: &mut Vec<u8>) -> Result<Sequence, HistoryError>` appending up to `maximum` bytes from `from` into a caller-owned buffer and returning the sequence after them, `oldest() -> Sequence`, `newest() -> Sequence`. `HistoryError::AgedOut { oldest }` names the earliest byte still held.
- `pub struct HistoryBudget` shared by every pane on the daemon: `new(total: usize)`, `touch(pane: PaneId)` on focus, and eviction that shrinks the least recently focused pane's ring first when the total is exceeded, so a hundred idle panes cannot exhaust a small host. `DEFAULT_PANE_HISTORY_BYTES` (4 MiB) and `DEFAULT_HISTORY_BUDGET_BYTES` (256 MiB) are the defaults.
- Appending does not allocate per byte or per chunk in steady state; a counting allocator in the test asserts it, on one thread with no runtime running.

### `pane-assembly`

`crates/iznik-server/src/pane.rs` holds the four parts together behind one interface. The pane's VT task runs on the mirror thread; everything else runs anywhere:

- `pub struct Pane` with `spawn(id: PaneId, options: &SpawnOptions, budget: Arc<HistoryBudget>, mirrors: &MirrorThread) -> Result<Pane, PaneError>`, `id()`, `input(&self, bytes: Vec<u8>) -> Result<(), InputError>`, `resize(&self, columns, rows) -> Result<(), PaneError>`, `pub async fn screen(&self) -> Result<SerializedScreen, PaneError>` (a request to the VT task, serialized at the newest appended sequence, so `sequence` is exactly where a client continues), `read_history(&self, from: Sequence, maximum: usize, into: &mut Vec<u8>) -> Result<Sequence, HistoryError>` under a lock held for the copy and no longer, `subscribe(&self)` and `unsubscribe(&self)` maintaining the mirror's subscriber count, `close(&self)` (hangup), and `exit_status(&self) -> impl Future<Output = ExitStatus>`.
- `state(&self) -> watch::Receiver<PaneState>` where `pub struct PaneState { pub newest: Sequence, pub columns: u16, pub rows: u16, pub exited: Option<ExitStatus> }` — a subscriber learns that new bytes exist and reads them from the ring at its own cursor; **the ring is the queue**, and nothing is pushed to a subscriber. `marks(&self) -> broadcast::Receiver<MarkEvent>` carries the observer's events.
- The VT task, per output chunk: observe marks; for each alternate-screen entry the observer reports, feed the mirror up to it and let `ScreenState` remember the primary; feed the rest; append to history; publish the new `newest`; then, while the subscriber count is zero, write the mirror's pending responses to the pseudoterminal. On stream end: wait the child, publish `exited`.

## 0009 — Fidelity

### `fidelity-suite`

The corpus is `iznik_testkit::corpus`, landed in plan 0001. The regression driver gains its first real step kind beyond `run`: `crates/iznik-regression/src/step/pane.rs` implements `[steps.pane]` — `columns`, `rows`, `program` (`login-shell`, the default, or a path with arguments), `send` (a list of `text`, `hex` or `file` messages written through `Pane::input`, a `file` naming one of the scenario's copied files), `until_quiet_milliseconds`, `capture_to` (the bytes read from history since sequence zero, written to a file), `screen_to` (the serialized screen written to a file), and `expect_screen_reproduction` (the driver feeds the captured bytes into one `Vt` and the serialized screen into another and fails the step with the first differing row when their snapshots differ, so the oracle comparison happens where the emulator is). It runs inside the host container against the statically linked `iznik-regression`, which links `iznik-server` — so every claim here is proven against the profile the bootstrap ships, on the image a real host resembles.

## 0010 — Documentation

### `server-core-documentation`

The `iznik-server` README documents every module that landed; the root `ARCHITECTURE.md` §5.1 states what was confirmed about the emulator's query handling and the formatter's scope; `docs/notes/terminal-mirror.md` records the findings with the binding's version, so the next person who bumps `libghostty-vt` knows what to re-check.
