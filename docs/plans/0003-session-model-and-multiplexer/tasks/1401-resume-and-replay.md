---
id: resume-and-replay
title: "Resume and Replay"
workstream: "0014"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-server/src/resume.rs"
  - "crates/iznik-server/tests/resume.rs"
  - "regression/claims/resume-and-replay.toml"
  - "policy/lexicon/resume-and-replay.txt"
status: done
merged_as: ""
---
# Resume and Replay

A reconnecting client names the byte it holds; the server continues from it exactly or sends the truth at a named sequence. One mechanism, decided in one place, as a pure function over what the ring holds — so the decision is tested exhaustively in milliseconds here and executed by the multiplexer, which proves the bytes through the oracle.

**Steps:**

1. Implement `crates/iznik-server/src/resume.rs` — `StartRequest`, `StartPlan`, `plan_start` — exactly as the architecture's `resume-and-replay` section specifies.
2. Write `crates/iznik-server/tests/resume.rs`.
3. Declare this task's claims in `regression/claims/resume-and-replay.toml` as `test` proofs with their `because`.

**Tests:**

- `Subscribe` plans a `Screen` at `newest` whatever the ring holds.
- `Resume { from }` plans `Continue { from }` for every `from` between `oldest` and `newest` inclusive, and a `Screen` at `newest` for `from` below `oldest` or above `newest` — every boundary asserted, and a thousand generated `(oldest, newest, from)` triples agreeing with the inequality.
- `ScreenRequest` plans a `Screen` at `newest`.
- A plan never names a sequence the ring does not hold: for every generated input, `Continue { from }` satisfies `oldest <= from <= newest` and `Screen { at }` has `at == newest`.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test resume` passes every case above, `timeout 900 cargo xtask claims verify --task resume-and-replay` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
