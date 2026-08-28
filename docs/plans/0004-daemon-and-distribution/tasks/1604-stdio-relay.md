---
id: stdio-relay
title: "Stdio Relay"
workstream: "0016"
kind: task
depends_on:
  - daemon-lifecycle
gated: false
touches:
  - "crates/iznik-server/src/relay.rs"
  - "crates/iznik-server/tests/stdio_relay.rs"
  - "regression/claims/stdio-relay.toml"
  - "regression/scenarios/stdio-relay/**"
  - "policy/lexicon/stdio-relay.txt"
status: done
merged_as: ""
---
# Stdio Relay

`iznik-server --stdio` is the only command the bootstrap will ever run on a host: it finds the daemon or starts it, then relays its standard streams to the socket until either side closes. Everything about starting the daemon on first use lives here, which is why plan 0005 needs to know nothing about it.

**Steps:**

1. Implement `crates/iznik-server/src/relay.rs` — connect, start-if-absent with `socket_ready_cap`, `copy_bidirectional`, exit codes and messages — exactly as the architecture's `stdio-relay` section specifies, filling the `--stdio` stub.
2. Write `crates/iznik-server/tests/stdio_relay.rs` with `env!("CARGO_BIN_EXE_iznik-server")` and `TestClient::over` the relay's standard streams.
3. Declare this task's claims in `regression/claims/stdio-relay.toml` and author the scenarios under `regression/scenarios/stdio-relay/` — `starts-the-daemon` and `relays-over-ssh` — the latter driving `ssh host0 /iznik/bin/iznik-server --stdio` from the engine with raw `iznik/1` frames written by a `run` step and the handshake reply asserted.

**Tests:**

- Against a running daemon: bytes written to the relay's stdin arrive at the socket and replies arrive on its stdout, byte-identical in both directions, through a full handshake and a `Ping`/`Pong`.
- Starts the daemon: with no daemon running, the relay starts one, the socket appears within the cap, and the handshake succeeds; the daemon outlives the relay.
- Exits with its client: when stdin closes the relay exits 0 within a second; when the daemon closes the socket the relay exits 0.
- Fails audibly: with a runtime directory that cannot be created, the relay exits non-zero with a message naming the path.
- In the container: over real SSH from the engine, a `Hello` written to `ssh host0 /iznik/bin/iznik-server --stdio` is answered with the server's `Hello`, and the daemon it started is alive after the SSH session ends.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test stdio_relay` passes every case above, `timeout 900 cargo xtask claims verify --task stdio-relay` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
