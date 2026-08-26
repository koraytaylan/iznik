# Contributing

> These are the rules every line in this repository is held to, and the gates
> that hold them. They apply to people and to the agents that execute the
> plans under `docs/plans/` alike. There are no exceptions: a rule that is
> wrong for this codebase is changed for everyone, in its own commit, with the
> reason written here — it is never waived at one site.

## 1. Prerequisites

| Tool | Why | Check |
|---|---|---|
| The Rust toolchain pinned in `rust-toolchain.toml` | `rustup` installs it on first use. | `cargo --version` |
| `cargo-nextest` | The test runner. It kills a hung test at its deadline and keeps going; `cargo test` waits forever. | `cargo nextest --version` |
| `podman` with the `netavark` network backend | The two-container regression fixture. Rootless, daemonless, containers resolve each other by name, and it exits non-zero with a legible message when it cannot run. | `podman info` |
| `zig` and `x86_64-linux-musl-gcc` / `aarch64-linux-musl-gcc` | `libghostty-vt` is built by Zig; the regression containers and the SSH bootstrap run statically linked musl binaries. | `zig version` |
| `git` and, once per target and profile, network access | `libghostty-vt-sys` clones ghostty at a pinned commit and fetches its Zig packages on the first build of each target and profile; the shared cache keeps the result. | `git --version` |

`cargo xtask doctor` reports which of these is missing and how to install it.

## 2. The gate

One command runs everything a change must pass before it is committed:

```sh
cargo xtask check
```

It runs these five gates in order, each under its own deadline, and stops at
the first failure naming the gate and the step that was running:

| Gate | Command | Deadline |
|---|---|---|
| `format` | `cargo fmt --all --check` | 5 min |
| `lint` | `cargo clippy --workspace --all-targets --locked -- -D warnings` | 15 min |
| `documentation` | `cargo doc --workspace --no-deps --document-private-items --locked` with `RUSTDOCFLAGS=-D warnings` | 10 min |
| `test` | `cargo nextest run --workspace --locked` — includes every policy test in `xtask/tests/` | 15 min |
| `claims` | `cargo xtask claims verify` — the proofs for the claims this branch declares, run as nextest tests in parallel; nextest prints each test as it finishes, so a long proof is never silent | 15 min |

Warm, the five gates take about ten minutes and the claims gate about two.
The deadlines are what catch a hang, not what a run is allowed to take: a gate
that is slow is a bug in the gate. `.makina/config.toml` runs the same five
commands as Makina's quality gates, so a task that passes locally is a task
that lands. `cargo xtask gate <name>` runs one of them.

**Every command you run has a deadline.** Prefix anything that builds, tests,
starts a container or opens a connection with `timeout <seconds>`; run
anything longer than a minute in the background with its output in a file;
read that file with bounded commands. A command that can hang is a command
that will, and the cost of a hang is measured in hours. Never use `pkill -f`
or `pgrep -f | xargs kill`: they match the shell issuing them. List process ids
first, then kill by number.

### Sharing the build cache

Makina runs each task in its own worktree. A fresh worktree means a cold
`target/`, and a cold build here includes compiling `libghostty-vt` with Zig.
Set one shared cache before running anything:

```sh
export CARGO_TARGET_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/iznik/target"
```

The gates in `.makina/config.toml` set it themselves. Cargo keys artifacts by
package identity, so several checkouts share one cache safely.

## 3. Rules

Each rule names what enforces it. "Review" means a human or the reviewer
agent reads for it and rejects on it; everything else is a failing build.

### 3.1 Names

- **Every identifier is made of whole words.** `buffer`, not `buf`;
  `configuration`, not `config`; `sequence`, not `seq`; `maximum`, not `max`;
  `working_directory`, not `cwd`. Acronyms that are the canonical name of a
  thing — `pty`, `vt`, `ssh`, `ffi`, `utf8`, `osc`, `id` — are allowed by
  being listed. The project's vocabulary is the union of the files under
  `policy/lexicon/`, one per task, one word per line; every word of every
  identifier the workspace declares must appear in it, and so must every word
  of every file and directory name under `crates/`, `xtask/`, `regression/`
  and `policy/`. A word is admitted if it is an English word, a proper noun
  (`ghostty`, `podman`, `zstd`), or an acronym whose expansion nobody says.
  Adding a word is a reviewed edit in the task that needs it.
  *Enforced by `xtask/tests/policy_lexicon.rs`.*
- **No single-character identifiers**, closures included: `left` and
  `right`, not `a` and `b`; `formatter`, not `f`; `index`, not `i`.
  *Enforced by `clippy::min_ident_chars` with an empty allow list.*
- **A name says what the thing is or does.** `read_until_quiet`, not
  `read2`; `PaneHistoryRing`, not `Ring2`. *Review.*

### 3.2 Literals

- **No magic numbers.** The only integer or float literals allowed in an
  expression are `0` and `1`. Every other value is a named `const` whose
  documentation says what the number is and why it has that value; a
  `const`'s initializer is where literals live. Test bodies are fixture data
  and are outside this rule; test *support* code is not.
  *Enforced by `xtask/tests/policy_literals.rs`.*
