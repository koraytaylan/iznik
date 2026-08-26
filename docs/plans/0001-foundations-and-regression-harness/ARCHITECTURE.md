# Architecture — Plan 0001

> The concrete deltas, by symbol. The system this plan begins is described in the root [`ARCHITECTURE.md`](../../../ARCHITECTURE.md); the rules every task obeys and the way every task is worked are in [`CONTRIBUTING.md`](../../../CONTRIBUTING.md). Read both before your first task. This document does not repeat them.

## Layout

The workspace is a single cargo workspace at the repository root with ten members: `iznik-protocol`, `iznik-link`, `iznik-server`, `iznik-client`, `iznik-ffi`, `iznik-cli`, `iznik-harness`, `iznik-testkit`, `iznik-regression` and `xtask`. Their roles and dependency direction are the root architecture's crate table. `iznik-server`, `iznik-regression` and `xtask` are each a library with a thin binary, so that their tests and their peers link the library rather than shelling out to the binary.

Two layering facts decide the crate split, and both were learned the hard way:

- **`xtask` never depends on the emulator.** `libghostty-vt` is built by Zig from a fetched ghostty checkout; a tool whose job is to tell you Zig is missing cannot itself require Zig to compile. So the test infrastructure that `xtask` needs — the bounded process runner, the deadline helpers, the container fixture, the scenario format — lives in `iznik-harness`, whose dependencies are `nix`, `serde`, `serde_json`, `toml`, `regex` and `sha2`, and `cargo xtask` builds in seconds on a machine with nothing but a Rust toolchain. `iznik-testkit` holds the instruments that do need the emulator or the server: the golden loader, the VT oracle, the pseudoterminal harness, the process metrics every ceiling is measured with, the fidelity corpus, the model generator, the protocol test client and the in-process stack.
- **One link layer, not two.** Framing a duplex stream and compressing it are needed identically by the server, the client and the test client, and the protocol crate cannot hold them because it has no dependencies. `iznik-link` holds them once, over `tokio` and `zstd`, so that "the twins are held to the same corpus" is never a sentence anybody has to write.

**The invariant this plan establishes:** task `workspace-scaffold` writes every manifest and every module declaration the project will ever have, including modules later plans fill, and pins every dependency those plans need at an exact version. No later task in any plan adds a `mod` declaration, a workspace member or a dependency. A later task fills a body that already exists. This is what keeps every task footprint disjoint across six plans, and it is what makes the dependency gate an equality rather than a subset.

**The dispatcher rule that makes the invariant workable.** Every binary's `main.rs` looks at its first argument, and only its first argument, and hands the rest of `argv` to the entry point of the module that owns the subcommand: `xtask policy …` calls `policy::run(&arguments)`, `iznik-server --stdio …` calls `relay::run(&arguments)`. Each module's entry point is written by the scaffold as a stub that writes `<subcommand>: not implemented until task <task-id>` to stderr and returns `ExitCode::from(2)`; the task that owns the module replaces the body and parses its own flags. Nothing ever edits a `main.rs` after the scaffold, and a flag added in plan 0004 is a change inside plan 0004's footprint. One stub has a bootstrap behavior instead of a refusal, because it is a gate: `xtask claims verify` exits 0 while `regression/claims/` does not exist and fails naming task `claims-registry` the moment it does, so the fifth gate passes on every task that lands before the registry and cannot be forgotten by the task that lands it.

**Call direction is dependency direction.** A module that calls into another module lands after it, because the caller's task cannot edit the callee's file. Every plan's DAG is checked against this rule; where the original draft had a caller landing first — the multiplexer calling a resume module written later, a daemon accept loop calling a connection module written later — the order has been reversed.

### Module skeleton

Every file below is created by `workspace-scaffold` as a stub carrying module documentation only, with every `mod` declaration wired, except the subcommand entry points described above.

| Crate | Modules |
|---|---|
| `iznik-protocol` | `frame`, `capabilities`, `identity`, `message`, `model`, `delta`, `reconcile`, `command`, `dictionary` |
| `iznik-link` | `framed`, `compression` |
| `iznik-server` | `pty/{mod,spawn,streams}`, `terminal/{mod,mirror,screen,marks}`, `history/{mod,ring}`, `pane`, `session/{mod,registry,commands}`, `multiplexer/{mod,channel,credit,scheduler}`, `resume`, `daemon/{mod,socket,lock,logging,idle}`, `connection`, `relay` |
| `iznik-client` | `model`, `reduce`, `commands`, `transport/{mod,ssh,channel}`, `bootstrap/{mod,probe,upload,launch,terminfo}`, `host/{mod,identity,state,manager}` |
| `iznik-ffi` | `lib` (entry points), `error`, `model`, `pane` |
| `iznik-cli` | `output`, `probe`, `state`, `tail`, `benchmark`, `doctor`, `uninstall` |
| `iznik-harness` | `process`, `deadline`, `images`, `staging`, `fixture`, `scenario`, `report`, `runner` |
| `iznik-testkit` | `golden`, `vt`, `pty`, `metrics`, `corpus`, `generate`, `client`, `stack` |
| `iznik-regression` | `step/{mod,run,pane,client,transport,channel,probe,upload,bootstrap,manager}` |
| `xtask` | `gate`, `doctor`, `policy/{mod,lexicon,literals,length,documentation,links,dependencies,blocking,unsafe_boundary,attributes}`, `claims/{mod,registry,selection,verify}`, `regression`, `distribution/{mod,linux,darwin}`, `header`, `soak` |

