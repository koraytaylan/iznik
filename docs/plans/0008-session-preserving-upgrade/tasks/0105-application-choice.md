---
id: application-choice
title: "Offer the upgrade that keeps the sessions, and say what each path costs"
workstream: "0001"
kind: task
depends_on:
  - in-place-replacement
gated: false
touches:
  - crates/iznik-app/README.md
  - crates/iznik-app/src/host_ui.rs
  - crates/iznik-app/src/palette.rs
  - crates/iznik-app/src/prompt.rs
  - crates/iznik-app/tests/prompt.rs
  - crates/iznik-client/src/bootstrap/mod.rs
  - crates/iznik-client/src/host/manager/mod.rs
  - crates/iznik-client/src/host/state.rs
  - crates/iznik-protocol/src/capabilities.rs
  - crates/iznik-server/src/connection.rs
  - policy/lexicon/application-choice.txt
  - regression/claims/application-choice.toml
status: planned
merged_as: ""
---
# Offer the upgrade that keeps the sessions, and say what each path costs

An upgrade already warns that every session on the host ends. Where the host's
server can adopt, it does not have to: the prompt gains a second choice, and
the ending path stays exactly as it is for a host that cannot.

**Steps:**

1. `crates/iznik-protocol`: `Capabilities::ADOPT` (bit 3), included in
   `Capabilities::known()` but not in `FEATURES` — a client uses nothing extra
   when it is present, so its absence is not a missing feature.
2. `crates/iznik-server/src/connection.rs`: advertise `ADOPT`.
3. `crates/iznik-client`: `HostOperation::Upgrade { keep_sessions: bool }` and
   `HostManager::upgrade(alias, force, keep_sessions)`, keeping the sessions
   only for a host whose server advertised `ADOPT` and refusing otherwise with
   the warning it already gives.
4. `crates/iznik-app`: the upgrade prompt offers *keep the sessions* beside
   *end them* when the host's server advertises `ADOPT`, and only the warning
   path when it does not; `host: upgrade` does the same from the palette.
5. Write the tests in `crates/iznik-app/tests/prompt.rs`: both choices for an
   adopting server, one for one that does not, and each naming its cost.
6. Declare the claims in `regression/claims/application-choice.toml`.

**Tests:**

- A host whose server advertises `ADOPT` offers both paths, each saying what it
  costs; one that does not offers only the ending path.
- Choosing the keeping path carries `keep_sessions` through the manager.

- **Done when:** `timeout 600 cargo nextest run --package iznik-app --test prompt` passes every case above, `timeout 900 cargo xtask claims verify --task application-choice` reports every claim proven.