- Digits in a literal of five or more digits are separated:
  `1_048_576`. *Enforced by `clippy::unreadable_literal`.*

### 3.3 Size

- **A function body is at most 100 lines.**
  *Enforced by `clippy::too_many_lines`, threshold in `clippy.toml`.*
- **A file is at most 1000 lines**, whatever its kind — source, test,
  fixture, document, script. `Cargo.lock` is generated and is the only file
  this does not apply to. *Enforced by `xtask/tests/policy_length.rs`.*
- Cognitive complexity of a function is at most 15.
  *Enforced by `clippy::cognitive_complexity`, threshold in `clippy.toml`.*

### 3.4 Documentation

- **Every item is documented** — public, private, fields, variants,
  modules, crates. A doc comment says what the thing is for and what a caller
  must know; it does not restate the signature.
  *Enforced by `missing_docs` and `clippy::missing_docs_in_private_items`.*
- Every `Result`-returning function documents its errors; every function
  that can panic documents when — and no function in this workspace panics.
  *Enforced by `clippy::missing_errors_doc`, `clippy::missing_panics_doc`.*
- Every crate root is `#![doc = include_str!("../README.md")]`, so the crate
  README and the rendered documentation are one text.
  *Enforced by `xtask/tests/policy_documentation.rs`.*
- Every relative link in every Markdown file resolves.
  *Enforced by `xtask/tests/policy_links.rs`.*
- `cargo doc` is warning-free, private items included. *The `documentation` gate.*

### 3.5 Failure

The server holds people's terminals; the client holds their windows. Neither
may die of a bug that could have been an error value.

- No `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `unreachable!`,
  `std::process::exit`. *Enforced by the clippy restriction lints of the
  same names.*
- No indexing or slicing with `[]`; use `get`, iterators or pattern matching.
  *Enforced by `clippy::indexing_slicing`.*
- No arithmetic that can overflow silently; use `checked_`, `saturating_` or
  `wrapping_` operations and say which you mean.
  *Enforced by `clippy::arithmetic_side_effects`.*
- No `as` casts; use `From`, `TryFrom` and `u16::from(..)`-style widening.
  *Enforced by `clippy::as_conversions`.*
- Tests may `unwrap`, `expect`, index and panic: a panicking test is a failing
  test, which is what it should be. This is scope, set once in `clippy.toml`,
  not an exemption.

### 3.6 Blocking

`iznik-server` and `iznik-client` are asynchronous end to end. In their
sources there is no `std::thread::sleep`, no blocking `std::io::Read` or
`std::io::Write`, and no `std::process` beyond its inert types and values —
`std::process::ExitCode`, `ExitStatus`, `Stdio`, `Output` and `id`, which a
binary's `main` returns and `tokio::process` itself hands out; the async
runtime's equivalents are used instead.
*Enforced by `xtask/tests/policy_blocking.rs`.*

### 3.7 Unsafe

`unsafe` is forbidden in every crate but `iznik-ffi`, whose purpose is the C
boundary. There, every `unsafe` block carries a `// SAFETY:` comment naming
the invariant it relies on and the caller obligation from the header that
guarantees it, and a block contains one unsafe operation. *Enforced by
`#![forbid(unsafe_code)]` in every other crate root (checked by
`xtask/tests/policy_unsafe.rs`), `clippy::undocumented_unsafe_blocks` and
`clippy::multiple_unsafe_ops_per_block`.*

### 3.8 Lints

The workspace denies `clippy::all`, `clippy::pedantic`, the restriction lints
named in this document, `clippy::cognitive_complexity`, `rustdoc::all`, and
the rustc lints `missing_docs`, `unreachable_pub`, `unused_qualifications`,
`missing_debug_implementations` and `rust_2018_idioms`. The complete table is
`[workspace.lints]` in `Cargo.toml`, with thresholds in `clippy.toml`.

One lint clippy offers is deliberately absent, and this is the record of why:
`clippy::renamed_function_params` forbids renaming a trait method's
parameters, while `min_ident_chars` with an empty allow list forbids keeping
`fmt(f)` and `from_str(s)` as the standard library names them. The two cannot
both hold; whole words win, and a trait implementation names its parameters
like everything else. Note that `arithmetic_side_effects` and
`non_ascii_literal` have no test-scope relaxation: test code writes
`count.checked_add(1)` and `"\u{6F22}"` like the rest of the workspace.

A second lint is absent because the toolchain removed it:
`clippy::string_to_string` no longer exists in the pinned clippy, which
reports it as removed and covered by `clippy::implicit_clone` — a pedantic
lint this workspace already denies — on every crate it compiles. Denying a
lint that does not exist is a warning on every build and enforces nothing, so
the table omits it; `pedantic`, which the table denies, already carries the
replacement.

