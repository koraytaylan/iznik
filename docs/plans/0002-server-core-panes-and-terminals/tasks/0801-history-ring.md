---
id: history-ring
title: "History Ring"
workstream: "0008"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-server/src/history/mod.rs"
  - "crates/iznik-server/src/history/ring.rs"
  - "crates/iznik-server/tests/history_ring.rs"
  - "regression/claims/history-ring.toml"
  - "policy/lexicon/history-ring.txt"
status: done
merged_as: ""
---
# History Ring

The last bytes of every pane, indexed by absolute sequence, so that a reconnecting client names the byte it holds and receives exactly the bytes it missed — and bounded per pane and in total, so a hundred idle panes cannot exhaust a small host.

**Steps:**

1. Implement `crates/iznik-server/src/history/mod.rs` and `ring.rs` — `PaneHistory` with `range` and `copy_range`, `HistorySlices`, `HistoryError`, `HistoryBudget`, and the two default constants — exactly as the architecture's `history-ring` section specifies.
2. Write `crates/iznik-server/tests/history_ring.rs`, with a counting global allocator for the allocation assertion, running the ring on one thread with no runtime.
3. Declare this task's claims in `regression/claims/history-ring.toml` as `test` proofs with their `because`.

**Tests:**

- The property: for ten thousand random appends of random lengths, `range(from)` for every held `from` returns exactly the bytes appended since `from`, `copy_range` with every `maximum` returns exactly the prefix of that and the sequence after it, and `oldest` and `newest` are correct throughout.
- Aged out: a `from` older than `oldest` returns `AgedOut` naming `oldest`.
- Capacity: after appending more than `capacity` bytes, the ring holds exactly the newest `capacity` bytes.
- At most two slices: `range` over a wrapped ring yields two contiguous slices whose concatenation is the expected bytes.
- The budget: with a total smaller than the sum of the panes' capacities, the least recently focused pane's history shrinks first, and a `touch` changes which pane that is.
- No per-byte allocation: appending ten thousand chunks performs a number of allocations independent of the number of bytes.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test history_ring` passes every case above, `timeout 900 cargo xtask claims verify --task history-ring` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
