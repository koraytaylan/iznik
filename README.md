# iznik

A remote terminal system with a cross-platform GPUI front end.

The GPUI application connects to a host over your own SSH configuration,
installs `iznik-server` there if it is missing, and attaches. Every pane is a
real pseudoterminal on the remote host, rendered by the application from a
`libghostty-vt` terminal fed the pane's raw bytes. Sessions, tabs and panes live in the
server, so a dropped link, a closed laptop or a restarted application costs a
reconnect and nothing else.

This repository is the server, the client engine, the wire protocol, the GPUI
application, the C ABI for other front ends, and the harness that proves all of
it. The GPUI application links `iznik-client` directly;
[`docs/CLIENT.md`](docs/CLIENT.md) is the contract of the C ABI, for a front
end written in another language.

## Status

Eight plans are written. Six have landed; the seventh has done every task
and is still being integrated; the eighth is proposed. **0001**, the
foundations and the regression harness:
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

**0005**, the SSH bootstrap and the multi-host client: hosts reached over the
user's own `ssh`, bootstrapped from a machine that had nothing, held several at
once behind a client-side model with optimistic commands, and carried through
link drops without losing a pane's identity or its bytes.

**0006**, the client API and the handoff: the C ABI the application links,
with a generated header pinned against the crate and a C program that drives a
real daemon through it; the plumbing commands and the diagnostics bundle that
say which of five layers is broken; [the contract](docs/CLIENT.md) an
application is written against; and a soak that runs the whole stack for hours
and refuses a run that measured nothing.

**0007**, the client application, in progress: the GPUI application in
`crates/iznik-app`, linking the client engine directly — the VT thread, the
terminal grid element, input and IME, the window with its tab and session
bars, splits, the command palette, settings, packaging and a headless end to
end proof. All twenty of its tasks are done; the plan is not yet closed, and
its two native display measurements are still to be taken.
**0008**, session-preserving upgrade, is proposed and not started: replacing
a host's server without ending the shells it holds.

