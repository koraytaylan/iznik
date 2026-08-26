---
id: ffi-surface
title: "FFI Surface"
workstream: "0027"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-ffi/src/lib.rs"
  - "crates/iznik-ffi/src/error.rs"
  - "crates/iznik-ffi/src/model.rs"
  - "crates/iznik-ffi/tests/ffi_surface.rs"
  - "regression/claims/ffi-surface.toml"
  - "policy/lexicon/ffi-surface.txt"
status: planned
merged_as: ""
---
# FFI Surface

The C ABI the application calls: lifecycle, hosts, one event callback carrying the protocol's own encoding, command submission, and errors that say which layer failed. The threading and ownership contracts are declared here, in the header and the module documentation, and published later — in that order.

**Steps:**

1. Implement `crates/iznik-ffi/src/lib.rs`, `error.rs` and `model.rs` — every function, type and constant the architecture's `ffi-surface` section lists, the callback thread, the internal serialization of calls with the lock released around every callback, the re-entrancy guarantee, and an `Obligation:` line in the header comment of every function that has one — with a `// SAFETY:` comment on every `unsafe` block naming the caller obligation it relies on.
2. Write `crates/iznik-ffi/tests/ffi_surface.rs`, calling the `extern "C"` functions from Rust against an in-process `Stack` through a `unix:` alias.
3. Declare this task's claims in `regression/claims/ffi-surface.toml` as `test` proofs with their `because`.

**Tests:**

- Lifecycle: `iznik_client_new` with a null configuration uses defaults; with an invalid runtime directory it returns null and fills the error with layer `client`; `iznik_client_free` on a live client with hosts tears everything down within a second.
- Events: after `iznik_host_add` on a `unix:` host, the callback receives a host state change, then a snapshot whose payload decodes with `iznik-protocol`, then deltas in generation order.
- Commands: an encoded `CreateSession` through `iznik_command` returns an id, and the callback receives a command result with that id and then the session's deltas.
- Threading: every callback arrives on one thread, asserted by thread id; a callback that itself calls `iznik_command` does not deadlock and its result arrives; calls from four threads at once are serialized without error.
- Ownership: a callback payload copied during the callback survives; the buffer passed to `iznik_command` may be freed as soon as the call returns.
- Errors: every `iznik_error` carries a non-empty UTF-8 message and a layer, asserted across every error path the surface has; an alias that is neither reachable nor `unix:` fails at layer `transport` with the host named.

- **Done when:** `timeout 600 cargo nextest run --package iznik-ffi --test ffi_surface` passes every case above, `timeout 900 cargo xtask claims verify --task ffi-surface` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
