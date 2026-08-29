# iznik-client

The client engine the macOS application links: the SSH transport over the system `ssh`, the bootstrap, the client-side model and reducer, optimistic commands, and multi-host management. Asynchronous end to end.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `bootstrap` | The bootstrap: probe, decide, upload, launch, handshake, snapshot, and the upgrade and uninstall paths. | `remote-launch` (plan 0005) |
| `bootstrap::launch` | Launching or adopting the daemon on a probed host, and the upgrade decision. | `remote-launch` (plan 0005) |
| `bootstrap::probe` | One script asked of a host and a pure reading of its answer: the machine, an installed server, the terminal's terminfo and `tic`, and the first prefix the host will let iznik write. | `host-probe` (plan 0005) |
| `bootstrap::terminfo` | The `xterm-ghostty` terminfo source as a constant, with the ncurses it came from and the one field of it that is not `infocmp`'s. | `terminfo-asset` (plan 0005) |
| `bootstrap::upload` | The artifact and the terminfo down the standard input of one remote shell, verified by a digest this client computed, and renamed into place only once the host agrees. | `payload-upload` (plan 0005) |
| `commands` | Optimistic commands: the unambiguous ones applied locally at once and confirmed or rolled back by the authoritative delta, the rest waiting one round trip. | `optimistic-commands` (plan 0005) |
| `host` | Multi-host: host identity, the per-host connection state machine, and the manager that runs one task per host. | `host-identity-and-state` (plan 0005) |
| `host::identity` | `HostId`, the user's alias, and the global pane address `iznik://<host>/<pane>`. | `host-identity-and-state` (plan 0005) |
| `host::manager` | The host manager: one task per host, isolation between hosts, and the resume that keeps a pane's bytes across a drop. | `connection-manager` (plan 0005) |
| `host::manager::task` | What one host's own task does, from its first bootstrap to its last: connect, serve, lose the link, wait, connect again. | `connection-manager` (plan 0005) |
| `host::state` | The per-host connection state machine with exponential backoff and jitter, as a pure table of transitions. | `host-identity-and-state` (plan 0005) |
| `model` | The client's model: one host view per host with its subscriptions, focus and pending commands, holding everything a resume needs. | `client-model` (plan 0005) |
| `reduce` | The reducer that applies every server message to the client model, routed by host first, yielding the effects the engine acts on. | `client-reducer` (plan 0005) |
| `transport` | The transport under a host: the client's own runtime paths, the `unix:` alias it interprets itself, and the choice between a local socket and the system `ssh`. | `ssh-control-master` (plan 0005) |
| `transport::channel` | One channel per host carrying every pane: the `Hello` exchange, a protocol version refused rather than worked around, compression through the shared link layer, and a ping on its own task so a dead link is a message in seconds. | `remote-channel` (plan 0005) |
| `transport::ssh` | Spawning the system `ssh` with the four options iznik owns and none a person could have configured, and classifying its failures into messages they can act on. | `ssh-control-master` (plan 0005) |

## Tests

Integration tests live under `tests/`; there is no test module inside `src/`, here or anywhere in the workspace. `connection_manager.rs` stands whole servers up on this machine and reaches them through the `unix:` alias, with a relay in front of one of them so a link can be cut and let back: nothing else in reach can drop a connection without also taking away the server a resume needs. The same operations over real SSH, against two containers, are `end-to-end-ssh`'s scenarios. `ssh_control_master.rs` starts no process: what it holds are the argument vector, the alias forms, the control paths and the classification of `ssh`'s own words, captured under `tests/fixtures/ssh/`. Everything that touches a network is a scenario, run from the engine container. `remote_channel.rs` is the one exception and only in appearance: it opens channels over the `unix:` alias, against a real daemon and against a server written to say the wrong version or nothing at all — a socket on this machine being the same link the SSH path builds.
