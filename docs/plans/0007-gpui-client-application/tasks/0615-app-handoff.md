---
id: app-handoff
title: "Handoff: run the whole registry, record the deferred proofs"
workstream: "0006"
kind: task
depends_on:
  - app-packaging
  - architecture-amendment
gated: false
touches:
  - docs/notes/release-checklist.md
  - regression/claims/claims-registry.toml
status: done
merged_as: "1f91de5"
---
# Handoff: run the whole registry, record the deferred proofs

Close the plan the way the house closes plans: every claim over every task verified at once, the deferred display-bound proofs named rather than forgotten, and the release checklist carrying the application's items.

**Steps:**

1. Register every task's claims file in `regression/claims/claims-registry.toml` and run full coverage: every claim proven, every proof passing, every task that declares none named as declaring none.
2. Verify the deferral posture: every claim whose proof needs a display or a Mac (`terminal-grid` frame timings, `app-packaging` codesign and notarize) is recorded with its platform and its `because`, and none is silently absent.
3. Finish `docs/notes/release-checklist.md`'s application section: bundle, sign, notarize by hand on the Mac, run the headless end-to-end on the development machine, and re-measure the render budget on the release machine, recorded beside the machine's name. The plan's `STATUS.md` outcome and the root roll-up row are left to the coordinator from what landed.

**Tests:**

- Full claims coverage passes over the whole registry, application claims included.
- The checklist's application section names only commands that exist and pass.

- **Done when:** `timeout 1200 cargo xtask claims coverage` reports every claim proven with none outstanding.
