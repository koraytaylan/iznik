# Architecture — Plan 0008

> The concrete deltas, by symbol. The system is described in the root
> [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) §5.5 and §6.2; the rules and the
> way of working are in [`AGENTS.md`](../../../AGENTS.md). Read both before your
> first task. Every task here declares its claims in
> `regression/claims/<task-id>.toml`; a `test` proof carries its `because`.

## The order of this plan

The boundary lands first, because nothing else can hold a descriptor it did not
open until there is a safe constructor for one. The state that crosses lands
next, because it is pure over bytes and provable without a daemon. The in-place
replacement lands third, on both. The mirror rebuild lands fourth, on a daemon
that has adopted panes. The application's choice lands last, on a capability
the server advertises once adoption works.

## 0101 — The adoption boundary

### What a session actually is, on the host

One pane is exactly: a pseudoterminal master descriptor, opened by the daemon
and never on the wire; a child process group, whose leader is the shell; a
history ring holding the pane's bytes since creation, at absolute sequence
numbers; and a `libghostty-vt` mirror reproducing the current screen from those
bytes. A tab is ordered panes; a session is ordered tabs; the registry holds
them all. Everything a person sees is derived from the ring and the mirror, so
adoption is: keep the descriptor and the child, carry the ring and the
sequence, rebuild the mirror.

### `iznik-ffi` as the one unsafe boundary, or a named change to the rule

`crates/iznik-ffi/src/lib.rs` already holds every `unsafe` block in the
workspace, each with a `// SAFETY:` comment. A constructor that takes an
inherited raw descriptor and makes an owning `portable-pty` master is a genuine
`unsafe` operation — the descriptor's validity and ownership are a contract the
caller must keep — and it belongs there or nowhere. This task decides between:

- `pub fn pty_adopt(master_fd: RawFd, ...) -> Result<...>` in `iznik-ffi`, used
  by the daemon through a thin safe wrapper, keeping `forbid(unsafe_code)` in
  every other crate; or
- an amendment to `AGENTS.md` §3.7 naming a second boundary — a small
  `iznik-server` module — with the reason written down, changing the rule for
  everyone.

The first is preferred and is the plan's default: one boundary, already proven
by `policy_unsafe_boundary`.

## 0102 — Carrying the state across

### `crates/iznik-server/src/adopt.rs`

`pub struct AdoptedState { version: u16, model: HostModel, panes: Vec<AdoptedPane> }`
where `AdoptedPane { pane: PaneId, master_fd: RawFd, process_id: u32, sequence: Sequence, ring: Vec<u8>, termios: TermiosState }`. Encoded with the
protocol's own [`iznik_protocol::wire`] primitives — the same length-prefixed
bytes, optional fields and counts — so it is one codec implementation, not a
second. `encode_adopted(&AdoptedState) -> Result<Vec<u8>, AdoptError>`,
`decode_adopted(bytes) -> Result<AdoptedState, AdoptError>`, and
`ADOPTED_STATE_VERSION`, refused rather than guessed across versions exactly as
`PROTOCOL_VERSION` is. Pure over bytes; a test over generated states is the
whole proof.

### The handover, not the wire

The state file lives at `RuntimePaths.state` — beside the socket and the lock,
under the `0700` runtime directory — never on the client protocol. The old
daemon writes it before it `exec`s, and the new incarnation reads it from the
descriptor it inherited. A client never sees it.

## 0103 — The in-place replacement

### `--adopt` on the daemon

`iznik-server --adopt <state-path> <listener-fd> <lock-fd>` runs the accept
loop over an inherited listener, takes the lock it inherited rather than
acquiring it, and rebuilds the registry from the state file and the inherited
masters. It refuses, with the words and an exit code, when the state does not
decode, is another version, or names a master that is not open.

### The replacement itself

`crates/iznik-server/src/daemon/adopt.rs`: `pub fn replace(paths, state, options, deadline) -> Result<(), AdoptError>` — the old daemon stops accepting,
drains its connections, writes `AdoptedState`, clears `CLOEXEC` on every master
and the listener and the lock, serializes them into the environment or an
inherited map, and `execv`s the staged binary. `execv` is the point: there is no
window in which two daemons hold the same masters, and a failed `execv` leaves
the calling process alive to report it.

### Rollback

An `execv` that fails is the old daemon still running, and it says so and
carries on. A new daemon that fails to adopt writes the reason to the log, and
the *staged* old binary — kept as `<prefix>/bin/iznik-server.previous` until the
new one answers `--version` correctly — is put back and re-executed by the same
path. The task proves the successful replacement, the `execv` failure leaving
the daemon alive, and the adoption refusal rolling back.

## 0104 — Rebuilding the mirrors

The registry's `adopt` builds each pane's `Pane` from the ring and the reported
screen, so a `ScreenRequest` after the upgrade is exact and `Resume` from a held
byte is contiguous. The one subtlety is that the mirror is rebuilt from bytes
that were already sent to some client: the pane's `newest` sequence is adopted
verbatim, and the mirror is fed its ring through the same path a live pane is,
so nothing can disagree about where the stream is. Clients reconnect and resume,
which is what plan 0005 already proves for a link drop.

## 0105 — The application offers it

### The `ADOPT` capability

`Capabilities::ADOPT` (bit 3) is advertised by a server that can adopt. The
application's upgrade prompt gains a second choice when the host's server
advertises it: *upgrade and keep the sessions* beside *upgrade and end them*.
`HostOperation::Upgrade` becomes `Upgrade { keep_sessions: bool }`, and the
manager's `upgrade` takes the same flag; keeping is refused for a server without
the bit, with the warning it always carried.