The design is [`ARCHITECTURE.md`](ARCHITECTURE.md); the work is eight
executable plans under [`docs/plans/`](docs/plans/STATUS.md), run by
[Makina](https://github.com/koraytaylan/makina).

## Layout

```text
ARCHITECTURE.md      the system design every plan implements
CONTRIBUTING.md      the rules every line is held to, and the gates that hold them
docs/plans/          the plans: scope, architecture, status and one document per task
crates/              iznik-protocol, iznik-link, iznik-server, iznik-client, iznik-app, iznik-ffi,
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
iznik-server --adopt        # serve inherited sessions after an in-place replacement
iznik-server --adopt-request # ask the running daemon to replace itself and keep its sessions
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

## What iznik puts on a host

A tool that installs binaries on other people's machines says so up front.
Connecting to a host that has never seen iznik puts exactly this on it, and
nothing else:

| Where | What |
|---|---|
| `<prefix>/bin/iznik-server` | The server, one static binary, verified by its `SHA-256` before it is renamed into place. |
| `<prefix>/terminfo` | The `xterm-ghostty` entry, compiled there by the host's own `tic`. Skipped where the host has no `tic`; its panes are then told `xterm-256color`. |
| `<runtime>/server.sock` | The daemon's socket. |
| `<runtime>/server.lock` | The lock that keeps one daemon per user, holding its process id. |
| `<runtime>/server.log` | What the daemon has to say. |
| `<runtime>/agent.sock` | A link to the SSH agent of the newest connection, re-pointed by each one as it arrives, and what every pane started after it was made has as its `SSH_AUTH_SOCK`. Made only by a connection that forwards an agent; a pane started while there is none has no `SSH_AUTH_SOCK` at all. |

The prefix is the first of `$XDG_DATA_HOME/iznik`, `$HOME/.local/share/iznik`
and the runtime directory that the host says this user both owns and may
write; nothing is created to find out. The runtime directory is
`$XDG_RUNTIME_DIR/iznik`, or `$TMPDIR/iznik-<user_id>` where there is no
`XDG_RUNTIME_DIR`.

The daemon outlives the SSH session that started it — that is the point of it
— so a pane is not handed that session's `SSH_CONNECTION`, `SSH_CLIENT`,
`SSH_TTY` or `XDG_SESSION_ID`, and its `SSH_AUTH_SOCK` is the agent link above
rather than the first session's agent, which would be dead after a reconnect.
The daemon exits on its own once it has held no panes and no clients for ten
minutes. Connecting again to a host whose installed server has the same
SHA-256 digest as the one this build carries uploads nothing at all; a server of
the same version but other bytes is uploaded again, and a daemon already
running is left alone until it is upgraded.

Taking it off removes the binary, the terminfo, the runtime directory and the
prefix itself where iznik made it. A prefix iznik was lent rather than made —
`XDG_RUNTIME_DIR` is one of the candidates — keeps everything that was not
iznik's. The client engine does it through `HostManager::uninstall`, which
also lets the host go and tells the application it has; `iznik uninstall
<host>` reaches the host and removes what is on it, having no model to keep.

## Building the application

The application is the `iznik-app` GPUI binary in this workspace. Other front
ends may use the C ABI and the same [`docs/CLIENT.md`](docs/CLIENT.md) contract.

Every path below is under `target/`, which is where cargo writes unless
`CARGO_TARGET_DIR` says otherwise — and [`CONTRIBUTING.md`](CONTRIBUTING.md)
asks you to set it to a shared cache. Whatever it is set to, the layout under
it is the same.

**The contract is [`docs/CLIENT.md`](docs/CLIENT.md).** Read that, not the
Rust: it is the threading rules, the ownership rules, the credit protocol,
what a reconnection obliges, how query responses are answered, what the marks
carry, and a worked example. Where the contract and the implementation
disagree, the contract wins and the implementation is the bug.

**The header and the library.**

```sh
cargo xtask header                        # regenerate include/iznik.h
cargo build --release --package iznik-ffi # libiznik.a, and libiznik.dylib on a Mac
```

`include/iznik.h` is committed and pinned: a signature that changes changes
the header in the same commit, and a test compares them byte for byte. Build
against that header and link `target/release/libiznik.a` — the archive is
named for the library and not for the package — or the dynamic library beside
it, which is `libiznik.dylib` on macOS and `libiznik.so` on Linux. A static
archive needs the system libraries it was built against, and they differ by
platform, so ask the toolchain rather than guessing:

```sh
cargo rustc --release --package iznik-ffi -- --print native-static-libs
```

**The servers the client installs.** A client bootstraps a host by uploading
one, so it needs a directory of them — one per triple it means to reach. The
triples are the *hosts'*, not the Mac's: reaching Linux hosts means the two
musl targets.

```sh
cargo xtask distribution --target x86_64-unknown-linux-musl
cargo xtask distribution --target aarch64-unknown-linux-musl
```

That writes `target/distribution/<triple>/iznik-server`, which is the layout
the client reads. Name the directory in the `artifacts_directory` field of the
`iznik_configuration` you make the client with; the
`IZNIK_ARTIFACTS_DIRECTORY` variable is the command-line tool's way of saying
the same thing and the library does not read it. Left null, the library looks under its own runtime directory, and a host
whose triple it cannot find there is reported as unsupported rather than
bootstrapped.

Building a Darwin server — for reaching a Mac — needs the SDK path exported,
which macOS does not export itself:

```sh
export SDKROOT=$(xcrun --show-sdk-path)
cargo xtask distribution --target aarch64-apple-darwin
```

**Developing without SSH.** A host alias of the form `unix:<path>` names a
daemon socket on this machine and is reached with no SSH at all. Start one
with `iznik-server --daemon` and use the socket under its runtime directory —
`$XDG_RUNTIME_DIR/iznik/` where that is set, and `$TMPDIR/iznik-<user_id>/`
where it is not, which is the case on macOS:

```sh
iznik-server --daemon
runtime="${XDG_RUNTIME_DIR:+$XDG_RUNTIME_DIR/iznik}"
iznik state "unix:${runtime:-${TMPDIR:-/tmp}/iznik-$(id -u)}/server.sock"
```

It is the alias the C smoke program and every in-process test use, and it is
the fastest way to have a real pane in front of an application under
development.

**When something is wrong**, one command collects what each layer says — what
`ssh` would do for this alias, what the probe found, what the server answers,
what state the host reached, and what a keystroke costs — and nothing in it
that could carry a secret: most sections never collect one (no identity file,
agent socket, proxy command or environment, and a transport's refusal by its
kind rather than in its own words), and the daemon's log tail keeps each line's
shape but replaces every field's value with `<redacted>`:

```sh
iznik doctor <host>
```

## Distribution

One file goes to a host whose libc version nothing knows: a stripped,
statically linked musl binary, reproducible from the same commit, beside a
`SHA256SUMS`, a manifest naming the crate version, the protocol version,
the triple and the digest, and a `THIRD-PARTY-NOTICES` listing every package
the binary is built from with its licence, and each licence text once. An
application bundle carries the same file for the application and its servers.

```sh
cargo xtask distribution --target x86_64-unknown-linux-musl
cargo xtask distribution --target aarch64-unknown-linux-musl
```

The two Darwin triples build the same way where a Darwin toolchain is present;
on anything else the command says which component is missing.
[`.github/workflows/darwin-artifacts.yml`](.github/workflows/darwin-artifacts.yml)
builds them on a macOS runner, where the SDK is the system's own.

Every push to `develop` that passes the gates in
[`.github/workflows/check.yml`](.github/workflows/check.yml) also updates the
rolling GitHub prerelease named `develop-snapshot`. The Linux archive, the two macOS archives and the
Windows client zip are replaced in place, so the release URL stays stable
while always pointing at the latest verified development build, and a
`SHA256SUMS` beside them lists each archive's digest. The Windows
zip is the application and the Linux and macOS servers it can install on a
host; a Mac host is reached through Remote Login. The zip is unsigned. No
Windows server is published, and `cargo xtask distribution` refuses the
Windows triples: the daemon there listens on loopback TCP without
authentication, so any local user could reach every shell, and the client
cannot start it, because the command it sends is quoted for a POSIX shell.
The workflow is [`.github/workflows/snapshot.yml`](.github/workflows/snapshot.yml).

`cargo xtask app-bundle` signs a macOS bundle as its last step — the macOS
servers it carries first, then the bundle — and verifies it with
`codesign --verify --deep --strict`. With `IZNIK_CODESIGN_IDENTITY` naming a
signing identity in a keychain `codesign` can reach, it signs with that
identity under the hardened runtime with a secure timestamp; without it, ad
hoc. The snapshot workflow signs with a Developer ID and notarizes when these
repository secrets are set, and says in the release notes which it did:

| Secret | What it holds |
|---|---|
| `MACOS_CERTIFICATE_P12_BASE64` | The Developer ID Application certificate and its private key, exported as `.p12` and base64-encoded. |
| `MACOS_CERTIFICATE_PASSWORD` | The password of that `.p12`. |
| `MACOS_CODESIGN_IDENTITY` | The identity as `codesign` names it, `Developer ID Application: <name> (<team>)`. |
| `APPLE_ID` | The Apple ID notarization submits as. |
| `APPLE_TEAM_ID` | The team the certificate belongs to. |
| `APPLE_APP_SPECIFIC_PASSWORD` | An app-specific password of that Apple ID. |

Without the first three the bundles are signed ad hoc and not notarized;
with them but without the last three, signed and not notarized.

## License

MIT.