Subcommands dispatched from the first commit: `iznik-server {--stdio, --daemon, --foreground, --stop, --version}`; `iznik {probe, state, tail, benchmark, doctor, uninstall}`; `iznik-regression step`; `xtask {check, gate, doctor, policy, claims, regression, distribution, header, soak}`, where `claims` takes `verify` and `coverage` and `regression` takes `images`, `stage` and `reap`.

Module style is `mod.rs` directories (`clippy::self_named_module_files`). Every crate root is `#![doc = include_str!("../README.md")]` and, except `iznik-ffi`, `#![forbid(unsafe_code)]`. The regression driver's step kinds are one file each because every plan adds a kind: plan 0002 fills `step/pane.rs`, plan 0004 `step/client.rs`, plan 0005 the rest, and `step/mod.rs` — written once by `scenario-driver` — dispatches on the table key of a step to `<kind>::execute`, so adding a kind is filling a file, never editing the dispatcher.

## Time is a parameter

Every interval, deadline, cap and backoff a product or test crate uses is a field of the options struct its owner is constructed with, and the named constant is that field's default. `IDLE_SHUTDOWN` is the default of `DaemonOptions::idle_shutdown`; `PING_INTERVAL` of `ChannelOptions::ping_interval`; `SSH_READY_CAP` of `FixtureOptions::ssh_ready_cap`. The product uses the defaults; a test or a scenario sets the field. A test that waits ten seconds for a pong deadline or thirty seconds for a readiness cap is a test written against a constant instead of a parameter, and it is rejected in review. This rule exists because the previous incarnation's suites were slow for exactly this reason, and it is stated once here so that no task has to rediscover it.

## What the harness is held to

The regression harness is the thing every later claim runs through, so its own speed is a claim with a proof, measured and asserted like any other number in this repository:

- `FIXTURE_START_CEILING` (10 seconds): from `Fixture::start` to both containers answering over SSH, with warm images. The fixture's test prints the measured time and fails above the ceiling.
- `SCENARIO_OVERHEAD_CEILING` (30 seconds): a scenario's wall clock minus the sum of its steps' durations — fixture start, file copy, teardown — asserted for every scenario by the scenario harness, with four scenarios sharing the machine.
- `MAXIMUM_SCENARIO_BUDGET_SECONDS` (300): a scenario whose `budget_seconds` exceeds this does not parse. Anything that needs more than five minutes is a soak, not a scenario.
- Scenarios are tests, and tests run in parallel — four at a time, the `scenarios` test group's `max-threads` in `.config/nextest.toml`, because every scenario owns three containers and sixteen fixtures starting at once would contend for podman and turn the overhead ceiling into noise. A scenario that measures time declares `exclusive = true` and runs alone.
- Staging is a no-op when nothing changed, and the fixture is never started for a claim that does not need it.

The claims gate for a typical task — a warm build, five scenarios — is expected to finish in under two minutes. Its deadline is fifteen; a deadline is what catches a hang, not what a run is allowed to take.

## 0001 — Scaffold and Gates

### `workspace-scaffold`

The root `Cargo.toml` carries `[workspace] resolver = "3"`, the ten members, `[workspace.package]` with `version = "0.1.0"`, `edition = "2024"`, `rust-version = "1.97"`, `license = "MIT"`, the profiles, and the complete lint table. The table is the rule set of `CONTRIBUTING.md` §3 made executable, and it is decided here so that no task ever edits it:

```toml
[workspace.lints.rust]
missing_docs = "deny"
unreachable_pub = "deny"
unused_qualifications = "deny"
missing_debug_implementations = "deny"
rust_2018_idioms = { level = "deny", priority = -1 }
trivial_casts = "deny"
trivial_numeric_casts = "deny"
unused_import_braces = "deny"
unused_lifetimes = "deny"
unused_macro_rules = "deny"
dead_code = "deny"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "deny", priority = -1 }
cognitive_complexity = "deny"
allow_attributes = "deny"
allow_attributes_without_reason = "deny"
arithmetic_side_effects = "deny"
as_conversions = "deny"
assertions_on_result_states = "deny"
create_dir = "deny"
dbg_macro = "deny"
deref_by_slicing = "deny"
empty_enum_variants_with_brackets = "deny"
empty_structs_with_brackets = "deny"
error_impl_error = "deny"
exit = "deny"
expect_used = "deny"
filetype_is_file = "deny"
format_push_string = "deny"
get_unwrap = "deny"
if_then_some_else_none = "deny"
indexing_slicing = "deny"
iter_over_hash_type = "deny"
let_underscore_must_use = "deny"
lossy_float_literal = "deny"
map_err_ignore = "deny"
mem_forget = "deny"
min_ident_chars = "deny"
missing_assert_message = "deny"
missing_docs_in_private_items = "deny"
mixed_read_write_in_expression = "deny"
multiple_unsafe_ops_per_block = "deny"
needless_raw_strings = "deny"
non_ascii_literal = "deny"
panic = "deny"
print_stderr = "deny"
print_stdout = "deny"
pub_without_shorthand = "deny"
redundant_type_annotations = "deny"
ref_patterns = "deny"
rest_pat_in_fully_bound_structs = "deny"
same_name_method = "deny"
self_named_module_files = "deny"
shadow_unrelated = "deny"
single_char_lifetime_names = "deny"
str_to_string = "deny"
string_slice = "deny"
string_to_string = "deny"
suspicious_xor_used_as_pow = "deny"
todo = "deny"
try_err = "deny"
undocumented_unsafe_blocks = "deny"
unicode_not_nfc = "deny"
unimplemented = "deny"
unnecessary_safety_comment = "deny"
unnecessary_self_imports = "deny"
unreachable = "deny"
unseparated_literal_suffix = "deny"
unwrap_in_result = "deny"
unwrap_used = "deny"
verbose_file_reads = "deny"

[workspace.lints.rustdoc]
all = { level = "deny", priority = -1 }
```

