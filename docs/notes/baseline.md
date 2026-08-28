# The performance baseline

Every performance claim in this repository is a measured, committed number and
never an adjective. These were taken through the real `iznik-server` binary
over a real unix socket by `crates/iznik-server/benches/baseline.rs`, under the
`regression` profile — the profile the containers run — so the table describes
what a person on this machine would get.

Reproduce it with:

```sh
cargo bench --profile regression --package iznik-server --bench baseline
```

The ceilings are asserted, deliberately and separately, by
`crates/iznik-server/tests/regression_baseline.rs`. They are generous: a
regression test that fails on a noisy machine gets deleted, and then there is
none. What they catch is a change of *kind* — a keystroke that waits behind a
flood, a pane that buffers what it should stream — not a few per cent either
way.

## The machine

| | |
|---|---|
| Processor | AMD Ryzen 7 PRO 8700GE (16 threads) |
| Memory | 62 GiB |
| Kernel | Linux 7.0.0-29-generic |
| Toolchain | rustc 1.97.1 (8bab26f4f 2026-07-14) |
| Profile | `regression` (release, thin codegen units, debug info) |
| Commit | `ee5893f` |

## The figures

| Figure | Measured | Ceiling |
|---|---|---|
| Keystroke to echo, at rest, median | 0.056 ms | none |
| Keystroke to echo, at rest, 99th percentile | 0.095 ms | 5.000 ms |
| Keystroke to echo, under a flood, median | 0.080 ms | none |
| Keystroke to echo, under a flood, 99th percentile | 0.158 ms | 30.000 ms |
| One pane's throughput | 58 MiB/s | at least 50 MiB/s |
| Eight panes' throughput together | 157 MiB/s | more than one pane's |
| Resident memory at rest | 6 MiB | 32 MiB |
| Resident memory with fifty idle panes | 18 MiB | 256 MiB |
| Startup to a socket that answers | 18.811 ms | 500.000 ms |

## What each one is

**Keystroke to echo** is a line written to a pane running `cat` on a
pseudoterminal, timed until its echo arrives on that pane's channel through the
socket — a thousand round trips, sorted. The flooded figure runs the same
thousand while another subscribed pane produces at line rate, which is the
case the whole scheduler exists for: the tail rises from 0.095 ms to 0.158 ms,
a sixty per cent increase on a number two hundred times under the budget a
person can feel.

**Throughput** is sixteen mebibytes from a pane's shell, timed from the request
to the last byte at the client, with credit returned as it arrives. Eight panes
together sustain 157 MiB/s — more than one pane's 58, which is what says the
scheduler shares rather than serializes.

**Resident memory** is the daemon process's own, read from `/proc`, at rest and
then holding fifty idle panes: 12 MiB for fifty panes, about 240 KiB each, which
is the history ring's floor and not its capacity — a ring is allocated as it
fills.

**Startup** is `--foreground` to a socket that answers, which is what
`--daemon` and the relay wait for on a host's first use.
