# Architecture — Plan 0006

> The concrete deltas, by symbol. The system is described in the root [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) §7; the rules and the way of working are in [`CONTRIBUTING.md`](../../../CONTRIBUTING.md). Read both before your first task. Every task here declares its claims in `regression/claims/<task-id>.toml`; a `test` proof carries its `because`.

## Two rules that decide most of this plan

**The consumer cannot read this repository.** Every name, every ownership rule and every threading guarantee must be legible from the header and `docs/CLIENT.md` alone. The caller obligations are declared in the header by workstream 0027 and published by `docs/CLIENT.md` in workstream 0030 — in that order, so a task in 0027 states an obligation in the header as an `Obligation:` line and its module documentation, and `client-contract` later publishes it verbatim and cross-checks.

**`iznik-ffi` is the one crate where `unsafe` exists.** Every `unsafe` block carries a `// SAFETY:` comment naming the invariant it relies on and the caller obligation from the header that guarantees it, one operation per block, and the crate exposes nothing that is not `extern "C"` with a stable name.

## The threading contract

Decided once because it governs every signature below. `iznik-client` owns its own runtime. The FFI exposes a single-threaded facade: every callback is delivered on one dedicated callback thread, never concurrently, and never re-entrantly with respect to a call the application is making. The application hops to its main actor for UI work; iznik does not pretend to know about run loops. Every `iznik_*` function is safe to call from any thread; calls are serialized internally by a lock the callback thread never holds while it is inside a callback, so a call made from inside a callback is permitted and does not deadlock, because the natural way to write a client is to submit a command in response to an event. The one exception is an obligation: `iznik_client_free` must not be called from inside a callback, because it joins the callback thread, and the header says so.

## 0027 — FFI Surface

### `ffi-surface`

`crates/iznik-ffi/src/lib.rs`, `error.rs`, `model.rs`:

- `iznik_client *iznik_client_new(const iznik_configuration *configuration, iznik_error *error)` and `void iznik_client_free(iznik_client *)`. `iznik_configuration` carries `runtime_directory`, `artifacts_directory`, `askpass_program` and `log_path` as UTF-8 C strings, any of which may be null for the default.
- `int32_t iznik_host_add(iznik_client *, const char *alias, iznik_error *)`, `iznik_host_remove`, `iznik_host_reconnect`, `iznik_host_upgrade(iznik_client *, const char *alias, bool force, iznik_error *)`, `iznik_host_uninstall`. An alias is what the user typed; `unix:<path>` names a local daemon socket, which is how the tests, the smoke program and one day a Mac attaching to itself reach a daemon with no SSH.
- Observation by callback, never by polling: `void iznik_set_event_callback(iznik_client *, iznik_event_callback, void *context)` invoked with `const iznik_event *` — `kind` (host state changed, snapshot, delta, command result, mark, notification), `host`, and a `payload` with `payload_length` in `iznik/1`'s own encoding, so the application decodes with the same schema the protocol uses and there is one format, not two.
- `int32_t iznik_command(iznik_client *, const char *host, const uint8_t *command, size_t length, uint64_t *out_command_id, iznik_error *)` — commands go in encoded, ids come back, confirmation or rollback arrives on the event callback; adding a command later does not change the header.
- `iznik_error { int32_t code; iznik_layer layer; const char *message; }` where `layer` is one of `transport`, `bootstrap`, `server`, `protocol`, `client`, and `message` is UTF-8 the application may display verbatim.
- **Ownership, stated in the header and repeated in every module:** every buffer iznik passes to a callback is valid only for that callback's duration; an application that needs it longer copies it. Every buffer the application passes in is copied by iznik before the call returns. There is exactly one exception and it is the pane output path below.

### `pane-byte-pipe`

`crates/iznik-ffi/src/pane.rs`, shaped for a libghostty consumer:

- `int32_t iznik_pane_attach(iznik_client *, const char *host, uint64_t pane, iznik_pane_callbacks callbacks, void *context, iznik_error *)` and `iznik_pane_detach`. `iznik_pane_callbacks` holds `output` — `void (*)(void *context, const uint8_t *bytes, size_t length)`, invoked with pane bytes for direct handoff into a surface, the one borrow that is not copied; `screen` — `void (*)(void *context, uint64_t sequence, uint16_t columns, uint16_t rows, const uint8_t *bytes, size_t length)`, which obliges the application to reset its surface to that size and feed it those bytes before any further output; `mark`; and `detached`.
- Flow control is explicit and mandatory: `void iznik_pane_credit(iznik_client *, const char *host, uint64_t pane, uint64_t bytes)`. The application returns credit as its surface consumes, which is what connects the server's scheduler to the rate a surface can absorb; an application that never returns credit stalls only its own pane, and that is tested from this side too.
- `iznik_pane_input(..., const uint8_t *bytes, size_t length)` with the same one-message atomicity the server guarantees, so bracketed paste survives; the application forwards its emulator's query responses through it exactly like keystrokes. `iznik_pane_resize(..., uint16_t columns, uint16_t rows)` — the application decides size, and every client observes the result as a `PaneResized` delta. `iznik_pane_focus(...)` names the pane the user is looking at.