`clippy::renamed_function_params` is deliberately absent: it forbids renaming a trait method's parameters, and `min_ident_chars` with an empty allow list forbids keeping `fmt(f)` and `from_str(s)` as the standard library names them. The two cannot both hold, the whole-word rule is the one this codebase wants, and `CONTRIBUTING.md` §3.8 records the removal. Note also that `arithmetic_side_effects` and `non_ascii_literal` have no test-scope relaxation in clippy: test code writes `count.checked_add(1)` and `"\u{6F22}"` like everything else, and the lexicon and literal policies are the only rules with a test scope.

Profiles: `[profile.regression]` inherits `release` with `lto = false`, `codegen-units = 16`, `incremental = true`, `debug = "line-tables-only"` and `debug-assertions = true` — optimized enough to measure, fast enough to rebuild on every task, with the registry's debug-only model validation still on inside the containers. `[profile.release]` has `strip = true`, `debug = false`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` and is used only by `xtask distribution`, because a fat-LTO build of the server takes minutes and nothing but a shipped artifact needs it.

`clippy.toml` holds the thresholds and the scope of the test-only relaxations: `too-many-lines-threshold = 100`, `cognitive-complexity-threshold = 15`, `min-ident-chars-threshold = 1` with `allowed-idents-below-min-chars = []`, `check-private-items = true`, `avoid-breaking-exported-api = false`, and `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-indexing-slicing-in-tests`, `allow-panic-in-tests` all `true` — a panicking test is a failing test. `allow-print-in-tests` and `allow-dbg-in-tests` stay `false`: tests speak through assertions. A measured number a test prints on failure goes into the assertion message.

