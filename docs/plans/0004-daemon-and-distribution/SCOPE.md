# Scope — Plan 0004

> One long-lived process on the remote host that outlives every connection to it, reachable through a relay that starts it on first use, measured with committed numbers, and shipped as one static binary.

## Why this plan

Everything so far runs inside one test's lifetime. This plan builds the thing that actually lives on the server: a daemon that owns the session registry and every pane, accepts any number of clients over a unix socket, and is completely indifferent to whether anyone is currently attached. That indifference is the whole product — a dropped link costs a reconnect, not a session — and it rests on one property this plan proves inside the host container over real SSH: a daemon started by an SSH command survives the SSH session ending.

`iznik-server --stdio` is the other half. Plan 0005's bootstrap will run exactly that over SSH; here it is built and proven: connect to the daemon, start it if it is not running, relay bytes both ways, exit when either side closes. With it, the scenario driver on the engine container can attach to the daemon on the host container through real SSH before the client engine exists, which is how this plan's claims are proven end to end.

The integration harness — a real daemon and a protocol-speaking test client in one process tree — is what every later integration test is written against, and the performance baseline is where "fast" becomes a number: keystroke latency at idle and under a flood, throughput, memory with fifty panes, startup time, committed with the machine described and asserted as generous ceilings so the regression test survives a noisy machine.

## In scope

- **0016 — Daemon.** The protocol test client every later test speaks through; the per-client connection loop with handshake, dispatch, per-client focus and cleanup on disconnect, proven over a socket pair; runtime paths, the single-instance lock, socket hygiene, detaching from the launching shell, idle shutdown and size-capped rotating logs; and the `--stdio` relay that starts the daemon on first use.
- **0017 — Measurement.** The stack — a daemon in-process or as a binary — the regression driver's `client` step, and the committed performance baseline with its ceilings, measured under the profile the containers run.
- **0018 — Distribution.** Stripped, reproducible, statically linked `iznik-server` artifacts for both Linux musl targets with a checksum manifest, proven to run inside the host container; Darwin artifacts behind a human gate.
- **0019 — Documentation.** The daemon's documentation, the baseline, and the crate READMEs brought into line.

## Out of scope

No SSH is spoken by product code; the relay speaks only to its standard streams and the socket, and the fixture's `ssh` is what carries it across the network in this plan's scenarios. The client engine, the bootstrap that uploads these artifacts, and multi-host management are plan 0005. Hot upgrade of a running daemon by descriptor passing is deferred; this plan makes the daemon refuse to be replaced while it holds panes and reports the count.
