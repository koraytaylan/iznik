# Architecture — Plan 0004

> The concrete deltas, by symbol. The system is described in the root [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) §5.5 and §8; the rules and the way of working are in [`CONTRIBUTING.md`](../../../CONTRIBUTING.md). Read both before your first task. Every task here declares its claims in `regression/claims/<task-id>.toml`; a `test` proof carries its `because`.

## The order of this plan

The protocol test client lands first, because every test after it speaks through it and a second hand-rolled client in `client-connections` would be the second implementation of the protocol's client side. The connection loop lands before the daemon, because the daemon's accept loop calls it and a caller cannot edit a callee's file — and because a connection loop is testable over a socket pair in milliseconds, with no daemon, no filesystem and no process. The relay lands after the daemon it starts, and the in-process stack after the daemon it embeds.

## 0016 — Daemon

### `test-client`

`crates/iznik-testkit/src/client.rs`: `pub struct TestClient` speaking `iznik/1` over any duplex stream through `FramedLink` — `connect(socket: &Path)`, `over(stream)`, `hello(capabilities) -> Result<ServerHello, ClientError>` (which engages compression through `iznik_link::compression::compressed` when both sides advertised `ZSTD`), `snapshot()`, `command(SessionCommand) -> CommandOutcome`, `subscribe(pane)`, `unsubscribe(pane)`, `resume(pane, from)`, `screen_request(pane)`, `input(pane, bytes)`, `resize(pane, columns, rows)`, `focus(pane)`, `credit(channel, bytes)`, `channel_released(channel)`, `ping()`, `next(deadline) -> Result<Received, ClientError>` where `Received` is a decoded control message or `PaneBytes { channel, bytes }`, `bytes_of(pane)` (everything received on the pane's channel since subscription), `deltas()`, and `auto_credit(bool)` which returns credit for every byte as it arrives and acknowledges every detach — the client the macOS application will resemble, minus the surface. `ClientError::Deadline { waited }` is what `next` returns rather than waiting. Its own tests run it against a hand-written server side over `tokio::io::duplex`, asserting every frame it sends against the protocol goldens.

### `client-connections`

`crates/iznik-server/src/connection.rs`, one per accepted stream:

- `pub async fn serve<S>(stream: S, registry: Arc<RwLock<Registry>>, budget: Arc<HistoryBudget>) -> Result<(), ConnectionError>` for any `AsyncRead + AsyncWrite + Unpin` stream, which is why it is tested over `tokio::net::UnixStream::pair()` with no daemon present.
- Handshake first: the client's `Hello` must arrive as the first frame; a `protocol_version` other than `PROTOCOL_VERSION` is answered with `Error { code: ProtocolVersion }` and the connection closes; otherwise the server's `Hello` follows, the link is rebuilt through `compressed` from its parts if both advertised `ZSTD`, and the loop begins.
- Dispatch: `SnapshotRequest` → `Snapshot`; `Command` → `session::commands::apply` → `CommandResult`; `Subscribe`, `Resume`, `ScreenRequest`, `Credit`, `ChannelReleased`, `Focus`, `Unsubscribe` → the connection's `Multiplexer`, whose `MultiplexerError` becomes `Error` with the matching `ErrorCode`; `Input` → `Pane::input` (a `Backlog` becomes `Error { code: InputBacklog }` naming the pane, and the connection stays open); `Resize` → `Pane::resize`, whose `PaneResized` delta every client then receives; `Ping` → `Pong`. A frame that does not decode closes the connection: a peer that speaks garbage is disconnected, a well-formed request that is wrong is refused.
- One writer per connection: the multiplexer's pump owns the link's writer half, and the loop hands it every reply — `Hello`, `Snapshot`, `CommandResult`, `Pong`, `Error` — through a channel, so the order of frames on the wire is one task's decision and two tasks never race for the socket.
- Focus is per connection and is not model state. On disconnect — clean or not — every subscription is dropped (so subscriber counts fall and mirrors answer queries again), every channel is released, and nothing about the sessions changes.

### `daemon-lifecycle`

`crates/iznik-server/src/daemon/mod.rs`, `socket.rs`, `lock.rs`, `logging.rs`, `idle.rs`:

- `pub struct RuntimePaths { pub directory: PathBuf, pub socket: PathBuf, pub lock: PathBuf, pub log: PathBuf }` with `resolve() -> Result<RuntimePaths, PathsError>`: `$XDG_RUNTIME_DIR/iznik/`, else `$TMPDIR/iznik-<user_id>/`, else `/tmp/iznik-<user_id>/`; the directory is created with mode `0700`. A locked-down host without a runtime directory is a real case and must not be discovered during someone's first bootstrap.
- `pub struct DaemonOptions { pub idle_shutdown: Duration (IDLE_SHUTDOWN, 10 minutes), pub socket_ready_cap: Duration (SOCKET_READY_CAP, 10 seconds), pub stop_cap: Duration (STOP_CAP, 10 seconds), pub program: Program (LoginShell) }` — every timing a parameter, with `--idle-shutdown-seconds <n>` accepted by `--daemon` and `--foreground` so no test ever waits ten minutes.
- `pub struct Lock` with `acquire(path) -> Result<Lock, LockError>` — an exclusive, non-blocking `flock` on the lock file, which holds the daemon's process id for diagnostics; `LockError::Held { process_id }` names the holder. Single instance is enforced by the lock, never by socket existence: a stale socket file left by a killed daemon is removed and rebound once the lock is ours; a live daemon is respected because its lock is held.
- `pub async fn serve(paths: &RuntimePaths, options: DaemonOptions, shutdown: watch::Receiver<bool>) -> Result<(), DaemonError>` is the daemon: bind after removing a stale file, create the `MirrorThread`, the registry and the budget, accept, and hand every stream to `connection::serve` on its own task; exit with code 0, removing socket and lock, after `idle_shutdown` with no panes and no clients, or when `shutdown` flips, or on `SIGTERM`. A daemon with panes never exits on its own; one lingering forever on a shared host with nothing to do is rude. `--foreground` runs `serve` in the calling process; the in-process stack calls it directly.
- `iznik-server --daemon` spawns `iznik-server --foreground` with its standard streams on `/dev/null` — through `tokio::process`, in the parent's process group, because a process that is already a group leader cannot `setsid` — and the foreground child calls `setsid` first thing, so it belongs to no session that an SSH disconnect can hang up; `--daemon` returns as soon as a connection to the socket succeeds, within `socket_ready_cap`, and exits non-zero naming the path otherwise.
- `logging::initialize(paths) -> Result<(), LoggingError>` installs `tracing-subscriber` writing through a `MakeWriter` of this module's own with a `MAXIMUM_LOG_BYTES` (4 MiB) cap and one rotated predecessor — `tracing-appender` rotates by time, not size — level from `IZNIK_LOG`; the one thing worse than no logs from a remote daemon is a full disk.
- `iznik-server --version` prints `iznik-server <crate version> protocol <PROTOCOL_VERSION>` on stdout and nothing else, because the bootstrap parses it.
- `iznik-server --stop` reads the process id from the lock file, sends `SIGTERM`, and waits up to `stop_cap` for the socket to disappear; it is what an explicit upgrade and `uninstall` run, and a daemon holding panes is stopped only because a client asked for exactly that.

### `stdio-relay`

`crates/iznik-server/src/relay.rs`: `iznik-server --stdio` connects to `RuntimePaths::resolve()?.socket`; if the connection is refused or the file is absent, it runs the `--daemon` start and waits for the socket with `socket_ready_cap`, exiting non-zero with a message naming the path when it does not appear; then it relays with `copy_bidirectional` until either side closes, and exits 0. The relay is the only command the bootstrap in plan 0005 ever runs on a host; everything about starting the daemon lives here.

## 0017 — Measurement

### `integration-harness`

`crates/iznik-testkit/src/stack.rs`:

- `pub struct Stack` with `start(options: StackOptions) -> Result<Stack, StackError>` and `socket() -> &Path`, under a temporary runtime directory, with teardown on drop that ends the daemon and every pane it spawned. `StackOptions { daemon: DaemonMode, idle_shutdown: Duration, program: Program }` where `DaemonMode::InProcess` runs `iznik_server::daemon::serve` on a runtime thread the stack owns — the default, used by every test outside the server package, because `CARGO_BIN_EXE_iznik-server` exists only inside the package that builds the binary and a test that guesses a binary's path finds a stale one — and `DaemonMode::Binary(PathBuf)` spawns `<path> --foreground`, used by the server package's own lifecycle, relay and baseline tests with `env!("CARGO_BIN_EXE_iznik-server")`.
- The regression driver gains `[steps.client]` in `crates/iznik-regression/src/step/client.rs`: `command` (a process whose standard streams speak `iznik/1`, such as `/iznik/bin/iznik-server --stdio` or `ssh host0 /iznik/bin/iznik-server --stdio`), an ordered list of `actions` mirroring `TestClient`'s methods plus `await_bytes`, `until_quiet_milliseconds`, `capture_to`, `screen_to` and `expect_reassembly` — the driver feeds the captured screen and the bytes that followed it into one `Vt` and an unbroken reference stream into another and fails the step on a difference. It is what lets a scenario on the engine attach to the daemon on the host through real SSH.

### `performance-baseline`

`crates/iznik-server/benches/baseline.rs` (`harness = false`, `std::time::Instant`) runs against a `Stack` in `Binary` mode over the unix socket and reports, as a Markdown table: input-to-first-echo latency at idle and under a line-rate flood on another pane (median and 99th percentile over a thousand samples); sustained single-pane throughput and aggregate throughput across eight panes; resident memory of the daemon process at rest and with fifty idle panes; daemon startup to socket ready. It is run with `cargo bench --profile regression`, so the numbers describe the profile the containers run. The numbers land in `docs/notes/baseline.md` with the machine described. `crates/iznik-server/tests/regression_baseline.rs`, `#[ignore]` because it measures over a window, asserts generous ceilings, not the measured values — `IDLE_LATENCY_CEILING` 5 ms, `FLOOD_LATENCY_CEILING` 30 ms at the 99th percentile, `SINGLE_PANE_THROUGHPUT_FLOOR` 50 MiB/s, `RESTING_MEMORY_CEILING` 32 MiB, `FIFTY_PANE_MEMORY_CEILING` 256 MiB, `STARTUP_CEILING` 500 ms — because a regression test that fails on a noisy machine gets deleted, and then there is none. Its claims carry `profile = "regression"`, so the claims gate runs it optimized and alone.

## 0018 — Distribution

### `linux-artifacts`

`xtask distribution --target <triple>` for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` builds `iznik-server` under the `release` profile the scaffold defined, passing `--remap-path-prefix` so two consecutive builds are byte-identical, and produces `target/distribution/<triple>/iznik-server`, stripped, plus `SHA256SUMS` and `manifest.toml` recording the crate version, `PROTOCOL_VERSION`, the triple and the digest — for people and for CI; the bootstrap computes digests itself. Each binary is under `ARTIFACT_SIZE_CEILING` (24 MiB). `xtask/tests/regression_distribution_linux.rs` builds release artifacts and is `#[ignore]`. The x86_64 artifact is proven inside the host container by a test that assembles a staging directory around it and runs `--version` there through the fixture — a `test` proof, because the artifact under test is built by the test — and the aarch64 artifact by ELF header inspection, since no such machine is in the fixture.

### `darwin-artifacts`

Remote hosts are not always Linux. `xtask distribution --target aarch64-apple-darwin|x86_64-apple-darwin` reuses the manifest and checksum path and fails with a message naming the missing SDK component when the toolchain is absent; `.github/workflows/darwin-artifacts.yml` builds them on a macOS runner. The task is gated because a Darwin toolchain cannot be assumed on a Linux development machine; a human decides how it is provided.

## 0019 — Documentation

### `daemon-documentation`

The root `ARCHITECTURE.md` §5.5 and §8 say what is true, `README.md` gains the build and distribution commands, and the READMEs of `iznik-server`, `iznik-testkit`, `iznik-regression` and `xtask` document every module and step that landed, with the baseline linked.
