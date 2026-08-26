---
id: test-client
title: "Test Client"
workstream: "0016"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-testkit/src/client.rs"
  - "crates/iznik-testkit/tests/client.rs"
  - "regression/claims/test-client.toml"
  - "policy/lexicon/test-client.txt"
status: planned
merged_as: ""
---
# Test Client

A client that speaks the real protocol over any duplex stream — the client the macOS application will resemble, minus the surface — landed before anything that needs one, so that no test in this plan or the next hand-rolls a second protocol client. It is proven against the goldens and a scripted server side, in milliseconds, before a daemon exists.

**Steps:**

1. Implement `crates/iznik-testkit/src/client.rs` — `TestClient` with every method the architecture's `test-client` section lists, `Received`, `ClientError`, compression engagement in `hello`, and `auto_credit` — over `FramedLink`.
2. Write `crates/iznik-testkit/tests/client.rs` with a scripted server side over `tokio::io::duplex`.
3. Declare this task's claims in `regression/claims/test-client.toml` as `test` proofs with their `because`.

**Tests:**

- Every method sends exactly the frame the `message.jsonl` golden pins for that message, asserted byte for byte against the fixture line.
- `next` decodes every `ToClient` variant and yields pane bytes with their channel; `bytes_of` concatenates a pane's bytes in order; `deltas` holds every delta received in generation order.
- `next(deadline)` returns `ClientError::Deadline` rather than waiting when nothing arrives, within the deadline plus a stated slack, and every deadline in this file is under a second.
- `auto_credit` sends a `Credit` frame equal to every pane payload received and a `ChannelReleased` for every `PaneDetached`; without it, neither is sent.
- Compression: with both `Hello`s carrying `ZSTD`, frames after the handshake are compressed on the wire and decode correctly; with one not, they are plain.

- **Done when:** `timeout 600 cargo nextest run --package iznik-testkit --test client` passes every case above, `timeout 900 cargo xtask claims verify --task test-client` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