## 0028 — Build and ABI Stability

### `static-library-and-header`

`cbindgen.toml` at the root configures generation; `xtask header` writes `include/iznik.h` and `xtask/tests/header_golden.rs` compares it against the committed copy, so an ABI change is a deliberate edit to the golden in the same commit with a version bump. `cargo build --package iznik-ffi --profile regression` produces `libiznik.a` and `libiznik.so` on the host platform in seconds — the release profile's fat LTO is for shipped artifacts, and a smoke test needs a library, not a small one; the Darwin library is built by the application's build or the gated Darwin workflow. `xtask/tests/regression_ffi_smoke.rs`, `#[ignore]` because it builds the library, reads the system libraries the static library needs from `cargo rustc --package iznik-ffi --profile regression -- --print native-static-libs`, compiles `xtask/tests/fixtures/ffi/smoke.c` with the system C compiler against the header and `libiznik.a`, and runs it against an in-process `Stack` through a `unix:` alias passed as its one argument: create a client, add the host, create a session, attach a pane, type a line, receive its bytes through the output callback, return credit, detach, free — a Swift developer's first afternoon, proven in C. Symbols are asserted where the toolchain decides them: `nm -D --defined-only libiznik.so` exports only `iznik_` names, and `nm --defined-only libiznik.a` defines every `iznik_` function the header declares — a static archive carries every object of every dependency and cannot hide them, so the export assertion belongs to the shared library.

## 0029 — Diagnosis

### `plumbing-commands`

`iznik probe <host>`, `iznik state <host>`, `iznik tail <host> <pane>`, `iznik benchmark <host>` and `iznik uninstall <host>` in `crates/iznik-cli/src/`: plumbing that prints structured output — one JSON object per line, built by hand in `crates/iznik-cli/src/output.rs`, the one place the binary writes, because the protocol crate carries no serialization dependency. Explicitly not an interface: no screen, no interactivity, no attempt to be a terminal. `tail` prints a pane's bytes as they arrive and exits on a signal; `benchmark` runs the keystroke round trip against a host and prints the distribution. A `unix:` alias reaches a local daemon, which is how every local test drives them.

### `diagnostics-bundle`

`iznik doctor <host>` in `crates/iznik-cli/src/doctor.rs` collects into one JSON artifact: the client version and build; the SSH configuration iznik would use for that host from `ssh -G`, never from parsing; the probe result; the installed server's version and its log tail; the runtime paths on both sides; the host's connection state and its recent transitions; the compression numbers negotiated; a measured keystroke round trip. Secrets are redacted by construction — key material, tokens, passphrases and environment values are never collected, and the redaction is tested rather than assumed. When something is wrong in a system spanning five layers, this is the command that says which one.

## 0030 — Handoff

### `client-contract`

`docs/CLIENT.md` is the normative contract, and where it and the implementation disagree the contract wins and the implementation is the bug. It covers: the threading contract in full, including the permitted call from a callback and the one forbidden one; buffer ownership with the pane-output exception; the credit protocol and what happens when the application is slow; geometry — the application decides, every client observes, the last resize wins; reconnect semantics — what the application keeps, what it discards, and what a `screen` callback obliges it to do; query responses — the application's emulator answers when it is attached, the server when nobody is, and the application forwards its emulator's responses through `iznik_pane_input`; the marks, including the alternate-screen events, and what can be built from them; the `unix:` alias; the upgrade policy and the `askpass` hook; the diagnostics command; and the reserved shape for predictive local echo so adding it later is not an ABI break. `xtask/tests/contract_matches_header.rs` asserts every function and type the contract names exists in `include/iznik.h` and the reverse, and every `Obligation:` line of the header appears verbatim in the contract.

### `soak-and-release`

`xtask soak --duration <minutes> --warmup <minutes>` runs the end-to-end stack against two fixture hosts — `SOAK_DURATION` (six hours) and `SOAK_WARMUP` (ten minutes) by default — with link drops on a schedule, panes created and closed continuously, and a flood on one pane, sampling resident memory on both sides every minute and printing each sample as it is taken; it fails if memory grows by more than `SOAK_GROWTH_CEILING_PER_HOUR` after the warmup or if any reconnection loses a byte. The task commits a ten-minute run with a two-minute warmup to `docs/notes/soak.md`, which is what fits inside a task's wall clock beside its gates; `xtask/tests/regression_soak.rs` runs a one-minute soak, `#[ignore]`; `docs/notes/release-checklist.md` is what a release runs through, and its first item is a six-hour soak run by a person, with the report replaced.

## 0031 — Documentation

### `handoff-documentation`

The final pass: the root `README.md` gains "Building the macOS application against this repository", the root `ARCHITECTURE.md` and `CONTRIBUTING.md` say what is true after six plans, and every crate README documents every module. Nothing in the repository describes something that does not exist.