`rustfmt.toml`: `newline_style = "Unix"`, `use_field_init_shorthand = true`, `use_try_shorthand = true`. `rust-toolchain.toml` pins channel `1.97.1` with `clippy` and `rustfmt` and the targets `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`. `.cargo/config.toml` defines the alias `xtask = "run --quiet --package xtask --"`, the musl linkers (`x86_64-linux-musl-gcc`, `aarch64-linux-musl-gcc`, with `-C link-self-contained=no` because those cross-compilers supply libc and the startup objects and rustc's self-contained copies must not be linked beside them), and `[env] LIBGHOSTTY_VT_SYS_OPTIMIZE = "ReleaseFast"`: the `libghostty-vt-sys` build script otherwise builds the emulator in Zig's `Debug` mode for the dev profile, and every test would then run a different, many times slower emulator than the one the product ships. One emulator build, everywhere.

`.config/nextest.toml` gives the default profile `retries = 0`, `fail-fast = false` and `slow-timeout = { period = "5s", terminate-after = 12 }`: a test over five seconds is printed as `SLOW` and is a review rejection; one over sixty is killed and reported by name. Three overrides: test binaries whose name starts with `regression_` — the ones that start containers, build release artifacts or measure over a window — get `slow-timeout = { period = "60s", terminate-after = 10 }`; tests named `scenario::…` belong to the test group `scenarios` with `max-threads = 4`; tests whose name matches `latency|throughput|baseline` get `threads-required = "num-cpus"` so a timing measurement never shares the machine with the rest of the suite. A `claims` profile inherits `default` and adds `[profile.claims.junit] path = "claims.xml"`, which is how `xtask claims verify` reads outcomes per test.

Dependencies, pinned exactly, per crate — this is the whole tree, and `policy-gates` turns it into an equality check. Development dependencies are listed with the crate that declares them; `iznik-protocol` has no `[dependencies]` and that is what its rule means:

- `iznik-protocol`: none. Development: `iznik-testkit`, `serde_json` (the JSONL goldens).
- `iznik-link`: `iznik-protocol`, `tokio` (`io-util`), `zstd`. Development: `tokio` (`rt`, `macros`), `iznik-testkit`.
- `iznik-server`: `iznik-protocol`, `iznik-link`, `tokio` (`rt-multi-thread`, `net`, `io-util`, `sync`, `time`, `signal`, `macros`, `fs`, `process`), `portable-pty`, `nix` (`fs`, `signal`, `user`, `process`), `libghostty-vt`, `tracing`, `tracing-subscriber` (`env-filter`, `fmt`). Development: `iznik-testkit`.
- `iznik-client`: `iznik-protocol`, `iznik-link`, `tokio` (`rt-multi-thread`, `process`, `net`, `io-util`, `sync`, `time`, `macros`, `fs`), `nix` (`user`), `sha2`, `tracing`, `tracing-subscriber` (`env-filter`, `fmt`). Development: `iznik-testkit`.
- `iznik-ffi`: `iznik-client`, `iznik-protocol`. Development: `iznik-testkit`.
- `iznik-cli`: `iznik-client`, `iznik-protocol`, `tokio` (`rt-multi-thread`, `macros`, `signal`), `serde_json`. Development: `iznik-testkit`.
- `iznik-harness`: `nix` (`signal`, `process`), `serde`, `serde_json`, `toml`, `regex`, `sha2`.
- `iznik-testkit`: `iznik-protocol`, `iznik-link`, `iznik-server`, `iznik-harness`, `libghostty-vt`, `portable-pty`, `tokio` (`rt-multi-thread`, `net`, `io-util`, `sync`, `time`, `macros`).
- `iznik-regression`: `iznik-harness`, `iznik-testkit`, `iznik-server`, `iznik-client`, `iznik-protocol`, `serde`, `serde_json`, `toml`, `tokio` (`rt-multi-thread`, `macros`). Development: `libtest-mimic`.
- `xtask`: `iznik-harness`, `syn` (`full`, `visit`), `serde`, `serde_json`, `toml`, `sha2`, `cbindgen`.

The development-dependency cycle — `iznik-server`'s tests use `iznik-testkit`, which depends on `iznik-server` — is legal for integration tests and compiles the library once; it is not legal for `#[cfg(test)]` modules, which would see two copies of every server type. So there are no `#[cfg(test)]` modules anywhere: every test is a file under `tests/`, and `policy-gates` reports the attribute.

No crate has a `build.rs`. No benchmark harness: `crates/iznik-server/benches/baseline.rs` is `harness = false` and measures with `std::time::Instant`.

`policy/lexicon/workspace-scaffold.txt` seeds the vocabulary with every word the skeleton uses. `xtask/tests/skeleton.rs` is the scaffold's own acceptance: module completeness in both directions, README inclusion, `forbid(unsafe_code)` placement, exact pins, the profiles, and every dispatcher stub.

**About the emulator build, so nobody is surprised by it.** `libghostty-vt-sys` 0.2.1 clones ghostty from GitHub at a pinned commit into its `OUT_DIR` — a partial clone, `--filter=blob:none` — and runs `zig build`, which fetches ghostty's Zig packages. That happens once per target and profile in the shared `CARGO_TARGET_DIR` (the dev profile for the host; the regression profile for `x86_64-unknown-linux-musl`; the release profile for both musl targets) and needs network and `git` each first time. `cargo xtask doctor` checks for `git` and says so. A sandboxed or offline machine sets `GHOSTTY_SOURCE_DIR` and `GHOSTTY_ZIG_SYSTEM_DIR` as the crate documents; nothing in this repository depends on them being set.

### `policy-gates`

Each check is a pure function in `xtask::policy` from a repository root to a list of violations, and each has an integration test in `xtask/tests/` that runs it against the real tree **and** against synthetic trees under `xtask/tests/fixtures/policy/<check>/` that contain deliberate violations, so the gate is proven to catch what it claims to catch. `pub struct Violation { pub path: PathBuf, pub line: Option<usize>, pub rule: &'static str, pub detail: String }` is shared by all of them.

- `lexicon::check` parses every `.rs` file under `crates/` and `xtask/` with `syn` and visits every identifier the file **declares** — items, fields, variants, function and closure parameters, local bindings, generic parameters, lifetimes, labels, `use … as` renames — and every file and directory name under `crates/`, `xtask/`, `regression/` and `policy/`. An identifier is split into words on `_`, on `-`, and on case boundaries; a run of digits belongs to the word before it (`utf8`, `sha256`) and a token that is only digits is ignored (`x86_64` yields `x86`); leading and trailing underscores and the bare `_` are ignored; raw identifiers lose their `r#`. Every word must appear in the union of `policy/lexicon/*.txt`. A lexicon file must be lowercase `[a-z][a-z0-9]*` words, one per line, sorted, unique, and named after a task id under `docs/plans/`, so a dropped task cannot leave words behind that nothing owns.
- `literals::check` visits every integer and float literal in expression position outside `const` and `static` initializers and enum discriminants, in files that are not tests (`tests/`, `benches/`, `xtask/tests/fixtures/`), and reports any value other than `0` and `1`. An array-repeat count and an array-type length are expressions for this purpose.
- `length::check` counts lines of every file `git ls-files --cached --others --exclude-standard` reports and reports any over 1000, except `Cargo.lock`.
- `documentation::check` asserts every crate root contains `#![doc = include_str!("../README.md")]`, that the README exists and begins with `# <crate name>`, that every `.rs` file begins with a `//!` line, that every module file name under a crate's `src/` appears in that crate's README, and that every fenced code block in a README or in a doc comment carries a language tag that is not `rust` — an untagged fence in documentation is a doctest that `cargo nextest` never runs, which is documentation that can rot without failing anything.
- `links::check` resolves every relative Markdown link and reference target in every `.md` file against the tree, ignoring targets with a scheme or a leading `#`, and reports the ones that do not exist.
- `dependencies::check` runs `cargo metadata --format-version 1 --locked` through the bounded process runner and asserts the resolved package set equals, in both directions, the backticked names under `## Workspace` in `policy/dependencies.md`; that every version requirement in every workspace manifest, development dependencies included, is an exact `=` pin; that `iznik-protocol` declares no `[dependencies]`; and that no workspace crate has a `build.rs`.
- `blocking::check` asserts that under `crates/iznik-server/src/` and `crates/iznik-client/src/` no file imports or names `std::thread::sleep`, `std::io::Read`, `std::io::Write`, `std::io::stdin`, `std::io::stdout`, `std::io::stderr` or `std::process`, with one named exception that is part of the rule: `crates/iznik-server/src/pty/streams.rs`, whose whole purpose is to turn a pseudoterminal's blocking descriptor into an async stream on dedicated blocking threads.
- `unsafe_boundary::check` asserts every crate root except `crates/iznik-ffi/src/lib.rs` contains `#![forbid(unsafe_code)]`.
- `attributes::check` reports any `allow`, `expect` or `cfg(test)` attribute, inner or outer, anywhere under `crates/` and `xtask/` outside `xtask/tests/fixtures/`.

`policy/dependencies.md` is created here with one line per package — `` - `name` — justification. `` — where a transitive entry names the direct dependency it was reached from. `xtask policy` runs every check and prints violations as `path:line: rule: detail`.

### `gate-runner`

`iznik_harness::process` is the only place in this repository that spawns a child process synchronously — `xtask`, the fixture and the scenario runner all go through it — and every child has a deadline:

- `pub struct Deadline(pub Duration)`.
- `pub fn run(command: Command, deadline: Deadline, output: Output) -> Result<Completed, ProcessError>` spawns the child in its own process group (`CommandExt::process_group(0)`), polls it, and on expiry sends `SIGTERM` to the group, waits `TERMINATION_GRACE` (2 seconds), and sends `SIGKILL`. `Output::Inherit` streams the child's output through as it happens — Makina's idle watchdog reads that stream, so a gate must never buffer ten minutes of output and print it at the end — and `Output::Capture` collects it with a size cap for tests and for callers that parse it.
- `pub struct Completed { pub status: ExitStatus, pub stdout: Vec<u8>, pub stderr: Vec<u8>, pub elapsed: Duration }`.
- `pub enum ProcessError { Spawn { program, source }, TimedOut { program, deadline, stdout_tail, stderr_tail }, Failed { program, status, stderr_tail } }` — every variant names the program, and a timeout carries what the child said before it was killed.

`iznik_harness::deadline`: `pub fn run_capped<T: Send>(cap: Duration, context: &str, body: impl FnOnce() -> T + Send) -> Result<T, DeadlineError>` runs the body on a thread and gives up waiting at the cap — the thread is not killed, which is why anything that owns containers also registers with the reaper described under `regression-fixture` — and `pub fn wait_until(cap: Duration, interval: Duration, context: &str, condition: impl FnMut() -> bool) -> Result<(), DeadlineError>` polls; these are the two primitives every fixture wait is written with. `DeadlineError::Elapsed { cap, context }` names what was being waited for.

`xtask::gate` builds on the runner: `pub enum Gate { Format, Lint, Documentation, Test, Claims }` with `command()` and `deadline()` exactly as tabulated in `CONTRIBUTING.md` §2 — `.makina/config.toml` is a transcription of this table, and a test in `xtask/tests/gate_runner.rs` reads that file and asserts the two agree — and `pub fn check() -> Result<(), GateError>` runs them in order, printing one line per gate with its elapsed time and stopping at the first failure with the gate's name and the tail of its output. `xtask::doctor` probes each prerequisite in `CONTRIBUTING.md` §1 — the toolchain, `cargo-nextest`, `podman` with the `netavark` network backend, `zig`, the two musl cross-compilers, `git` — and prints what is missing with the command that installs it; `xtask check` runs the doctor first so a missing tool is reported by name instead of surfacing as a failed gate.

## 0002 — Wire Primitives

Both codecs are little-endian, length-prefixed and hand-written against `core` and `alloc` only. Each is pinned by a golden fixture — `crates/iznik-protocol/tests/fixtures/{frame,message}.jsonl`, lines of `{"description": "...", "message": {...}, "hex": "..."}` — asserted in both directions: encoding yields exactly those bytes, decoding yields exactly that message. The fixtures are the contract; the code is held to them. `iznik_testkit::golden::lines(path)` is the one JSONL loader every golden test uses.

### `frame-codec`

`crates/iznik-protocol/src/frame.rs`:

- `pub const MAXIMUM_PAYLOAD_LENGTH: u32 = 1 << 20;` and `pub const HEADER_LENGTH: usize = 5;`.
- `pub struct FrameHeader { pub length: u32, pub channel: u8 }` with `write(&self, out: &mut Vec<u8>)` and `read(bytes: &[u8]) -> Option<FrameHeader>`.
- `pub fn encode(channel: u8, payload: &[u8], out: &mut Vec<u8>) -> Result<(), FrameError>`.
- `pub struct FrameDecoder { buffer: Vec<u8>, consumed: usize }` with `pub fn push(&mut self, bytes: &[u8])` and `pub fn next(&mut self) -> Result<Option<Frame<'_>>, FrameError>` where `pub struct Frame<'decoder> { pub channel: u8, pub payload: &'decoder [u8] }`. The decoder resumes correctly across arbitrary read boundaries and compacts its buffer without ever copying a payload twice.
- `pub enum FrameError { Oversize { length: u32 } }` — the only decode failure a length-prefixed codec has; everything else is "need more bytes".

### `control-messages`

`crates/iznik-protocol/src/identity.rs` declares the newtypes every message uses: `PaneId(u64)`, `TabId(u64)`, `SessionId(u64)`, `CommandId(u64)`, `Generation(u64)`, `Sequence(u64)`, each `Copy`, ordered and hashable. `crates/iznik-protocol/src/capabilities.rs` declares `pub struct Capabilities { bits: u32 }` with the constants `ZSTD` and `RESUME`, `from_bits`, `bits`, and `unknown_bits` — unknown bits are preserved, never dropped.

`crates/iznik-protocol/src/message.rs` declares `pub const CHANNEL_CONTROL: u8 = 0;`, `pub const PROTOCOL_VERSION: u16 = 1;`, and the two enums exactly as tabulated in the root architecture §4.2: `pub enum ToServer { Hello, SnapshotRequest, Command, Subscribe, Unsubscribe, Resume, ScreenRequest, Credit, ChannelReleased, Input, Resize, Focus, Ping }` and `pub enum ToClient { Hello, Snapshot, Delta, CommandResult, PaneChannel, PaneDetached, Screen, Mark, Pong, Error }`. Each has a one-byte discriminant, `encode_to_server`, `decode_to_server`, `encode_to_client`, `decode_to_client`, and `pub fn pane_output(channel: u8, payload: &[u8]) -> Result<&[u8], MessageError>` which hands back a borrow, never a copy.

Two vocabularies that later plans need are pinned here so their encodings never move: `pub enum ErrorCode { ProtocolVersion, InputBacklog, UnknownPane, ChannelsExhausted, NotSubscribed }`, the `code` of `Error`, and `pub enum MarkKind { PromptStart, CommandStart, CommandExecuted, CommandFinished { exit_status: Option<i32> }, WorkingDirectory { path: String }, Title { text: String }, AlternateScreen { entered: bool } }`, the `kind` of `Mark { pane, sequence, kind }` — a mark is fully typed on the wire, not an opaque blob, because the server and the client both act on it. The session-model payloads — `Command::payload`, `Snapshot::payload`, `Delta::payload`, `CommandResult::payload` — are length-delimited opaque byte blobs here; plan 0003 defines their encodings without changing a byte this fixture pins. `pub enum MessageError { UnknownDiscriminant { channel, discriminant }, Truncated { discriminant, needed, available }, TrailingBytes { discriminant, count }, Utf8 { discriminant }, ControlChannel, Oversize { length } }`.

### `framed-link`

`crates/iznik-link/src/framed.rs`: `pub struct FramedLink<S: AsyncRead + AsyncWrite + Unpin>` with `new(stream: S)`, `pub async fn send(&mut self, channel: u8, payload: &[u8]) -> Result<(), LinkError>` — header and payload written in one vectored write, so a pane byte is copied from the caller's buffer to the socket and nowhere else — and `pub async fn next(&mut self) -> Result<Option<Frame<'_>>, LinkError>` over a `FrameDecoder`, returning `None` at a clean end of stream; `split(self) -> (FrameReader<S>, FrameWriter<S>)` over `tokio::io::split`, because every real user reads on one task and writes on another and one `&mut self` cannot do both; `into_parts(self) -> (S, Vec<u8>)` gives back the stream and any bytes already read past the last frame, which is how plan 0003 puts a compression layer under a link after the handshake without losing the peer's first compressed bytes. `pub enum LinkError { Io { source }, Frame(FrameError), Closed }`. Tested over `tokio::io::duplex` with the stream split at every boundary. Plan 0003 adds compression inside this crate; the server's connection loop, the client's channel and the test client all speak through this one type.

## 0003 — Test Instruments

### `vt-oracle`

`crates/iznik-testkit/src/vt.rs` wraps the `libghostty-vt` `Terminal`: `pub struct Vt` with `new(columns, rows)`, `feed(&mut self, bytes)`, `resize(columns, rows)`, `cell(column, row) -> Cell`, `row_text(row) -> String`, `screen_text() -> String`, `cursor() -> (u16, u16)`, `title() -> String`, `working_directory() -> String`, `scrollback_rows() -> usize`, and `snapshot() -> String`. `pub struct Cell { pub grapheme: String, pub foreground: Color, pub background: Color, pub bold: bool, pub italic: bool, pub underline: Underline, pub width: Width }` with `Width::{Narrow, Wide, Continuation}`. The binding at 0.2.1 exposes each of these directly — `title()`, `pwd()`, `scrollback_rows()`, `cursor_x()`/`cursor_y()`, and per-cell `grid_ref(Point::Screen { … })` with `cell()`, `style()` and `graphemes()` — and the task confirms and records the exact calls it used. `snapshot` is a fixed row-major rendering of graphemes followed by an attribute legend, versioned `vt/1`, with no addresses, no timestamps and no hash-ordered iteration, so it can be committed as a golden; the goldens live in `crates/iznik-testkit/tests/fixtures/vt/`. A `Terminal` is `!Send`: `Vt` is created, fed and read on one thread, and a test that needs it under a cap constructs it inside the capped body.

### `pty-harness`

`crates/iznik-testkit/src/pty.rs`: `pub struct PtyChild` with `spawn(program, arguments, columns, rows) -> Result<PtyChild, PtyError>`, `write(&mut self, bytes) -> Result<(), PtyError>`, `read_until_quiet(&mut self, quiet: Duration, cap: Duration) -> Result<Vec<u8>, PtyError>`, `resize(columns, rows)`, and `wait(self) -> Result<ExitStatus, PtyError>`; `pub enum ExitStatus { Exited(u32), Signalled(String) }`. `read_until_quiet` reads until the child has produced nothing for `quiet` or `cap` elapses, and a cap with nothing received is `PtyError::Timeout { received }` carrying the escaped bytes it did see — a read that comes back empty and succeeds is the worst failure in this domain and is made impossible here. Tests spawn `sh`, `cat` and small scripts, never the developer's login shell: a prompt that draws itself asynchronously is not quiet.

`crates/iznik-testkit/src/metrics.rs`: `pub fn resident_memory(process_id: u32) -> Result<u64, MetricsError>` and `pub fn cpu_time(process_id: u32) -> Result<Duration, MetricsError>` from `/proc/<pid>/statm` and `/proc/<pid>/stat`, the one implementation behind every memory ceiling, every "memory does not grow" assertion and every "costs no CPU" assertion in the workspace.

### `fidelity-corpus`

`crates/iznik-testkit/src/corpus.rs` builds the corpus every plan reuses, each construct separately addressable by name so a failure names it: a Kitty graphics payload; an OSC 8 hyperlink open and close; an OSC 52 clipboard write; the four OSC 133 marks; an OSC 7 working directory; OSC 0 and OSC 2 titles terminated by BEL and by ST; a CSI sequence split across two writes; a wide CJK glyph (written as a `\u{…}` escape, since `non_ascii_literal` is denied); a lone `0x1b` at a chunk boundary; synchronized-output begin and end; an alternate-screen enter and leave around content on both screens; a Kitty keyboard-protocol query; a cursor position query. `pub fn constructs() -> Vec<Construct { name, chunks: Vec<Vec<u8>> }>` is the hand-authored part, committed as the golden `crates/iznik-testkit/assets/fidelity-corpus.bin` (the constructs concatenated with a length-prefixed name before each) and asserted equal to the builder's output, because the compression dictionary is trained on these bytes and a silent change to them is a silent change to the wire. `pub fn generated(seed: u64, length: usize) -> Vec<u8>` produces the high-rate runs — pseudo-random printable text with line breaks from a seeded xorshift generator, 4 MiB for the byte-identity proofs and 64 MiB for the flood — which are never committed.

## 0004 — Regression Harness

The proof surface: `iznik-host` (an `sshd`, a login user with a `bash` login shell, `procps` for the census and `ncurses-bin` for `tic` and `infocmp`, nothing else) and `iznik-engine` (as bare as a freshly installed machine — `openssh-client` with `ssh-agent` and `ssh-add` deleted, no `sshd`, no `~/.ssh` beyond what the fixture generates) on a private Podman network, speaking real SSH with credentials generated per run that exist only inside the containers. Images carry no toolchain; the developer's machine builds static musl binaries into a staging directory the fixture mounts read-only at `/iznik`.

### `regression-images`

`regression/images/Containerfile.engine` and `Containerfile.host`, base pinned by digest. In the host image `sshd` runs as root on port 22 with `UsePAM no`, exactly as on a real host, and logins land in the unprivileged user `iznik` (uid 1000, `/bin/bash`, a minimal `.bashrc` that sets `PS1='$ '` so a prompt is predictable bytes, and a `.bash_profile` that sources it, because a login shell reads the profile and not the rc); the engine image runs entirely as uid 1000. `iznik_harness::images`: `pub fn image_tag(containerfile: &Path) -> Result<String, ImagesError>` is the SHA-256 of the file's content, so the tag is an honest cache key; `pub fn ensure_images(deadline: Deadline) -> Result<Images, ImagesError>` builds only what is missing, under `IMAGE_BUILD_DEADLINE` (15 minutes, because the first build pulls a base image); `xtask regression images` is the subcommand.

### `regression-fixture`

`crates/iznik-harness/src/fixture.rs`: `pub struct Fixture` with `start(options: FixtureOptions) -> Result<Fixture, FixtureError>` — `FixtureOptions { hosts: usize, staged: PathBuf, ssh_ready_cap: Duration (SSH_READY_CAP, 30 seconds), ssh_ready_interval: Duration (SSH_READY_INTERVAL, 100 milliseconds), container_timeout: Duration (CONTAINER_TIMEOUT, 10 minutes) }` — `exec(&self, container: &str, command: &str, deadline: Duration) -> Result<Completed, FixtureError>` running as the unprivileged user, `fault(&self, fault: Fault) -> Result<(), FixtureError>` for `Fault::{DisconnectNetwork { container }, ReconnectNetwork { container }, PauseProcess { container, process }, ResumeProcess { container, process }, KillProcess { container, process }}` where `process` is `Process::{Id(u32), IdFile(PathBuf)}` — a scenario cannot know a process id in advance, so a step writes `$$` to a file and the fault reads it — `host_alias(index) -> String`, and `elapsed_start() -> Duration`, the measured start time the fixture's test prints and asserts under `FIXTURE_START_CEILING`.

Provisioning: a per-run network; an ed25519 key pair generated inside the engine; host keys generated inside each host; `authorized_keys`; a generated `~/.ssh/config` in the engine with one alias per host whose `HostName` is the container's name, resolved by the network's DNS, never an address — a container reconnected after a fault may come back with a different one — and `ConnectTimeout 3` so a fixture command against a disconnected host fails in seconds; readiness through `wait_until` against `ssh -o BatchMode=yes <alias> true` from the engine. The staging directory is mounted read-only at `/iznik`.

**Orphans are made impossible to keep, not merely unlikely.** Teardown on drop covers success, panic and the cap; it does not cover `SIGKILL` from nextest's deadline or Makina's watchdog, and five containers from the previous incarnation were found still running sixteen hours after the process that started them died. Three mechanisms, all cheap: every container is started with `--timeout <container_timeout>` so podman itself kills it when its run is long dead; every container and network carries the labels `iznik.run=<prefix>` and `iznik.owner=<pid>`; and `Fixture::start` first lists everything labelled `iznik.owner` and removes what belongs to a process that no longer exists. `xtask regression reap` removes everything with the label, for a person.

`iznik_harness::staging::stage(deadline) -> Result<PathBuf, StagingError>` builds `iznik-server`, `iznik-regression` and `iznik` — only those three; `iznik-ffi`'s `cdylib` cannot be built for a `crt-static` musl target and nothing in a container needs it — with `--profile regression --target x86_64-unknown-linux-musl`, hashes the three binaries, and returns `target/regression-staging/<hash>/` laid out as `bin/{iznik-server,iznik-regression,iznik}` and `distribution/x86_64-unknown-linux-musl/iznik-server` (the same binary, where the bootstrap looks for artifacts), a no-op when the hash already exists. `IZNIK_STAGED=<dir>`, when set, is used instead of building: `xtask claims verify` stages once and sets it for the tests it runs, so parallel scenarios never contend for the cargo lock. `xtask regression stage` is the by-hand form.

### `scenario-driver`

A scenario is TOML data under `regression/scenarios/<task-id>/<name>.toml`, and **every scenario is a test**: `crates/iznik-regression/tests/regression_scenarios.rs` is a `harness = false` test binary over `libtest-mimic` that enumerates every scenario file at startup and registers one ignored test per scenario named `scenario::<task-id>::<name>`. So scenarios are listed by `cargo nextest list`, run in parallel by nextest with its deadlines and its per-test report, selected by filterset, and there is one runner for everything in this repository. `xtask regression run` does not exist; the command is a nextest filter, and `docs/notes/claims.md` shows it.

```toml
name = "both-containers"
claims = ["scenario-driver-both-containers"]
driver = "engine"
budget_seconds = 60
exclusive = false

[setup]
hosts = 1
files = []

[[steps]]
id = "host-hostname"
container = "host0"
run = "hostname"
timeout_seconds = 10

[[steps]]
id = "drop-the-link"
container = "host0"
fault = "disconnect-network"
timeout_seconds = 10

[[expect]]
step = "host-hostname"
exit = 0
stdout_equals = "host0"
```

`iznik_harness::scenario` is the format: every table denies unknown keys; `timeout_seconds` on every step and `budget_seconds` on every scenario are mandatory, and a budget over `MAXIMUM_SCENARIO_BUDGET_SECONDS` does not parse; a step's kind is exactly one of the keys `run`, `fault`, `pane`, `client`, `transport`, `channel`, `probe`, `upload`, `bootstrap`, `manager`, and only the envelope — `id`, `container`, `timeout_seconds` — is parsed here, the kind's own table being parsed by the module that executes it. `iznik_harness::report` is the record: `{"scenario", "step", "exit", "timed_out", "duration_milliseconds", "stdout", "stderr"}` with `stdout` and `stderr` as UTF-8 with lossy replacement — binary output goes to files, never into a record. `iznik_harness::runner` executes a scenario: stage, start the fixture, copy `files` into the driver's home, run each step in order, evaluate `expect`, enforce the budget, record the overhead. A `fault` step is executed by the runner through the fixture. Every other step is executed **inside the container it names** by `podman exec --user iznik <container> /iznik/bin/iznik-regression step`, which reads the step as TOML on stdin, executes it in its own process group under its deadline — a `run` step's command through `sh -c` — and prints one NDJSON record; at the deadline the record carries `timed_out = true` and the partial output, and the process group is gone. A `run` step on the engine that wants the host says so itself: `run = "ssh host0 hostname"`. Assertions are `exit`, `stdout_equals`, `stdout_contains`, `stdout_matches`, the `stderr_` equivalents, and `duration_under_seconds`; assertions that need the emulator — a captured stream reproducing a screen through the oracle — are actions inside `pane` and `client` steps, evaluated in the container, so the harness crate never links the emulator.

`iznik-regression` is the driver: `step/mod.rs` holds the dispatcher, `Context` (the driver's paths and the staged directory) and `StepError`, `step/run.rs` the one kind this task implements, and the other eight files stubs whose `execute` returns `StepError::Unsupported { kind }` until their plan fills them — the only stubs in the repository that are functions rather than documentation, because the dispatcher must be complete on the day it is written.

### `claims-registry`

One file per task, `regression/claims/<task-id>.toml`, each `[[claim]]` with a stable `id`, a one-sentence present-tense `statement`, and exactly one proof: `scenario = "<name>"` naming `regression/scenarios/<task-id>/<name>.toml`, or `test = "<package>::<binary>::<test name>"` with a mandatory one-sentence `because` saying why a container adds nothing. Optional `platform` and `profile` mark a claim that can only be established on a named operating system or cargo profile; such a claim is reported as deferred, never as proven, when it cannot run here, and a `profile = "regression"` proof is run in its own nextest invocation with `--cargo-profile regression`.

`xtask::claims`: `registry::load(root) -> Result<Registry, RegistryError>` parses and validates every file — duplicate ids, a `test` without `because`, both proofs or neither, a file whose name matches no task under `docs/plans/`, a scenario file naming a claim declared nowhere, a scenario proof naming a file that does not exist. `selection::select(root, selection) -> Result<Vec<TaskId>, SelectionError>`: `Selection::Tasks(ids)` is explicit; `Selection::Everything` is every task with a claims file; `Selection::CurrentBranch` is derived from `git diff --name-only <merge base with develop>` — the tasks whose claim files changed — and it enforces the rule that gives the registry its teeth: if that diff touches product or tooling code (a path under `crates/*/src/`, `crates/*/benches/` or `xtask/src/`) and changes no claims file, and `regression/claims/` exists, the selection fails naming the rule, because a task that changes code without declaring what it claims is the case the registry exists to catch. On `develop` itself the diff is empty and the selection is empty; `coverage` is the run for that. `verify::verify(root, selection, deadline) -> Result<Report, VerifyError>` builds one nextest filterset from the selected proofs — `test(=scenario::<task>::<name>)` for scenarios, `package(<p>) & binary(<b>) & test(=<t>)` for tests — runs `cargo nextest run --locked --run-ignored all --profile claims -E <filter>` with one `--package` per package the proofs name, so nothing else is built or listed, through the process runner with output inherited, reads `target/nextest/claims/claims.xml`, and reports every claim as proven, failed, missing (no test case appeared, which is an unproven claim) or deferred. `xtask claims verify [--task <id>]…` and `xtask claims coverage` are the subcommands; both take `--root` so the gate's own tests can build a deliberately broken registry without editing a committed file. `docs/notes/claims.md` explains the mechanism and is the worked example every task copies.

**The bootstrap is exempt, and only the bootstrap.** Every code task in this plan lands before `claims-registry` — the registry depends on all of them — and proves itself with its own tests, because the registry cannot be a precondition for the tasks that build it. `claims-registry` self-applies: its own claims are proven by its own scenarios. From the commit that creates `regression/claims/`, every task in every plan that changes code fails the gate on a claim without a registered proof.

## 0005 — Documentation

### `foundations-documentation`

Brings the root `README.md`, `ARCHITECTURE.md` and `CONTRIBUTING.md` and every crate `README.md` into line with what landed: the real commands, the real module maps, the real gate names. The crate READMEs are crate documentation, so each documents every module the crate has. Nothing in the repository may still name the previous incarnation's crates.
