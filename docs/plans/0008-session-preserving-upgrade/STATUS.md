# Plan 0008 — Session-Preserving Upgrade — 📝 Proposed

- **Status:** 📝 Proposed. No task has been executed; the plan is authored and
  awaits registration.
- **Goal:** Replace the server on a host without ending the sessions it holds,
  by exec'ing the new binary in place and handing it the pseudoterminal
  masters, the history rings and the sequence numbers it already owns.
- **Root cause:** The daemon is the sessions — a pane is a master fd, a child
  group, a history ring and a mirror — so replacing its binary ends them. The
  root architecture defers hot upgrade by descriptor passing; this plan is that
  work, and it is its own plan because it is mostly server-side and needs its
  own container proof.
- **Approach:** Land the unsafe adoption boundary first (in `iznik-ffi`, or by
  a named amendment to the boundary rule); then the versioned `AdoptedState`
  codec, pure over bytes; then the in-place `execv` replacement with rollback;
  then rebuilding each mirror from the carried ring; then the application's
  choice, gated on an `ADOPT` capability.
- **Progress:** 0/5 tasks done; 0 blocked; 0 dropped.
- **Integration:** not registered.
- **Exceptions:** —.
- **Outcome:** A host whose server is behind can be upgraded with the shells
  running on it still alive, still in their sessions and tabs under the same
  pane ids, still resuming byte-exact across the reconnect — and an upgrade
  that cannot adopt leaves the host exactly as it was.

_Last updated: 2026-09-21._
