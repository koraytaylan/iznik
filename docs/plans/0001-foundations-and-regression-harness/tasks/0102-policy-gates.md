---
id: policy-gates
title: "Policy Gates"
workstream: "0001"
kind: task
depends_on:
  - workspace-scaffold
gated: false
touches:
  - "xtask/src/policy/**"
  - "xtask/tests/policy_*.rs"
  - "xtask/tests/fixtures/policy/**"
  - "policy/dependencies.md"
  - "policy/lexicon/policy-gates.txt"
status: done
merged_as: ""
---
# Policy Gates

Clippy enforces most of `CONTRIBUTING.md` §3. This task enforces the rest — whole-word names from a committed vocabulary, no magic numbers, file length, documentation placement, link integrity, the dependency allowlist, the blocking-call boundary, the unsafe boundary, and the absence of `allow`, `expect` and `cfg(test)` — as ordinary tests that run in the `test` gate, each proven against synthetic trees that contain the violations it must catch.

**Steps:**

1. Write the synthetic trees first: under `xtask/tests/fixtures/policy/<check>/`, one minimal tree per check containing at least one violation of each rule the check enforces and one clean sibling, so every rule has a positive and a negative case.
2. Implement `xtask::policy::Violation` and the nine checks — `lexicon`, `literals`, `length`, `documentation`, `links`, `dependencies`, `blocking`, `unsafe_boundary`, `attributes` — each a pure function from a repository root to a `Vec<Violation>`, exactly as the architecture's `policy-gates` section specifies them, with `syn`'s visitor for the source-level checks.
3. Write `policy/dependencies.md` listing every package `cargo metadata --format-version 1 --locked` resolves for the scaffold, one justified line each, transitive entries naming the direct dependency they were reached from.
4. Fill the `xtask policy` subcommand to run every check and print violations as `path:line: rule: detail`, and write `xtask/tests/policy_<check>.rs` for each check running it against its fixtures and against the real tree.

**Tests:**

- Lexicon: a declared identifier with a word outside the lexicon is reported with its file and line; a used-but-undeclared identifier such as a method from `std` is not; splitting handles `snake_case`, `CamelCase`, `SCREAMING_CASE`, trailing digits, digit-only tokens, raw identifiers and leading underscores; a file name with an unlisted word is reported; a lexicon file that is unsorted, has a duplicate, has an uppercase or non-alphanumeric entry, or is named after no task is reported.
- Literals: `0` and `1` pass; `2` in an expression fails with its line; any value in a `const` or `static` initializer or an enum discriminant passes; an array repeat count and an array-type length are checked; a literal in a `tests/` file, a `benches/` file or under `xtask/tests/fixtures/` is not reported.
- Length: a 1001-line file is reported and a 1000-line file is not; `Cargo.lock` is never reported; untracked-but-not-ignored files are counted.
- Documentation: a crate root without the README include, a README without the crate heading, a source file without a leading `//!`, a module absent from its crate's README, and a fence in a README or a doc comment that is untagged or tagged `rust` are each reported.
- Links: a relative link to a missing file is reported with its file and line; links with a scheme, a bare fragment, and a fragment on an existing file are not.
- Dependencies: a resolved package absent from the allowlist and a listed package nothing resolves are both reported; a version range, in a development dependency too, is reported; a `build.rs` in a workspace crate is reported; a `[dependencies]` entry on `iznik-protocol` is reported and a `[dev-dependencies]` entry is not.
- Blocking: `std::thread::sleep`, `std::io::Read`, `std::io::stdout` and `std::process` under `iznik-server` or `iznik-client` are reported; the same in `crates/iznik-server/src/pty/streams.rs` or in any other crate are not.
- Unsafe boundary: a crate root without `forbid(unsafe_code)` is reported unless it is `iznik-ffi`.
- Attributes: inner and outer `allow`, `expect` and `cfg(test)` attributes are reported anywhere under `crates/` and `xtask/`, including in tests, and not under `xtask/tests/fixtures/`.
- The real tree passes every check, which is what makes the gate meaningful from this commit on.

- **Done when:** `timeout 600 cargo nextest run --package xtask -E 'test(policy_)'` passes every case above against both the fixtures and the real tree, `timeout 300 cargo xtask policy` exits 0 with no output, and `timeout 300 cargo fmt --all --check`, `timeout 900 cargo clippy --workspace --all-targets --locked -- -D warnings` and `timeout 900 cargo nextest run --workspace --locked` succeed.
