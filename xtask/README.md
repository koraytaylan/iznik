# xtask

The gates, the policy checks clippy cannot express, the claims registry, the container images, staging, distribution, the C header and the soak, behind `cargo xtask`. Depends on neither the emulator nor any product crate, so it builds on a machine with nothing but a Rust toolchain.

`cargo xtask <subcommand>` routes `check`, `gate`, `doctor`, `policy`, `claims`, `regression`, `distribution`, `header` and `soak` to the modules below; `--help` lists them.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `claims` | The claims registry: what a task claims about runtime behavior, the proof that establishes each claim, and the gate that runs the proofs. | `claims-registry` (plan 0001) |
| `claims::registry` | Loading and validating every claims file under `regression/claims/`. | `claims-registry` (plan 0001) |
| `claims::selection` | Which tasks a run verifies: explicit ids, everything, or the current branch's diff under the product-code rule. | `claims-registry` (plan 0001) |
| `claims::verify` | Building the nextest filterset, running the proofs under the `claims` profile, and reading the `JUnit` report. | `claims-registry` (plan 0001) |
| `distribution` | `xtask distribution --target <triple>`: reproducible release artifacts with checksums and a manifest. | `linux-artifacts` (plan 0004) |
| `distribution::darwin` | The Darwin targets, failing with the missing SDK component named when the toolchain is absent. | `darwin-artifacts` (plan 0004) |
| `distribution::linux` | The two musl targets built under the release profile, stripped, byte-identical across builds. | `linux-artifacts` (plan 0004) |
| `doctor` | `xtask doctor`: every prerequisite with a probe and an install hint, reported by name when missing. | `gate-runner` (plan 0001) |
| `gate` | `xtask check` and `xtask gate <name>`: the five gates in order, each under its deadline, stopping at the first failure with its name. | `gate-runner` (plan 0001) |
| `header` | `xtask header`: generating `include/iznik.h` with cbindgen, for the golden test that pins the ABI. | `static-library-and-header` (plan 0006) |
| `policy` | `xtask policy`: the policy checks, each a pure function from a repository root to a list of violations. | `policy-gates` (plan 0001) |
| `policy::attributes` | No `allow`, `expect` or `cfg(test)` attribute anywhere under `crates/` and `xtask/`. | `policy-gates` (plan 0001) |
| `policy::blocking` | No blocking standard-library call under the two asynchronous crates, with `pty::streams` as the one named exception. | `policy-gates` (plan 0001) |
| `policy::dependencies` | The resolved package set equals the allowlist in both directions, every requirement is an exact pin, `iznik-protocol` has no dependencies, and no crate has a build script. | `policy-gates` (plan 0001) |
| `policy::documentation` | Every crate root includes its README, every file begins with documentation, every module appears in its README, and every fence is tagged. | `policy-gates` (plan 0001) |
| `policy::length` | No file over a thousand lines, `Cargo.lock` excepted. | `policy-gates` (plan 0001) |
| `policy::lexicon` | Every word of every declared identifier and of every file name under the checked directories appears in the committed vocabulary. | `policy-gates` (plan 0001) |
| `policy::links` | Every relative Markdown link resolves. | `policy-gates` (plan 0001) |
| `policy::literals` | No integer or float literal but `0` and `1` in an expression outside a constant's initializer. | `policy-gates` (plan 0001) |
| `policy::unsafe_boundary` | Every crate root but `iznik-ffi`'s forbids unsafe code. | `policy-gates` (plan 0001) |
| `regression` | `xtask regression images`, `stage` and `reap`: the container images, the staging directory, and the removal of every labelled container. | `regression-images` (plan 0001) |
| `soak` | `xtask soak`: hours of the end-to-end stack against two fixture hosts with faults, sampling memory and losing no byte. | `soak-and-release` (plan 0006) |

## Tests

`tests/skeleton.rs` is the scaffold's own acceptance; the policy, gate, doctor and claims tests arrive with the tasks that fill the modules, and every one of them runs in the `test` gate.
