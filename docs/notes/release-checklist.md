# The release checklist

What a release runs through, in order. Everything here is a command or a file
in this repository; nothing is a judgement call, and nothing is skipped because
it passed last time.

The order is not arbitrary. The soak comes first because it is the only item
that takes a working day and the only one that fails for reasons the others
cannot see; running it last would mean discovering on Friday afternoon that
Monday's release is not one. Everything after it is minutes.

## 1. A six-hour soak, run by a person

```sh
cargo xtask soak
```

Six hours against two hosts. Round after round, back to back: a flood poured
through a pane a client holds open for the whole run, the daemon stopped
underneath it for longer than any deadline a client keeps, started again, and a
host that has to answer afterwards — with sessions made and unmade on the
second host beside all of it. The sampling has a schedule; the work does not.

It refuses a run for any of these, and each of them is a real outcome to read
rather than a formality:

- either the held client or either daemon growing by more than four mebibytes
  an hour after the warmup;
- a side never weighed at all, which is a census that found nothing and would
  otherwise report a flat series and no leak;
- the held client gone before the end, or its pane detached — a stream that
  stopped early has no gap in it either;
- the held client hearing fewer bytes than its pane was made to say;
- a second screen, meaning a reconnection the host could not carry the client
  on from, so the bytes between were lost;
- fewer than half the rounds finishing.

The report is printed whether it passed or not. A run that failed the ceiling
by a hair is exactly the run whose series somebody has to read.

This is the item that cannot be automated away. A leak of a few kilobytes per
reconnection is invisible in every other item on this list and fatal by
Thursday, and the only thing that finds it is time.

Replace `docs/notes/soak.md` with the report the run prints, keeping its
preamble: the date, the machine and the tree it was taken from. A release is
made against a report from that release's tree and not from the last one, and
those three lines are what let a reviewer tell the two apart.

## 2. The performance baseline, re-measured

```sh
cargo bench --profile regression --package iznik-server --bench baseline
```

Compare against `docs/notes/baseline.md` and replace it. The ceilings in
`crates/iznik-server/tests/regression_baseline.rs` are generous on purpose, so
a number inside them that has moved by half is still a finding — the table is
what a person on this machine gets, and a release that quietly doubled the cost
of a keystroke should say so out loud rather than pass.

## 3. The gate

```sh
cargo xtask check
```

Format, lint, documentation, tests, claims. Every gate, on a clean tree, from
this commit. Not the gates that ran on the branch: the merge is a different
tree from either side of it.

## 4. Every claim, over every task

```sh
cargo xtask claims coverage
```

The gate verifies the claims of what a run touched. This verifies all of them:
every task that has declared claims, every proof run, and every task that has
declared none named as having declared none. A release is where the whole
registry is asked at once, because a proof that rotted six weeks ago rotted
against a task nobody has touched since.

## 5. The distribution artifacts

```sh
cargo xtask distribution --target x86_64-unknown-linux-musl
cargo xtask distribution --target aarch64-unknown-linux-musl
```

Then, for the Darwin triples, the `darwin-artifacts` workflow on a macOS
runner. Each build writes `SHA256SUMS` and a `manifest.toml` naming the crate
version, the protocol version, the triple and the digest. Check that the
protocol version in the manifest is the one this release means to speak: a
client bootstraps a host by uploading one of these, and a mismatch here is a
mismatch on every host somebody upgrades.

Build each triple twice and compare the digests. They are built with
`--remap-path-prefix` so that two consecutive builds are byte-identical, and a
release is where that is checked rather than assumed.

## 6. The golden header

```sh
cargo xtask header
git diff --exit-code include/iznik.h
```

`include/iznik.h` is generated, and the gate already pins it — but a native
application is built against this file and not against the crate, so a release
looks at it with its own eyes. A diff here means the C ABI changed in this
release; if it did, say so where the people building against it will read it,
and if it did not, the empty diff is the evidence.

## Then

Tag, and write down which of the above was run on which machine. The soak and
the baseline are measurements of a machine, and a report that does not say
which one is not a measurement.
