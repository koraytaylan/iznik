---
id: client-connections
title: "Client Connections"
workstream: "0016"
kind: task
depends_on:
  - test-client
gated: false
touches:
  - "crates/iznik-server/src/connection.rs"
  - "crates/iznik-server/tests/client_connections.rs"
  - "regression/claims/client-connections.toml"
  - "policy/lexicon/client-connections.txt"
status: done
merged_as: ""
---
# Client Connections

Any number of clients, each with its own multiplexer and its own focus, all seeing the same model. A client that disconnects — cleanly or by having its laptop closed — leaves no subscription, no channel and no change to any session behind. The loop is written over any duplex stream, so it is proven here over a socket pair with no daemon, no socket file and no process.

**Steps:**

1. Implement `crates/iznik-server/src/connection.rs` — `serve`, the handshake with its version refusal, compression engagement through the link's parts, the dispatch of every `ToServer` message with `MultiplexerError` mapped to `ErrorCode`, the single writer through the pump, and cleanup on disconnect — exactly as the architecture's `client-connections` section specifies.
2. Write `crates/iznik-server/tests/client_connections.rs` over `tokio::net::UnixStream::pair()` with `TestClient::over` on one end and `serve` on the other, the registry defaulting to `sh`.
3. Declare this task's claims in `regression/claims/client-connections.toml` as `test` proofs with their `because`; the same behavior through the daemon, the relay and over SSH is proven by `daemon-lifecycle`, `stdio-relay` and `integration-harness`.

**Tests:**

- Handshake: a first frame that is not `Hello` closes the connection; a wrong protocol version is answered with `Error { ProtocolVersion }` and closed; a correct one receives the server's `Hello`.
- Two clients: a session created by one appears in the other's deltas with the same generation numbers; a resize by one arrives at both as `PaneResized`.
- Input reaches the pane: bytes sent by a client are echoed by the shell and arrive on the subscribed channel of another client.
- Backlog is reported: input beyond the pane's pending limit against a stopped shell is answered with `Error { InputBacklog }` naming the pane, and the connection stays open.
- Refusals: a `Subscribe` for a pane that does not exist is answered with `Error { UnknownPane }`; a `Credit` for a channel that is not subscribed with `Error { NotSubscribed }`; the connection stays open.
- Garbage closes: an undecodable frame closes only that client's connection; the other client continues.
- Cleanup on disconnect: after a client with two subscriptions drops without unsubscribing, the panes' subscriber counts are zero — observable because a cursor position query is answered again — and a new client receives fresh channels from 1.
- Compression negotiated: with both `Hello`s advertising `ZSTD`, frames after the handshake are compressed; with one not, they are not.
- One writer: a thousand `Ping`s interleaved with a flood on a subscribed pane produce a well-formed frame stream with every `Pong` present, asserted by decoding everything the client received.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test client_connections` passes every case above, `timeout 900 cargo xtask claims verify --task client-connections` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
