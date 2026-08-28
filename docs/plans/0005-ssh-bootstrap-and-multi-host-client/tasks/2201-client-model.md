---
id: client-model
title: "Client Model"
workstream: "0022"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-client/src/model.rs"
  - "crates/iznik-client/tests/client_model.rs"
  - "regression/claims/client-model.toml"
  - "policy/lexicon/client-model.txt"
status: done
merged_as: ""
---
# Client Model

Everything the client knows, with no I/O: one host model per host plus what the server does not hold — which panes this client subscribes to, the byte each subscription has reached, its focus, and the commands it is waiting to hear about. The cursor per subscription is what makes resume possible.

**Steps:**

1. Implement `crates/iznik-client/src/model.rs` — `ClientModel`, `HostView`, `Subscription`, `PendingCommand` and their accessors — exactly as the architecture's `client-model` section specifies.
2. Write `crates/iznik-client/tests/client_model.rs`.
3. Declare this task's claims in `regression/claims/client-model.toml` as `test` proofs with their `because`.

**Tests:**

- Hosts are independent: adding, replacing and removing one host's view leaves the others byte-identical.
- A subscription records its channel and cursor; advancing the cursor by a byte count is exact and never moves backwards.
- Pending commands are ordered by submission and addressable by id.
- The model is `Default` empty and validates every host model it holds.

- **Done when:** `timeout 600 cargo nextest run --package iznik-client --test client_model` passes every case above, `timeout 900 cargo xtask claims verify --task client-model` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
