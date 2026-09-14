---
id: app-packaging
title: "Package the app: .app bundle layout, icon, version stamping"
workstream: "0005"
kind: task
depends_on:
  - settings-and-theme
gated: true
touches:
  - crates/iznik-app/assets/icon.svg
  - crates/iznik-app/src/bundle.rs
  - crates/iznik-app/tests/bundle_contents.rs
  - docs/notes/release-checklist.md
  - policy/lexicon/app-packaging.txt
  - regression/claims/app-packaging.toml
  - xtask/src/distribution/app.rs
status: planned
merged_as: ""
---
# Package the app: .app bundle layout, icon, version stamping

Produce the shippable shape: a bundler that lays out the macOS `.app` (executable, `Info.plist`, icon set) and a Linux binary layout, version-stamped from the workspace — its logic tested anywhere, its execution on a Mac recorded as deferred like the Darwin artifacts.

**Steps:**

1. Write `crates/iznik-app/src/bundle.rs`: the bundler — given a built binary and the assets, write `Contents/MacOS/iznik-app`, `Contents/Info.plist` (identifier, version from the workspace version, minimum macOS per GPUI's requirement), and the icon set; and the Linux layout (binary, `.desktop` file, icons).
2. Wire `cargo xtask app-bundle --target <triple>` in `xtask/src/distribution/app.rs` to stage and bundle, reusing the distribution task's staging; report darwin-specific signing as an explicit deferred step in its output.
3. Write `crates/iznik-app/tests/bundle_layout.rs`: run the bundler on staged fixtures and assert the layout, plist fields, and version stamping byte-for-byte — no Mac required.
4. Extend `docs/notes/release-checklist.md` with the application items: bundle both platforms, codesign and notarize by hand on a Mac, and record which machine did it.
5. Declare this task's claims in `regression/claims/app-packaging.toml`.

**Tests:**

- The bundle layout test passes on the Linux development machine: the bundler's output is fully determined by its inputs.
- `cargo xtask app-bundle` refuses a missing binary or a version mismatch and names both.
- The plist's version equals the workspace version at build time.

- **Done when:** `timeout 600 cargo nextest run --package iznik-app --test bundle_layout` passes every case above, `timeout 600 cargo nextest run --package xtask --test app_bundle` passes, `timeout 900 cargo xtask claims verify --task app-packaging` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
