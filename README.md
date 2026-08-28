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

Four plans have landed. **0001**, the foundations and the regression harness:
the rule-gated workspace, the golden-pinned wire primitives, the headless VT
oracle, the pseudoterminal harness, and the two-container Podman suite whose
scenarios are parallel nextest tests and whose claims registry gates every
later plan. **0002**, the server core: pseudoterminals, the terminal mirror
over `libghostty-vt`, history rings under a shared budget, shell-integration
marks and the screen serializer. **0003**, sessions and multiplexing: the
session registry and its numbered deltas, resume from a held sequence, and the
credit-windowed pump that carries every subscribed pane over one link.
**0004**, the daemon and distribution: a process that outlives every client,
reached through `iznik-server --stdio` over real SSH, with
[committed numbers](docs/notes/baseline.md) and reproducible static artifacts.

The two that remain — the SSH bootstrap and the client API the macOS
application links — are designed and not yet built. The design is
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

## Running a server

The bootstrap runs exactly one of these on a host, and so can a person.

```sh
iznik-server --stdio        # relay standard streams to the daemon, starting it if absent
iznik-server --daemon       # start one in the background and return once its socket answers
iznik-server --foreground   # the same, in this terminal, for watching it
iznik-server --stop         # end the one that holds the lock
iznik-server --version      # the crate version and the protocol version, one line
```

The daemon listens on `$XDG_RUNTIME_DIR/iznik/server.sock`, falls back to
`$TMPDIR/iznik-<user_id>/`, refuses to run twice, survives the SSH session
that started it, and exits on its own once it has no panes and no clients.
`--idle-shutdown-seconds` shortens that interval and `--program` says what a
pane runs; every command answers `--help`.

What it costs — keystroke latency at rest and under a flood, one pane's
throughput and eight panes' together, resident memory idle and with fifty
panes, and startup to a socket that answers — is measured on a described
machine in [`docs/notes/baseline.md`](docs/notes/baseline.md), and asserted
against ceilings by `cargo nextest run --package iznik-server --test
regression_baseline --run-ignored all`.

## Distribution

One file goes to a host whose libc version nothing knows: a stripped,
statically linked musl binary, reproducible from the same commit, beside a
`SHA256SUMS` and a manifest naming the crate version, the protocol version,
the triple and the digest.

```sh
cargo xtask distribution --target x86_64-unknown-linux-musl
cargo xtask distribution --target aarch64-unknown-linux-musl
```

The two Darwin triples build the same way where a Darwin toolchain is present;
on anything else the command says which component is missing.
[`.github/workflows/darwin-artifacts.yml`](.github/workflows/darwin-artifacts.yml)
builds them on a macOS runner, where the SDK is the system's own.

## License

MIT.
