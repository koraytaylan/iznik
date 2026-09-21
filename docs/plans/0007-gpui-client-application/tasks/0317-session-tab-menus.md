---
id: session-tab-menus
title: "Give the session bar the tab bar's right-click menu, moves included"
workstream: "0003"
kind: task
depends_on:
  - tab-and-session-bars
  - ssh-config-hosts
gated: false
touches:
  - ARCHITECTURE.md
  - crates/iznik-app/README.md
  - crates/iznik-ffi/src/lib.rs
  - crates/iznik-cli/src/doctor.rs
  - crates/iznik-client/src/host/manager/mod.rs
  - crates/iznik-client/src/host/manager/task.rs
  - crates/iznik-client/src/host/state.rs
  - crates/iznik-client/src/model.rs
  - crates/iznik-client/tests/host_state.rs
  - crates/iznik-client/tests/manager_traffic.rs
  - crates/iznik-protocol/src/capabilities.rs
  - crates/iznik-protocol/tests/message_golden.rs
  - crates/iznik-server/tests/client_connections.rs
  - regression/claims/client-connections.toml
  - regression/claims/connection-manager.toml
  - crates/iznik-app/assets/default-keybindings.json
  - crates/iznik-app/src/actions.rs
  - crates/iznik-app/src/bars.rs
  - crates/iznik-app/src/prompt.rs
  - crates/iznik-app/src/tab_actions.rs
  - crates/iznik-app/src/window.rs
  - crates/iznik-app/tests/end_to_end.rs
  - crates/iznik-app/tests/inventory.rs
  - crates/iznik-app/tests/prompt.rs
  - crates/iznik-app/tests/session_menu.rs
  - crates/iznik-client/src/commands.rs
  - crates/iznik-client/tests/optimistic_commands.rs
  - crates/iznik-protocol/src/command.rs
  - crates/iznik-protocol/src/delta.rs
  - crates/iznik-protocol/src/reconcile.rs
  - crates/iznik-protocol/tests/command_golden.rs
  - crates/iznik-protocol/tests/delta_golden.rs
  - crates/iznik-protocol/tests/fixtures/command.jsonl
  - crates/iznik-protocol/tests/fixtures/delta.jsonl
  - crates/iznik-protocol/tests/reconcile_property.rs
  - crates/iznik-server/src/session/commands.rs
  - crates/iznik-server/src/session/mod.rs
  - crates/iznik-server/src/session/registry.rs
  - crates/iznik-server/tests/session_commands.rs
  - crates/iznik-server/tests/session_registry.rs
  - crates/iznik-testkit/src/generate.rs
  - crates/iznik-testkit/tests/generate.rs
  - docs/notes/protocol.md
  - policy/lexicon/session-tab-menus.txt
  - regression/claims/action-inventory.toml
  - regression/claims/command-palette.toml
  - regression/claims/deltas-and-reconciler.toml
  - regression/claims/optimistic-commands.toml
  - regression/claims/session-command-codec.toml
  - regression/claims/session-registry.toml
  - regression/claims/session-tab-menus.toml
status: done
merged_as: ""
---
# Give the session bar the tab bar's right-click menu, moves included

The bottom session chips had a right-click menu with only Rename and Close,
while the top tab chips offered New, Rename, Move Left/Right and the three
close entries. The session menu is brought to the same shape — and Move Left
and Move Right need a way to put the host's sessions in an order, which the
wire did not carry: sessions are an ordered `Vec` on the model and only tabs
had a reorder command. This task adds `ReorderSessions`, its
`SessionsReordered` delta and the reconciler that applies it, then generalises
the open menu to a subject so both bars share one shape.

A command a server does not know is not a refusal: the server's decoder has no
tag for it, so it reads the frame as garbage and ends the whole connection.
Because a remote is upgraded on its own and only on purpose — replacing its
server ends the sessions it holds — the new command is advertised as the
capability `Capabilities::REORDER_SESSIONS`, and a client sends it only to a
server that set the bit. An older server is then connected to as it is, its
window simply does not offer the moves, and its own sessions are never lost to
a command it cannot read. The protocol version is not bumped: nothing is
released yet, and a capability is what distinguishes a server that can decode
the command from one that cannot without ending every older host's connection.

**Steps:**

1. `iznik-protocol`: `SessionCommand::ReorderSessions { order }` (tag 11) and
   `Delta::SessionsReordered { order }` (tag 14), their codecs, and
   `reconcile::reorder_sessions` refusing an order that is not a permutation
   with `NotASessionPermutation`. `Capabilities::REORDER_SESSIONS`,
   `SessionCommand::needs`/`is_supported_by` and `SessionCommand::name`.
   Goldens for both, the reference table updated so `protocol_reference` still
   holds.
2. `iznik-server`: `Registry::reorder_sessions` emitting the delta only when
   the registry's own reconciler accepts it; the registry error and the
   command routing; the rejection code; and `REORDER_SESSIONS` in the
   capabilities it advertises.
3. `iznik-client`: the optimistic effect for `ReorderSessions`; the server's
   advertised capabilities carried into `HostState::Connected` and the host
   view; and `HostManager::command` refusing an unsupported command as
   `ManagerError::Unsupported` before it is shown or sent.
4. `iznik-testkit`: a `ReorderSessions` registry operation and a
   `reorder_sessions` change kind, so the generated convergence properties
   exercise the new delta.
5. `iznik-app`: `ActionId::ReorderSessions` in the inventory with a prompt of
   whole orders, offered only where the connected server can decode it; the
   order helpers generalised over identity; the session menu gains New
   Session, Move Left/Right, Close Session, Close Other Sessions and Close
   Sessions to the Right, its moves disabled for a server that cannot answer
   them.
6. Write the tests, extend the claims, add the vocabulary and amend the
   architecture's command list.

**Tests:**

- A `ReorderSessions` command and a `SessionsReordered` delta each encode to
  their golden bytes and decode back; a bad order is refused and changes
  nothing.
- Every command that has always been in the protocol needs no capability, and
  `ReorderSessions` needs the one that says so.
- The server advertises it can reorder sessions and answers the command; a
  manager whose connected server did not advertise it refuses the command
  without sending.
- `reorder_sessions` emits one delta carrying the whole order, and the
  registry rebuilds from it.
- A reorder of the host's sessions shows at once and is what the host would
  send.
- Reordering sessions offers every other position of the selected session as
  the host's whole order, and a one-session host offers none; a host whose
  server cannot decode the command is offered none.
- A session's right click opens its menu, and the menu is still rendered after
  the window's next repaints.
- Choosing Close Session and choosing Move Left from a session's own menu act
  on the live in-process host.

- **Done when:** `timeout 900 cargo nextest run --workspace --locked` passes, `timeout 900 cargo xtask claims verify --task session-tab-menus` reports every claim proven.
