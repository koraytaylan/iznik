# Scope — Plan 0001

> Lay the workspace and the rules it is held to, the wire primitives, the headless test instruments, and the two-container regression harness that every later claim is proven against — with a deadline on everything that can wait, and a ceiling on how long the harness itself may take.

## Why this plan

Every plan after this one builds a piece of a distributed system and has to prove it: a server that survives a dropped link, a bootstrap onto a host that has never seen iznik, a multiplexer that keeps the focused pane responsive while a neighbor floods. None of that is provable in a unit test, and none of it is provable on a developer's own machine either — a laptop with an `ssh-agent` and a populated `~/.ssh` passes tests that fail for everyone else. So the proof surface — two containers on a private network, real SSH, real shells — exists from the first plan rather than the last, together with the registry that makes "every claim has a proof" a build failure instead of a habit.

The second thing this plan settles is discipline. The rules in [`CONTRIBUTING.md`](../../../CONTRIBUTING.md) — whole-word names from a committed vocabulary, no magic numbers, bounded function and file sizes, documentation on every item, no `unwrap` and no `allow` — are only rules if a build fails when they are broken. This plan lands the gates that fail it: the lint table, and the policy checks clippy cannot express.

The third is time, in both directions. The previous incarnation of this project lost a working day to shells that were waiting on something that would never finish, and its container suite was slow enough that nobody wanted to run it. Here, nothing waits without a bound: every spawned process has a deadline that kills its process group, every test has a deadline the runner enforces, every scenario step declares one, every fixture wait has a cap, and a hang is a failure that names what was running. And nothing is allowed to be slow: an in-process test over five seconds is flagged, the fixture's start time is a measured number with a ceiling, scenarios are ordinary tests that run in parallel, every interval in the product is a parameter a test can shorten, and the gate that runs a task's container proofs is expected to finish in two minutes.

## In scope

- **0001 — Scaffold and Gates.** The cargo workspace with every manifest final and every module declared as a documented stub; the lint table, profiles, formatter and test-runner configuration; the policy checks for vocabulary, literals, size, documentation, links, blocking calls, unsafe boundaries and attributes; the dependency allowlist; and the bounded process runner and deadline helpers behind `cargo xtask check`, `cargo xtask gate` and `cargo xtask doctor`.
- **0002 — Wire Primitives.** The length-prefixed frame codec and the `iznik/1` control messages, each pinned by a golden fixture asserted in both directions, with the session-model payloads carried as opaque blobs until plan 0003 defines them; and the framed link over any duplex stream that both ends and the test client speak through.
- **0003 — Test Instruments.** The headless VT oracle over `libghostty-vt` with deterministic snapshots, the pseudoterminal harness whose reads end on quiet rather than on a clock, and the fidelity corpus of constructs that break naive terminal plumbing, committed as a golden because the compression dictionary is trained on it.
- **0004 — Regression Harness.** The engine and host container images pinned by digest and keyed by content hash; the fixture that starts them with per-run credentials, measures its own start time, and makes orphaned containers impossible to keep; the scenario format and runner, with every scenario an ordinary nextest test that runs in parallel and every step deadlined; and the claims registry with its coverage gate.
- **0005 — Documentation.** The root and crate documentation brought into line with what landed.

## Out of scope

No pseudoterminal is spawned by product code, no terminal is mirrored, and no session exists: that is plan 0002. The protocol's session model, deltas and commands are plan 0003; this plan carries them as opaque payloads so that the fixtures pinned here never change. No SSH is spoken by product code — the fixture speaks it, and plan 0005 puts a transport under the client. Nothing here is distributed to a remote host.
