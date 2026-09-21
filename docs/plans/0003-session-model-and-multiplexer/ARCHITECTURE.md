# Architecture — Plan 0003

> The concrete deltas, by symbol. The system is described in the root [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) §4 and §5.2–5.6; the rules and the way of working are in [`CONTRIBUTING.md`](../../../CONTRIBUTING.md). Read both before your first task. Every task here declares its claims in `regression/claims/<task-id>.toml`; a `test` proof carries its `because`.

## Identity, position and the layout rule

Every id is minted by the server, once, from a counter that never reuses a value. Position — a tab's index, a pane's place in a split — is derived from the order of a `Vec` and is never a key. This is asserted directly: closing a tab must not change any surviving tab's `TabId`.

A tab's layout is a tree of splits and leaves. It is kept **normalized**: a split has at least two children, a split's child is never a split of the same direction (it is flattened into the parent with its weights scaled), and a split with one child is replaced by that child. Normalization is a pure function on `LayoutNode`, applied by the server after every operation and to every layout a client submits, so two clients that compute the same arrangement produce the same tree and the reconciler's equality test means what it says.

## The order of this plan

Three pure things land before the thing that assembles them: the channel table and credit windows (`channel-multiplexer`), the resume decision (`resume-and-replay`) and the compression layer (`stream-compression`) are each testable alone in milliseconds, and `multiplexer-assembly` — the pump, the scheduler and the measured latency — is written against all three. The original draft had the multiplexer calling a resume module that landed after it; a caller cannot edit a callee's file, so the callee lands first.

## 0011 — Session Model

### `model-types`

`crates/iznik-protocol/src/model.rs`:

- `pub struct HostModel { pub generation: Generation, pub sessions: Vec<Session> }`, `pub struct Session { pub id: SessionId, pub name: String, pub tabs: Vec<Tab> }`, `pub struct Tab { pub id: TabId, pub name: String, pub panes: Vec<Pane>, pub layout: LayoutNode }`, `pub struct Pane { pub id: PaneId, pub title: String, pub working_directory: Option<String>, pub columns: u16, pub rows: u16 }`.
- `pub enum LayoutNode { Split { direction: SplitDirection, children: Vec<Weighted> }, Leaf(PaneId) }`, `pub struct Weighted { pub node: LayoutNode, pub weight: u32 }`, `pub enum SplitDirection { Horizontal, Vertical }`, and `LayoutNode::normalize(self) -> LayoutNode`, `leaves(&self) -> Vec<PaneId>`, `replace_leaf(&mut self, pane, with: LayoutNode) -> bool`, `remove_leaf(self, pane) -> Option<LayoutNode>`.
- `HostModel::validate(&self) -> Result<(), ModelError>` with one variant per invariant: ids unique across the host; every session has at least one tab and every tab at least one pane; a tab's layout leaves are exactly its panes, each once; every weight is at least one; the layout is normalized; names are non-empty. `validate` is for tests and debug builds, never a release path: a model glitch degrades a client, it does not kill the server.
- `encode_host_model` and `decode_host_model` — the `Snapshot` payload — pinned by `crates/iznik-protocol/tests/fixtures/model.jsonl`.

### `deltas-and-reconciler`

`crates/iznik-protocol/src/delta.rs` — each variant is the smallest thing that can happen; anything larger is several of these, and anything that cannot be expressed as some of these is a snapshot:

`pub enum Delta { SessionAdded { session: Session }, SessionRenamed { session: SessionId, name: String }, SessionRemoved { session: SessionId }, SessionsReordered { order: Vec<SessionId> }, TabAdded { session: SessionId, tab: Tab, index: usize }, TabRenamed { tab: TabId, name: String }, TabRemoved { tab: TabId }, TabsReordered { session: SessionId, order: Vec<TabId> }, PaneAdded { tab: TabId, pane: Pane }, PaneRemoved { pane: PaneId, reason: RemovalReason }, PaneMoved { pane: PaneId, to_tab: TabId }, LayoutChanged { tab: TabId, layout: LayoutNode }, PaneTitle { pane: PaneId, title: String }, PaneWorkingDirectory { pane: PaneId, path: String }, PaneResized { pane: PaneId, columns: u16, rows: u16 } }` with `pub enum RemovalReason { Closed, Exited(ExitStatus) }` and `pub enum ExitStatus { Exited(i32), Signalled(i32) }`. `TabsReordered` carries the whole order, never a swap: a swap applied to the wrong arrangement silently produces a third arrangement nobody has. Encoded and decoded as the `Delta` payload, pinned by `fixtures/delta.jsonl`.

