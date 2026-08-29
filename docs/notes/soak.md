# Soak report

This is the run this task commits: ten minutes with a two-minute warmup, taken
on 2026-08-29 from the tree that became this commit, whose parent is `b0cebe0`.
It is not the run a release needs. That one is six hours, is run by a person,
and is the first item on the [release checklist](release-checklist.md); this
one is here so that the shape of a report is in the tree, and so that a change
which breaks the soak breaks something visible.

Reproduce it with:

```sh
cargo xtask soak --duration 10 --warmup 2
```

## What ran

Two hosts in containers, a session on the first of them, and one client
holding that pane open for the whole run through `iznik tail` — the only
client here that returns credit for every byte it takes, which is what lets a
flood reach it at all.

Round after round, back to back:

1. Thirty thousand lines poured through the pane, and a line echoed after them
   that has to come back. A shell runs what it is given in the order it is
   given, so that line arrives only once the flood has been produced. The
   flood is sized to the two hundred and fifty-six kilobytes a subscription is
   given, because the client watching it is a step of the driver and a step
   returns no credit.
2. The daemon stopped where it stands for twenty seconds — twice the ten a
   client waits for a pong before it calls a link gone — and started again, so
   every client on that host has to notice and come back. What is stopped is
   checked to be the daemon before the signal is sent, and its state is read
   while it is meant to be stopped: a cut that signalled something else, or
   nothing, is not counted as a cut.
3. A fresh client that has to reach the host and get a line back from the same
   pane.

Beside all of it, on the second host, a session made, a hundred keystrokes
timed and the session unmade, over and over. Both daemons are weighed and not
only the one holding the pane: the sessions are made and unmade on the second
host, so a leak in making one would be there and nowhere else.

Before any of it, eight hundred thousand lines were poured through the pane
and waited for, so that the ring every pane keeps was full before the first
sample was taken. A server still filling a four-mebibyte ring is growing for a
reason that is not a leak.

## What was checked

Each of these fails the run, and each of them can:

- **Growth**, on every side, read as the middle of the first half of the
  measured samples against the middle of the second — because a flood in
  flight puts a peak on whichever sample catches it, and first-against-last
  would report megabytes an hour for a process that never grew.
- **A side never weighed.** A census that quietly found nothing would
  otherwise report an empty table and no leak.
- **The held client gone**, or its pane detached. A stream that stopped an
  hour in has no gap in it either.
- **Bytes that went missing.** What the held client heard is held against what
  its pane was made to say, which is arithmetic over `seq 1 30000` rather than
  a guess.
- **A second screen.** A screen is the host failing to carry a client on from
  where it was, sending the truth as it now stands instead — which is what
  losing bytes looks like from a client. One is the attachment; every one
  after it is a reconnection that could not be resumed.
- **Rounds that did not finish**: fewer than half of them finishing.
- **Sessions never churned**, by the same measure — the second daemon is
  weighed for the churn, and an idle one is flat for a reason that is not the
  absence of a leak.

What is not claimed: that consecutive deliveries beginning where the last one
ended proves no byte was lost. The sequence a client prints is its own cursor,
so that arithmetic can only catch a client whose own accounting broke. It is
checked for exactly that, and the screens carry the question it cannot answer.

Both daemons and the held client are weighed about once a minute out of
`/proc` — the sampling has a schedule, but a sample is taken between rounds
rather than in the middle of one, so the spacing is a round longer than the
interval. The
daemon is weighed and the per-connection relay is not: a host runs one of each
and both are called `iznik-server`, so a weighing that took the first one it
found would be of the daemon at one sample and of a relay at the next.

What the held client printed is reduced to four fields a line as it is
written, and read back a window of lines at a time: a command's output is
captured up to a mebibyte and the end of it is what survives, so a run read in
one go would be checked from its middle and its beginning called whole.


- **Machine:** Linux 7.0.0-29-generic x86_64, AMD Ryzen 7 PRO 8700GE w/ Radeon 780M Graphics, 61 GiB memory, 16 cores
- **Duration:** 10 minutes
- **Warmup:** 2 minutes
- **Rounds:** 29 attempted, 29 finished
- **Cuts:** 29, each seen to have stopped the daemon it named
- **Pane churn:** 29 sessions made and unmade
- **Held client:** 9808 deliveries, 5771486 bytes, 1 screens
- **Growth ceiling:** 4194304 bytes an hour, after the warmup


## The held client, in bytes

| At | Resident |
|---|---|
| 21s | 2711552 |
| 84s | 2740224 |
| 148s | 2744320 |
| 212s | 2752512 |
| 276s | 2756608 |
| 339s | 2752512 |
| 403s | 2752512 |
| 467s | 2752512 |
| 531s | 2744320 |
| 595s | 2752512 |

Growth after the warmup: 0 bytes an hour.

## The daemon it watches, in bytes

| At | Resident |
|---|---|
| 21s | 10276864 |
| 84s | 10285056 |
| 148s | 10289152 |
| 212s | 10297344 |
| 276s | 10305536 |
| 339s | 10289152 |
| 403s | 10293248 |
| 467s | 10309632 |
| 531s | 10326016 |
| 595s | 10326016 |

Growth after the warmup: 346955 bytes an hour.

## The daemon it churns, in bytes

| At | Resident |
|---|---|
| 21s | 4472832 |
| 84s | 4489216 |
| 148s | 4493312 |
| 212s | 4489216 |
| 276s | 4493312 |
| 339s | 4497408 |
| 403s | 4497408 |
| 467s | 4497408 |
| 531s | 4497408 |
| 595s | 4493312 |

Growth after the warmup: 57825 bytes an hour.
