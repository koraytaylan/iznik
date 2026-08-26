---
id: deltas-and-reconciler
title: "Deltas and Reconciler"
workstream: "0011"
kind: task
depends_on:
  - model-types
gated: false
touches:
  - "crates/iznik-protocol/src/delta.rs"
  - "crates/iznik-protocol/src/reconcile.rs"
  - "crates/iznik-protocol/tests/delta_golden.rs"
  - "crates/iznik-protocol/tests/reconcile_property.rs"
  - "crates/iznik-protocol/tests/fixtures/delta.jsonl"
  - "crates/iznik-testkit/src/generate.rs"
  - "crates/iznik-testkit/tests/generate.rs"
  - "regression/claims/deltas-and-reconciler.toml"
  - "policy/lexicon/deltas-and-reconciler.txt"
status: planned
merged_as: ""
---
# Deltas and Reconciler

Every change to the model is a numbered delta, and a client that misses a number asks for a snapshot rather than guessing. The reconciler is shared by the server's own model and every client, so the property both rely on — apply the deltas and you hold the snapshot — is proven once, here, by generation rather than by example, with a generator the registry and the client reducer will reuse rather than copy.

**Steps:**

1. Author `crates/iznik-protocol/tests/fixtures/delta.jsonl` first: at least one line per `Delta` variant with its exact encoding, including every `RemovalReason` and `ExitStatus` form.
2. Implement `crates/iznik-protocol/src/delta.rs` and `reconcile.rs` — `Delta`, `RemovalReason`, `ExitStatus`, the codec, `apply` with every check before the first mutation, `ReconcileError` — exactly as the architecture's `deltas-and-reconciler` section specifies.
3. Implement `crates/iznik-testkit/src/generate.rs` — `ModelGenerator` over a seeded xorshift, producing valid models, valid delta sequences with the model they lead to, and valid registry operation sequences — and write `crates/iznik-testkit/tests/generate.rs`.
4. Write `crates/iznik-protocol/tests/delta_golden.rs` and `reconcile_property.rs`.
5. Declare this task's claims in `regression/claims/deltas-and-reconciler.toml` as `test` proofs with their `because`.

**Tests:**

- Every fixture line round-trips exactly in both directions.
- Generation discipline: a delta numbered other than the model's generation plus one is refused with `GenerationGap` and the model is unchanged; a delta with the right number advances the generation by one.
- The property: for ten thousand generated sequences of valid deltas from a valid model, every intermediate model validates, and the final model equals the model the generator built directly, in under five seconds.
- Refusal leaves no trace: a delta that would break an invariant is refused with a variant naming the identity, and the model is byte-identical to before.
- Layout deltas are normalized on application.
- `TabsReordered` with a non-permutation is refused.
- The generator: the same seed yields the same sequence; every model it produces validates; every delta sequence it produces applies cleanly to its starting model and arrives at the model it names.

- **Done when:** `timeout 600 cargo nextest run --package iznik-protocol -E 'test(delta_golden) | test(reconcile_property)'` and `timeout 600 cargo nextest run --package iznik-testkit --test generate` pass every case above, `timeout 900 cargo xtask claims verify --task deltas-and-reconciler` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
