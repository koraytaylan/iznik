---
id: workspace-scaffold
title: "Workspace Scaffold"
workstream: "0001"
kind: chore
depends_on: []
gated: false
touches:
  - Cargo.toml
  - Cargo.lock
  - rust-toolchain.toml
  - rustfmt.toml
  - clippy.toml
  - .cargo/config.toml
  - .config/nextest.toml
  - "crates/iznik-protocol/Cargo.toml"
  - "crates/iznik-protocol/README.md"
  - "crates/iznik-protocol/src/**"
  - "crates/iznik-link/Cargo.toml"
  - "crates/iznik-link/README.md"
  - "crates/iznik-link/src/**"
  - "crates/iznik-server/Cargo.toml"
  - "crates/iznik-server/README.md"
  - "crates/iznik-server/src/**"
  - "crates/iznik-server/benches/baseline.rs"
  - "crates/iznik-client/Cargo.toml"
  - "crates/iznik-client/README.md"
  - "crates/iznik-client/src/**"
  - "crates/iznik-ffi/Cargo.toml"
  - "crates/iznik-ffi/README.md"
  - "crates/iznik-ffi/src/**"
  - "crates/iznik-cli/Cargo.toml"
  - "crates/iznik-cli/README.md"
  - "crates/iznik-cli/src/**"
  - "crates/iznik-harness/Cargo.toml"
  - "crates/iznik-harness/README.md"
  - "crates/iznik-harness/src/**"
  - "crates/iznik-testkit/Cargo.toml"
  - "crates/iznik-testkit/README.md"
  - "crates/iznik-testkit/src/**"
  - "crates/iznik-regression/Cargo.toml"
  - "crates/iznik-regression/README.md"
  - "crates/iznik-regression/src/**"
  - "xtask/Cargo.toml"
  - "xtask/README.md"
  - "xtask/src/**"
  - "xtask/tests/skeleton.rs"
  - "policy/lexicon/workspace-scaffold.txt"
status: planned
merged_as: ""
---
# Workspace Scaffold

This task establishes the invariant the whole bundle depends on: it writes every manifest, every module declaration and every dependency the project will ever have, so that no task in any later plan adds a `mod`, a member or a crate. It also lands the complete rule set as configuration — the lint table, the profiles, the formatter, the test runner's deadlines — so the very first line of product code is written under the same rules as the last, and it settles that the emulator is built optimized for every profile so tests never run a slower emulator than the product ships.

**Steps:**

1. Write the root `Cargo.toml` — `[workspace]` with the ten members and `resolver = "3"`, `[workspace.package]` with `version`, `edition = "2024"`, `rust-version = "1.97"` and `license = "MIT"`, `[profile.regression]` and `[profile.release]` as the architecture specifies, and the complete `[workspace.lints]` table transcribed verbatim from the architecture's `workspace-scaffold` section — then `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `.cargo/config.toml` (the alias, the musl linkers with `-C link-self-contained=no`, and `[env] LIBGHOSTTY_VT_SYS_OPTIMIZE = "ReleaseFast"`) and `.config/nextest.toml` (the default profile's deadlines, the `regression_` and timing overrides, the `scenarios` test group of four, and the `claims` profile with its JUnit path) exactly as the architecture specifies them.
2. Write all ten crate manifests with `[lints] workspace = true`, the binary names `iznik-server`, `iznik`, `iznik-regression` and `xtask`, `[[bench]] name = "baseline", harness = false` on `iznik-server`, `[[test]] name = "regression_scenarios", harness = false` on `iznik-regression`, library-plus-binary layout for `iznik-server`, `iznik-regression` and `xtask`, `crate-type = ["cdylib", "staticlib"]` for `iznik-ffi`, and every dependency and development dependency from the architecture's dependency list pinned at an exact version in the crate that will use it. Generate and commit `Cargo.lock`.
3. Create the complete module skeleton exactly as tabulated in the architecture: every file a stub whose only content is module documentation stating what the module will hold and which task fills it, every `mod` declaration wired, every crate root carrying `#![doc = include_str!("../README.md")]` and — except `iznik-ffi` — `#![forbid(unsafe_code)]`. Write each crate's `README.md` with its purpose and a table of its modules, every fenced block tagged.
4. Write each binary's `main.rs` as the dispatcher the architecture describes: it inspects the first argument only and calls `<module>::run(&arguments)`; each subcommand's `run` is written in its module as a stub that writes `<subcommand>: not implemented until task <task-id>` to stderr through `std::io::Write` and returns `ExitCode::from(2)` — except `xtask claims verify`, whose stub is the fifth gate's bootstrap form: it exits 0 while `regression/claims/` does not exist and fails naming task `claims-registry` once it does. Later tasks replace a stub's body and never edit a `main.rs`.
5. Seed `policy/lexicon/workspace-scaffold.txt` with every word the skeleton's identifiers and file names use, sorted and unique, and write `xtask/tests/skeleton.rs` with the acceptance inventory below.

**Tests:**

- Module completeness in both directions: for every crate, the set of `.rs` files under `src/` equals the set reachable from the crate root through `mod` declarations — an orphaned file is simply not compiled and no lint would catch it.
- Every crate root includes its README and, except `iznik-ffi`, forbids unsafe code; every README begins with `# <crate name>` and every fenced block in it carries a language tag other than `rust`.
- Every version requirement in every workspace manifest, development dependencies included, is an exact `=` pin; `iznik-protocol` declares no `[dependencies]`; no crate has a `build.rs`; `iznik-harness` and `xtask` depend on neither `libghostty-vt` nor any product crate but `iznik-protocol`, asserted from `cargo metadata`.
- The root manifest declares `[profile.regression]` and `[profile.release]` with exactly the fields the architecture lists, and `.cargo/config.toml` sets `LIBGHOSTTY_VT_SYS_OPTIMIZE`.
- Every dispatcher subcommand exits 2 with its stub line on stderr and nothing on stdout, asserted per subcommand with the two streams captured separately; a first argument the dispatcher does not know exits 2 naming the known ones; `xtask claims verify` exits 0 against this tree and exits non-zero naming `claims-registry` against a temporary root that contains `regression/claims/`.
- `cargo build --profile regression --target x86_64-unknown-linux-musl --package iznik-server --package iznik-regression --package iznik-cli` succeeds, since every regression container runs those artifacts; the `cdylib` is deliberately not built for musl.
- `cargo build --package xtask` completes without invoking Zig, asserted with a `PATH` that hides it.
- The five gates run clean on the empty scaffold — `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo doc` with `-D warnings`, `cargo nextest run --workspace --locked`, and `cargo xtask claims verify` in its stub form — proving the lint table, the configuration and the stubs are well-formed before any real code exists.
- `.makina/config.toml` names exactly the five gates in the order the architecture tabulates.

- **Done when:** `timeout 600 cargo nextest run --package xtask --test skeleton` passes every case above, `timeout 1200 cargo build --profile regression --target x86_64-unknown-linux-musl --package iznik-server --package iznik-regression --package iznik-cli` succeeds, and `timeout 300 cargo fmt --all --check`, `timeout 900 cargo clippy --workspace --all-targets --locked -- -D warnings`, `RUSTDOCFLAGS='-D warnings' timeout 600 cargo doc --workspace --no-deps --document-private-items --locked` and `timeout 900 cargo nextest run --workspace --locked` all succeed.
