---
id: model-types
title: "Model Types"
workstream: "0011"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-protocol/src/model.rs"
  - "crates/iznik-protocol/tests/model_invariants.rs"
  - "crates/iznik-protocol/tests/fixtures/model.jsonl"
  - "regression/claims/model-types.toml"
  - "policy/lexicon/model-types.txt"
status: done
merged_as: ""
---
# Model Types

The host model: sessions, tabs, panes and layout trees, keyed on identity that is minted once and never reused, with position a derived field. The layout normalization rule is decided here so that two clients computing the same arrangement produce the same tree.

**Steps:**

1. Author `crates/iznik-protocol/tests/fixtures/model.jsonl` first: an empty host, one session with one tab and one pane, nested splits in both directions with unequal weights, and a host with several sessions — each with its exact encoding.
2. Implement `crates/iznik-protocol/src/model.rs` — the types, `LayoutNode::{normalize, leaves, replace_leaf, remove_leaf}`, `HostModel::validate`, `ModelError`, `encode_host_model`, `decode_host_model` — exactly as the architecture's `model-types` section specifies.
3. Write `crates/iznik-protocol/tests/model_invariants.rs`.
4. Declare this task's claims in `regression/claims/model-types.toml` as `test` proofs with their `because`.

**Tests:**

- Every fixture line round-trips exactly in both directions.
- Each invariant has a violating model that `validate` rejects with the variant naming the identity: a duplicate id, a session without tabs, a tab without panes, a leaf naming a pane not in the tab, a pane missing from the layout, a zero weight, an unnormalized tree, an empty name.
- Normalization: a split nested in a same-direction split is flattened with weights scaled, a single-child split collapses, an already-normalized tree is unchanged, and normalization is idempotent over a thousand generated trees.
- `replace_leaf` and `remove_leaf` preserve every other leaf and leave the tree normalized.
- Identity is not positional: removing the first tab of three leaves the other two tabs' ids unchanged.

- **Done when:** `timeout 600 cargo nextest run --package iznik-protocol --test model_invariants` passes every case above, `timeout 900 cargo xtask claims verify --task model-types` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
