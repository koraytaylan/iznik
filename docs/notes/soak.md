# Soak report

This is the run this task commits: ten minutes with a two-minute warmup, taken
on 2026-08-29 from the tree that became this commit, whose parent is `c45ddd2`.
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
   given, so that line arrives only once the flood has been produced — and
   what is waited for is not what was typed, because a terminal echoes a line
   as it is typed and a wait for a string that appears in the typing is over
   before the shell has read it. The
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
and waited out, so that the ring every pane keeps was full before the first
sample was taken. A server still filling a four-mebibyte ring is growing for a
reason that is not a leak. That flood is poured before the held client
attaches: no client can be carried along six megabytes at once — it would fall
far enough behind for the host to stop streaming to it and send the truth
instead, which is the one thing this run reads as a byte lost.

## What was checked

Each of these fails the run, and each of them can:

- **Growth** past four mebibytes an hour on any of the three sides, read as
  the slope of the line that fits every measured sample — not as two points,
  which see only what happened between them and are blind to a leak that
  begins late;
- a side never weighed at all, or last weighed long before the run ended, or
  with too few samples after the warmup to read a line through — each of which
  is a census that stopped finding something, and each of which would
  otherwise report a flat series and no leak;
- the held client gone before the end, its pane detached, or nothing heard
  from it at all — a stream that stopped early has no gap in it either;
- the held client hearing fewer bytes than its pane was made to say;
- a screen count that is not exactly one: the first is the attachment, and any
  after it are bytes the host could not carry the client on from;
- fewer than half the rounds finishing, or more than three failing one after
  another, which is what a stack that has stopped answering looks like — a
  failing round is slower than a healthy one, so counting them is not enough;
- fewer than half of them churning a session, since the second daemon is
  weighed for the churn and an idle one is flat for a reason that is not the
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
- **Rounds:** 29 attempted, 29 finished, 29 flooded, longest run of failures 0
- **Cuts:** 29, each seen to have stopped the daemon it named
- **Pane churn:** 29 sessions made and unmade
- **Held client:** 9683 deliveries, 5771602 bytes, 1 screens, attached at byte 6288982
- **Growth ceiling:** 4194304 bytes an hour, after the warmup


## The held client, in bytes

| At | Resident |
|---|---|
| 21s | 2801664 |
| 84s | 2797568 |
| 148s | 2813952 |
| 212s | 2805760 |
| 276s | 2822144 |
| 340s | 2818048 |
| 403s | 2809856 |
| 467s | 2813952 |
| 531s | 2813952 |
| 595s | 2822144 |

Growth after the warmup: 44147 bytes an hour.

## The daemon it watches, in bytes

| At | Resident |
|---|---|
| 21s | 10776576 |
| 84s | 10801152 |
| 148s | 10813440 |
| 212s | 10817536 |
| 276s | 10821632 |
| 340s | 10821632 |
| 403s | 10829824 |
| 467s | 10854400 |
| 531s | 10862592 |
| 595s | 10870784 |

Growth after the warmup: 492396 bytes an hour.

## The daemon it churns, in bytes

| At | Resident |
|---|---|
| 21s | 4460544 |
| 84s | 4485120 |
| 148s | 4485120 |
| 212s | 4489216 |
| 276s | 4489216 |
| 340s | 4493312 |
| 403s | 4493312 |
| 467s | 4493312 |
| 531s | 4493312 |
| 595s | 4493312 |

Growth after the warmup: 60531 bytes an hour.
