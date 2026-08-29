# xtask

The gates, the policy checks clippy cannot express, the claims registry, the container images, staging, distribution, the C header and the soak, behind `cargo xtask`. Depends on neither the emulator nor any product crate, so it builds on a machine with nothing but a Rust toolchain.

`cargo xtask <subcommand>` routes `check`, `gate`, `doctor`, `policy`, `claims`, `regression`, `distribution`, `header` and `soak` to the modules below; `--help` lists them, and each of them answers `--help` with what it takes — including the two whose work has not landed, which say so rather than staying silent. `tests/readme_commands.rs` asks every one of them, so a name here that no binary answers to is a failing test.

## Modules

| Module | Holds | Landed by |
|---|---|---|
| `claims` | The claims registry: `xtask claims verify [--task <id>]…` runs the proofs of the tasks a branch changes, `xtask claims coverage` runs every one there is. | `claims-registry` (plan 0001) |
| `claims::registry` | Loading and validating every claims file under `regression/claims/`. | `claims-registry` (plan 0001) |
| `claims::selection` | Which tasks a run verifies: explicit ids, everything, or the current branch's diff under the product-code rule. | `claims-registry` (plan 0001) |
| `claims::verify` | Building the nextest filterset, running the proofs under the `claims` profile, and reading the `JUnit` report. | `claims-registry` (plan 0001) |
| `distribution` | `xtask distribution --target <triple>`: reproducible release artifacts with checksums and a manifest naming the crate version, the protocol version, the triple and the digest. | `linux-artifacts` (plan 0004) |
| `distribution::darwin` | The two Darwin targets, the toolchain they need written out, and the refusal that names the missing SDK rather than reporting a link error from inside cargo. | `darwin-artifacts` (plan 0004) |
| `distribution::shape` | What an artifact's own headers say: an ELF file's machine, whether it names a loader and whether it still carries a symbol table; a Mach-O file's CPU type and every library it names. | `linux-artifacts` (plan 0004) |
| `distribution::linux` | The two musl targets built under the release profile, stripped, byte-identical across builds, with the workspace's own configured flags carried through rather than replaced. | `linux-artifacts` (plan 0004) |
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
| `soak::commands` | What a soak asks the containers to do: the census of processes it weighs with, the held client started and stopped, and the link cut and made good again. Written for the `dash` and `mawk` these images carry. | `soak-and-release` (plan 0006) |
| `soak::report` | What a soak measured and what reading it says: the series, what the held client heard, the growth read from a series, and the report a note is given. No containers and no clock. | `soak-and-release` (plan 0006) |
| `soak::steps` | The driver steps a soak writes: the pane it opens, the wait for that flood to be over, and the flood and the recovery of every round. | `soak-and-release` (plan 0006) |

## Tests

`tests/skeleton.rs` is the scaffold's own acceptance; the policy, gate, doctor and claims tests arrive with the tasks that fill the modules, and every one of them runs in the `test` gate. `readme_commands.rs` runs every command the READMEs name with `--help`, on the one rule `iznik_harness::documents` holds; `artifact_shape.rs` holds the ELF and Mach-O readers to files made to have each shape, and builds nothing. The two `regression_distribution_*` binaries are ignored by default, because each builds release artifacts; they share a nextest group of one thread, since two release builds at once wait on each other for cargo's package-cache lock.