`crates/iznik-protocol/src/reconcile.rs`: `pub fn apply(model: &mut HostModel, generation: Generation, delta: &Delta) -> Result<(), ReconcileError>` requires `generation` to be exactly the model's generation plus one — `ReconcileError::GenerationGap { expected, received }` is the client's cue to request a snapshot — and refuses a delta that would break an invariant with a variant naming the identity involved, leaving the model untouched, which means every check runs before the first mutation. Layout deltas are normalized on application.

`crates/iznik-testkit/src/generate.rs` is the seeded generator three crates' tests share: `pub struct ModelGenerator` from a `u64` seed producing valid models, valid delta sequences from a model (with the model the sequence leads to), and valid registry operation sequences, over a hand-written xorshift so the protocol crate's tests add no dependency. It lands here, with its first consumer, because a generator copied into a second test file would be the second implementation of what a valid sequence is.

### `session-command-codec`

`crates/iznik-protocol/src/command.rs`: `pub enum SessionCommand { CreateSession { name, columns, rows, working_directory }, RenameSession { session, name }, CloseSession { session }, ReorderSessions { order }, CreateTab { session, name, columns, rows, working_directory }, RenameTab { tab, name }, CloseTab { tab }, ReorderTabs { session, order }, CreatePane { tab, placement: Placement, columns, rows, working_directory }, ClosePane { pane }, MovePane { pane, to_tab, placement: Placement }, SetLayout { tab, layout } }` with `pub struct Placement { pub target: PaneId, pub direction: SplitDirection, pub before: bool }` — a new pane replaces the target's leaf with a split of the two, weights equal, in the direction given, the new pane before or after the target — and `pub enum CommandOutcome { Applied { generation: Generation, created: Created }, Rejected { code: RejectionCode, message: String } }`, `pub enum Created { Nothing, Session(SessionId), Tab(TabId), Pane(PaneId) }`, `pub enum RejectionCode { UnknownSession, UnknownTab, UnknownPane, EmptyName, InvalidOrder, InvalidLayout, SpawnFailed }`. Encoded as the `Command` and `CommandResult` payloads, pinned by `fixtures/command.jsonl`.

## 0012 — Session Registry

### `session-registry`

`crates/iznik-server/src/session/mod.rs` and `registry.rs`:

