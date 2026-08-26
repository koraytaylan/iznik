# Claims

Every task from `claims-registry` onward declares, as data, what it claims
about runtime behaviour and names the test that proves each claim. A claim
without a passing proof, a proof for a claim that does not exist, and a change
to product code that declares no claims each fail `xtask claims verify` — the
fifth gate. This note is the format, the rules, and the reasoning, and it is the
worked example every later task copies.

## The format

One file per task, `regression/claims/<task-id>.toml`. The file's stem is the
task's id, exactly as its `docs/plans/**/tasks/*.md` frontmatter declares it. A
file named for no task is rejected, so a dropped task cannot leave claims behind
that nothing owns.

Each `[[claim]]` has a stable `id`, unique across the whole registry, a
one-sentence present-tense `statement`, and **exactly one** proof:

```toml
[[claim]]
id = "claims-registry-scenario-in-container"
statement = "A scenario proof runs inside the container it names."
scenario = "in-container"
```

- `scenario = "<name>"` names `regression/scenarios/<task-id>/<name>.toml`. The
  scenario must exist, and it must name the claim back in its own `claims`
  array, so the link is checked from both ends.
- `test = "<package>::<binary>::<test>"` names an in-process test, and then a
  one-sentence `because` is required, saying why a container adds nothing to the
  proof. A claim proven by a scenario needs no `because`; a claim proven by a
  test cannot omit it.

A claim names one proof or the other, never both and never neither.

Two optional keys mark a claim that can only be established under a named
condition:

- `platform = "linux"` (or `"macos"`, …) — a claim whose proof needs a
  particular operating system. On a machine that is not that system the claim is
  reported **deferred**, counted on its own, never proven and never failed.
- `profile = "regression"` — a claim whose proof runs under a non-default cargo
  profile. It is run in its own `cargo nextest` invocation with
  `--cargo-profile <profile>`.

## The rules `xtask claims verify` enforces

`registry::load` rejects, naming what and where: an unreadable or unparseable
file; a file named for no task; two claims that share an id; a claim with both
proofs or with neither; a test proof with no `because`; a scenario proof whose
scenario file does not exist; and a scenario of a registered task that names a
claim the registry declares nowhere. A task with no claims file is simply not in
the registry, and its scenarios are not read — which is how every task that
landed before this one stays exempt from a registry it could not depend on.

## Which tasks a run covers

`selection::select` resolves three selections to a list of task ids:

- **`--task <id>` (one or more)** — exactly those tasks.
- **`coverage`** — every task that has a claims file. This is the run for the
  trunk, whose branch diff is empty.
- **no `--task`** — the current branch: the tasks whose claims files differ from
  the merge base with `develop`. If that diff touches product or tooling code —
  a path under `crates/*/src/`, `crates/*/benches/`, or `xtask/src/` — while
  changing no claims file, and a registry exists, the selection fails, naming
  the rule. That is the case the registry exists to catch: code that changes
  without declaring what it now claims.

## What a verdict is

`verify::verify` builds one nextest filterset from the selected proofs — a
scenario proof becomes `test(=scenario::<task>::<name>)`, a test proof becomes
`package(<p>) & binary(<b>) & test(=<t>)` — and runs

```
cargo nextest run --locked --run-ignored all --profile claims -E <filter> \
  --package <p>…
```

with one `--package` per package the proofs name, so nothing else is built or
listed. Proofs under a non-default cargo profile run in a second invocation with
`--cargo-profile`. It then reads `target/nextest/claims/claims.xml`, the JUnit
report the `claims` profile writes, and reports each claim as **proven** (its
test passed), **failed** (its test failed), **missing** (its test did not appear
— an unproven claim, never a proven one), or **deferred**. The gate passes when
every claim is proven or deferred.

## Running one scenario by hand

There is no `xtask regression run`; a scenario is a nextest test, so the command
is a filter. Scenario tests are ignored by default, so `--run-ignored all` is
part of it:

```
cargo nextest run --run-ignored all -E 'test(=scenario::claims-registry::in-container)'
```

`cargo nextest list --run-ignored all -E 'test(/scenario::/)'` lists every
scenario the tree holds.

## Why

Every plan from 0001 onward makes claims about a distributed system under a real
network, a real SSH connection, and a real shell, which no unit test and no
developer's already-configured machine can honestly check. A rule that is not a
gate decays under pressure. This gate turns "every implementation comes with
acceptance criteria" from a habit into a build failure, and it is not exempt
from itself: `claims-registry` declares its own claims and proves them by its
own scenarios, run under the same `coverage` as everything else.
