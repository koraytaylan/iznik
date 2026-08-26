# iznik

A remote terminal system with a native macOS front end.

The macOS application connects to a host over your own SSH configuration,
installs `iznik-server` there if it is missing, and attaches. Every pane is a
real pseudoterminal on the remote host, rendered on the Mac by a libghostty
surface fed the pane's raw bytes. Sessions, tabs and panes live in the
server, so a dropped link, a closed laptop or a restarted application costs a
reconnect and nothing else.

This repository is the server, the client engine the application links, the
wire protocol between them, the C ABI the application calls, and the harness
that proves all of it. The macOS application lives in its own repository and is
built against the C ABI contract that plan 0006 publishes.

## Status

Plan 0001 — the foundations and the regression harness — has landed: the
rule-gated workspace, the golden-pinned wire primitives, the headless VT oracle,
the pseudoterminal harness, and the two-container Podman regression suite whose
scenarios are parallel nextest tests and whose claims registry gates every later
plan. The five plans that build the server, the multiplexer, the daemon, the SSH
bootstrap and the client API are designed and not yet built. The design is
[`ARCHITECTURE.md`](ARCHITECTURE.md); the work is six executable plans under
[`docs/plans/`](docs/plans/STATUS.md), run by
[Makina](https://github.com/koraytaylan/makina).

## Layout

```text
ARCHITECTURE.md      the system design every plan implements
CONTRIBUTING.md      the rules every line is held to, and the gates that hold them
docs/plans/          the plans: scope, architecture, status and one document per task
crates/              iznik-protocol, iznik-link, iznik-server, iznik-client, iznik-ffi,
                     iznik-cli, iznik-harness, iznik-testkit, iznik-regression
xtask/               gates, policy checks, the claims registry, images, staging, distribution
policy/              the dependency allowlist and the project's vocabulary
regression/          claims, scenarios and container images for the regression fixture
```

## Building and testing

```sh
cargo xtask doctor   # what is missing on this machine, and how to install it
cargo xtask check    # format, lint, documentation, tests, container proofs
cargo nextest run --workspace --run-ignored all   # every container proof, in parallel
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the prerequisites, the gates,
and the rules.

## License

MIT.