- `pub struct Registry` holding the `HostModel`, the server-side `Pane` for every model pane, the id counters, the shared `HistoryBudget`, the `MirrorThread` every pane's VT task is spawned on, and `pub struct RegistryDefaults { pub program: Program, pub terminfo_directory: Option<PathBuf> }` — the daemon passes `Program::LoginShell`, which is the product rule; a test passes `sh`, which is how a thousand-operation test finishes in seconds. `new(defaults: RegistryDefaults, budget: Arc<HistoryBudget>, mirrors: MirrorThread) -> Registry`, `snapshot(&self) -> HostModel`, `deltas(&self) -> broadcast::Receiver<Numbered<Delta>>` where `pub struct Numbered<T> { pub generation: Generation, pub value: T }` over a channel of `DELTA_BROADCAST_CAPACITY` (1 024) — a receiver that lags and sees `Lagged` sends its client a fresh `Snapshot`, which is what the client's reconciler would have asked for anyway — and `pane(&self, id) -> Option<&Pane>`.
- Operations, each returning the ids it minted and emitting its deltas in a defined order: `create_session` (a session, its first tab, that tab's first pane — one `SessionAdded` carrying the full session), `create_tab` (`TabAdded` carrying the tab with its first pane), `create_pane` (`PaneAdded`, then `LayoutChanged`), `close_pane` and pane exit (`PaneRemoved`, then `LayoutChanged` — or `TabRemoved` when it was the tab's last pane, then `SessionRemoved` when it was the session's last tab), `move_pane` (`PaneMoved`, `LayoutChanged` for the source or `TabRemoved`, `LayoutChanged` for the destination), `set_layout` (normalized, must have exactly the tab's panes as leaves), `rename_session`, `rename_tab`, `close_tab`, `close_session`, `reorder_tabs` (the order must be a permutation of the session's tabs).
- Ingestion: the registry subscribes to every pane's `marks()` and `state()`; `Title` and `WorkingDirectory` marks become `PaneTitle` and `PaneWorkingDirectory` deltas, a size change becomes `PaneResized`, an exit becomes the removal cascade with `RemovalReason::Exited`.
- The generation advances by exactly one per delta, the delta is applied to the registry's own model through the protocol's reconciler before it is broadcast, and with debug assertions on — the dev profile and the `regression` profile both — the model is validated after every operation.

### `session-commands`

`crates/iznik-server/src/session/commands.rs`: `pub fn apply(registry: &mut Registry, command: SessionCommand) -> CommandOutcome` — validation first, against the model, with the `RejectionCode` that names what is wrong; then the operation; then `Applied` with the `created` id. A rejected command changes nothing: the generation is unchanged and no delta is emitted. A spawn failure is `SpawnFailed` with the pseudoterminal error's message. Every command is answered exactly once, and `create_session`, `create_tab` and `create_pane` spawn the registry's program at the requested size in the requested directory.

## 0013 — Multiplexer

### `channel-multiplexer`

`crates/iznik-server/src/multiplexer/channel.rs` and `credit.rs`, the pure tables the pump is built on:

- `pub struct ChannelTable`: `assign(&mut self, pane: PaneId) -> Result<u8, MultiplexerError>` hands out the lowest free channel in `1..=255` (`MultiplexerError::ChannelsExhausted` at the 256th); `release(&mut self, channel)` moves a channel to `released_pending`, where it stays until `acknowledge(&mut self, channel)` — the client's `ChannelReleased` — returns it to the free set, so a late frame can never be misattributed to a new pane; `channel_of(pane)`, `pane_of(channel)`.
- `pub struct Cursor { pub pane: PaneId, pub channel: u8, pub sequence: Sequence, pub credit: CreditWindow, pub stale: bool }` and `pub struct CreditWindow` with `INITIAL_CREDIT_BYTES` (256 KiB) for a background pane, `FOCUSED_CREDIT_BYTES` (1 MiB) for the focused one, `refill(bytes)`, `consume(bytes)`, `available()`; `FRAME_PAYLOAD_LENGTH` (64 KiB), the most one pane frame carries, so a keystroke echo waits behind at most one frame per active pane; `STALE_THRESHOLD_BYTES` (4 MiB), the lag past which a background cursor is marked stale.
- `pub enum MultiplexerError { ChannelsExhausted, UnknownPane { pane }, NotSubscribed { pane }, Sink(SinkError) }`, mapped to the protocol's `ErrorCode` by the connection loop.

### `multiplexer-assembly`

`crates/iznik-server/src/multiplexer/mod.rs` and `scheduler.rs`, one `Multiplexer` per client connection:

- `pub trait FrameSink { async fn send(&mut self, channel: u8, payload: &[u8]) -> Result<(), SinkError>; }` — the test sink collects frames in memory; plan 0004 puts a `FramedLink` under it.
- `pub struct Multiplexer` with `new(registry: Arc<RwLock<Registry>>, sink: Box<dyn FrameSink>) -> Multiplexer`, `subscribe(&mut self, request: StartRequest) -> Result<(), MultiplexerError>` for `Subscribe`, `Resume` and `ScreenRequest`, deciding what to send first through `resume::plan_start` and then assigning a channel, creating a cursor, calling `Pane::subscribe` and sending `PaneChannel`; `unsubscribe(&mut self, pane)` sending `PaneDetached` and releasing the channel pending acknowledgement; `channel_released(&mut self, channel)`; `credit(&mut self, channel, bytes)`; `focus(&mut self, pane)`, which moves the larger window and first service to that pane and touches its history in the budget. A pane that exits while subscribed is detached the same way.
- The pump task fans out: every `Numbered<Delta>` from the registry as a `Delta` frame in order, or a `Snapshot` after `Lagged`; every `MarkEvent` of a subscribed pane as a `Mark` frame; and pane bytes as governed by the scheduler.
- One scheduling round: the focused cursor first, then every other cursor in round-robin order; for each, `Pane::read_history` at the cursor up to the smaller of its credit and `FRAME_PAYLOAD_LENGTH` into a buffer the pump owns, send, advance. A cursor at zero credit is skipped, never awaited. The round repeats while any cursor has bytes and credit; otherwise the pump waits on the pane state watches and the credit refills, and a stalled cursor costs nothing.
- Stale marking: a background cursor whose lag (`newest − sequence`) exceeds `STALE_THRESHOLD_BYTES` stops being served and is marked stale; on its next service when it has credit — and immediately on focus — the pump sends `PaneChannel { sequence: newest }` followed by a `Screen`, sets the cursor to `newest`, and clears the mark. The ring keeps every byte regardless, so a client that scrolls back after focus can still resume from an older sequence.
- The acceptance is a number: with one pane flooding at line rate and another echoing keystrokes, both subscribed, the keystroke-to-echo round trip through the multiplexer and an in-memory sink stays under `KEYSTROKE_ROUND_TRIP_BUDGET` (25 ms) at the 99th percentile over a thousand samples, and the test that measures it is named so that nextest gives it the machine to itself.

## 0014 — Resume and Compression

### `resume-and-replay`

`crates/iznik-server/src/resume.rs` decides what a subscription starts with, and it is the only place that decides it — a pure function over what the ring holds:

- `pub enum StartRequest { Subscribe { pane }, Resume { pane, from: Sequence }, ScreenRequest { pane } }` and `pub enum StartPlan { Continue { from: Sequence }, Screen { at: Sequence } }`; `pub fn plan_start(request: &StartRequest, oldest: Sequence, newest: Sequence) -> StartPlan`.
- `Subscribe` → `Screen { at: newest }`: a cold attach gets the truth and continues from it. `Resume { from }` → `Continue { from }` when `oldest <= from <= newest`, a hot reconnect that costs nothing; otherwise exactly the `Subscribe` plan. `ScreenRequest` → `Screen { at: newest }` and the cursor moves there, so no byte is delivered twice.
- The multiplexer executes a plan as: `Continue` → `PaneChannel { sequence: from }` and bytes from there with no `Screen`; `Screen { at }` → `PaneChannel { sequence: at }`, then the `Screen` serialized at exactly `at`, then live bytes from `at`. `Screen::sequence` is always the sequence the mirror was exact at and the bytes that follow on the channel begin exactly there; `multiplexer-assembly` proves it by reassembling the two and comparing with an unbroken stream through the oracle.

### `stream-compression`

`crates/iznik-protocol/src/dictionary.rs` exposes `pub const COMPRESSION_DICTIONARY: &[u8]`, `include_bytes!` of `crates/iznik-protocol/assets/compression-dictionary.bin`, trained with `zstd --train` on the fidelity corpus golden; the training command and the corpus hash are in the module documentation. `crates/iznik-link/src/compression.rs`: `pub struct ZstdStream<S>` implementing `AsyncRead + AsyncWrite` over any stream, a streaming zstd encoder and decoder primed with the dictionary — per connection, not per frame, so the window spans frames and a keystroke echo is not inflated, with a flush after every frame so nothing waits in a buffer — and `pub fn compressed<S>(stream: S, leftover: Vec<u8>) -> FramedLink<ZstdStream<S>>`, which is what both ends call with the parts of their plain link after both `Hello`s carried `Capabilities::ZSTD`; the leftover bytes are the peer's first compressed bytes, read past its `Hello`. Two measured numbers decide whether the capability stays advertised: a ratio of at least `MINIMUM_CORPUS_RATIO` (3.0) over the fidelity corpus, and an added latency of at most `SMALL_FRAME_LATENCY_CEILING` (1 ms) at the 99th percentile for a one-byte frame in-process, with the median printed beside it.

## 0015 — Documentation

### `session-documentation`

`docs/notes/protocol.md` is the wire reference the macOS repository reads: framing, every control message with its byte layout, the model, delta and command encodings, sequence semantics, the subscribe/resume/screen rules, the credit protocol and the compression negotiation — written from the golden fixtures, which remain the arbiter. The `iznik-protocol`, `iznik-link` and `iznik-server` READMEs document every module that landed, and the root `ARCHITECTURE.md` §4 and §5.2–5.6 say what is true.