**There is no `#[allow]` and no `#[expect]` anywhere.** A lint that fires
on correct code is either a bug in the code's shape — restructure — or a lint
that is wrong for this codebase — remove it from `Cargo.toml` for everyone,
in its own commit, with the reason added to this section. *Enforced by
`clippy::allow_attributes` and `xtask/tests/policy_attributes.rs`.*

### 3.9 Dependencies

- Every dependency is pinned to an exact version and justified in one
  sentence in `policy/dependencies.md`; the resolved package set must equal
  that list exactly, in both directions. A dependency nobody declared fails
  the build; so does a listed one nothing uses.
  *Enforced by `xtask/tests/policy_dependencies.rs`.*
- `iznik-protocol` has no `[dependencies]`; its development dependencies are
  the golden loader in `iznik-testkit` and `serde_json`. No workspace crate
  has a `build.rs`. `iznik-harness` and `xtask` depend on neither the emulator
  nor any product crate, so the gate runner builds without Zig.
- Manifests are written once, by the scaffold, with every dependency the
  plans need. Adding one is an architecture change: it lands with its
  justification and the allowlist, never alone.

### 3.10 Design

These are not gateable and they are what review is for:

- **DRY** — one implementation per fact. A second copy of a codec, a wait
  loop or a path rule is a bug that has not diverged yet.
- **KISS** — write the obvious version. This is systems plumbing where a
  wrong byte is a corrupted terminal; determinism and reviewability beat
  cleverness, and where a trick is genuinely needed the architecture names
  it.
- **YAGNI** — nothing exists without a consumer. A parameter nothing passes,
  a variant nothing constructs, a configuration nothing reads: delete it.
- **Time is a parameter** — every interval, deadline, cap and backoff is a
  field of an options struct whose default is the named constant; the product
  uses the default and a test shortens it. A test that waits ten seconds for
  a constant is a test written against the wrong thing, and it is rejected.
- **Fast** — an in-process test finishes in under five seconds. nextest
  prints `SLOW` past that and review rejects it; anything that genuinely
  needs longer is a regression test in a `regression_` binary.
- Every assumption a change relies on and every claim it makes about runtime
  behavior is declared in `regression/claims/<task-id>.toml` and proven by a
  scenario in the container fixture. A `test` proof is allowed only where a
  container adds nothing, and it carries a one-sentence `because`.

## 4. Working on a task

1. Read the task file top to bottom, then the workstream's section of the
   plan's `ARCHITECTURE.md`, then the relevant section of the root
   [`ARCHITECTURE.md`](ARCHITECTURE.md). Everything is decided there. If you
   find yourself making a design choice, you have missed a sentence; re-read
   before improvising.
2. Fixtures first: commit the golden, corpus or scenario the task names
   before writing implementation code. When a golden fails, fix the code to
   match the fixture — never the fixture to match the code — unless the
   fixture demonstrably contradicts the architecture, and then say so in the
   commit message.
3. The task's **Tests:** block is the complete acceptance inventory.
   Implement every listed case; add more if you find a gap; never fewer.
4. Stay inside the task's `touches` list. Needing another file is a signal
   you misread the design, not a reason to edit it.
5. Where a test says "a live server", "a real host" or "the real stack", it
   means the fixture's containers and nothing else. No test starts its own
   environment, reads the developer's `~/.ssh`, or touches anything the
   developer is using. A test that passes only because the machine was
   already set up is not evidence.
6. Run `cargo xtask check` before every commit. A task is done when its
   **Done when** command succeeds inside its deadline — not when the code
   works.
7. Commit messages: a subject line under 72 characters in the imperative,
   scoped by crate or area (`protocol: pin the frame codec with goldens`),
   and a body that says why, not what — the diff says what.

## 5. Tests

| Tier | Runs where | Opt-in | Command |
|---|---|---|---|
| Unit and integration | The developer's machine, in-process | none | `cargo nextest run --workspace` |
| Regression | The two-container Podman fixture, four scenarios at a time | `--run-ignored all` | `cargo nextest run --workspace --run-ignored all` |
| Soak | The fixture, for hours | a person | `cargo xtask soak --duration <minutes>` |

There is no environment switch. Every test that starts a container, builds a
release artifact, or measures over a window longer than the default test
deadline lives in a test binary whose name starts with `regression_`, is
`#[ignore]`, and runs only with `--run-ignored all`; nothing else is ignored.
Every scenario under `regression/scenarios/` is one such test, named
`scenario::<task-id>::<name>`, so one filter runs one scenario by hand:

```sh
cargo nextest run --package iznik-regression --test regression_scenarios \
  --run-ignored all -E 'test(=scenario::pty-spawn::login-shell)'
```

`.config/nextest.toml` kills any test that exceeds its deadline and reports
it as a failure naming the test — a hung test cannot hold the suite — and
prints `SLOW` for an in-process test past five seconds, which is the bar.

## 6. Vocabulary

To add a word, add one line to `policy/lexicon/<your-task-id>.txt`. The
reviewer asks two questions of every added word: is it a whole English word,
a proper noun, or an acronym nobody expands in speech; and could the
identifier have used a word already in the lexicon. "It is shorter" is not
an answer to either.
