---
id: adopted-state
title: "Carry the daemon's state across a replacement as versioned bytes"
workstream: "0001"
kind: task
depends_on: []
gated: false
touches:
  - crates/iznik-protocol/src/lib.rs
  - crates/iznik-server/Cargo.toml
  - crates/iznik-server/src/adopt.rs
  - crates/iznik-server/src/daemon/mod.rs
  - crates/iznik-server/src/lib.rs
  - crates/iznik-server/tests/adopted_state.rs
  - policy/lexicon/adopted-state.txt
  - regression/claims/adopted-state.toml
status: planned
merged_as: ""
---
# Carry the daemon's state across a replacement as versioned bytes

What a replacement must know is what the old daemon held: the model, and for
every pane its master descriptor number, its child's process id, its absolute
sequence, its history ring and its terminal modes. That is a value, and a value
is a codec — versioned, refused rather than guessed across versions, and pure
over bytes.

**Steps:**

1. `crates/iznik-protocol`: expose the wire primitives `iznik-server` needs to
   encode a state of its own with the workspace's one codec
   (`put_bytes`/`read`, `put_count`, `put_optional`), or state plainly why the
   server cannot and the state carries its own small codec.
2. `crates/iznik-server/src/adopt.rs`: `AdoptedState { version, model, panes }`
   and `AdoptedPane { pane, master_fd, process_id, sequence, ring, termios }`,
   `ADOPTED_STATE_VERSION`, `encode_adopted` and `decode_adopted` with
   `AdoptError` naming what was wrong.
3. `crates/iznik-server/src/daemon/mod.rs`: `RuntimePaths.state`, beside the
   socket and the lock under the `0700` runtime directory, never on the client
   protocol.
4. Write the tests in `crates/iznik-server/tests/adopted_state.rs`: a generated
   state round-trips exactly; another version is refused naming it; a truncated
   or oversized field is refused; a ring carrying bytes no terminal would print
   still round-trips.
5. Declare the claims in `regression/claims/adopted-state.toml`.

**Tests:**

- A generated state encodes to exactly its bytes and decodes back to itself.
- Another version, a truncated field and trailing bytes are each refused by
  name, and nothing is guessed.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test adopted_state` passes every case above, `timeout 900 cargo xtask claims verify --task adopted-state` reports every claim proven.
