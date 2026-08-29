# Soak report

This is the run this task commits: ten minutes with a two-minute warmup, taken
on 2026-08-29 from the tree that became this commit, whose parent is `9c1f87f`.
It is not the run a release needs. That one is six hours, is run by a person,
and is the first item on the [release checklist](release-checklist.md); this
one is here so that the shape of a report is in the tree, and so that a change
which breaks the soak breaks something visible.

Reproduce it with:

```sh
cargo xtask soak --duration 10 --warmup 2
```

What ran: two hosts in containers, a session on the first of them, and a
client holding that pane open for the whole run through `iznik tail` — the one
client here that returns credit for every byte it takes. Round after round,
back to back: thirty thousand lines poured through the pane and waited for,
the daemon stopped underneath it until the client noticed, started again, and
a line typed afterwards that had to come back on the subscription made before
the drop. Beside all of it, on the second host, a session made, a hundred
keystrokes timed and the session unmade, over and over.

Before any of it, eight hundred thousand lines were poured through the pane so
that the ring every pane keeps was full before the first sample was taken. A
server that is filling a four-mebibyte ring is growing for a reason that is
not a leak, and a ten-minute run has no time to wait it out.

Both sides are weighed once a minute out of `/proc`. The daemon is weighed and
the per-connection relay is not: a host runs one of each and both are called
`iznik-server`, so a weighing that took the first one it found would be of the
daemon at one sample and of a relay at the next. Growth is read as the middle
of the first half of the measured samples against the middle of the second,
because a flood in flight puts a peak on whichever sample it lands on and two
points would be at the mercy of which one each of them caught.

The held client's own stream is the byte-loss check: every delivery it was
sent begins where the one before it ended, across every drop. What it printed
is reduced to three numbers a line as it is written, so that the whole of a
run is checked rather than the end of it.

- **Machine:** Linux 7.0.0-29-generic x86_64, 16 cores
- **Duration:** 10 minutes
- **Warmup:** 2 minutes
- **Rounds:** 125 attempted, 125 finished
- **Pane churn:** 125 sessions made and unmade
- **Held client:** 48101 deliveries, 26790352 bytes, whole across every drop
- **Growth ceiling:** 4194304 bytes an hour, after the warmup


## The client, in bytes

| At | Resident |
|---|---|
| 4s | 2764800 |
| 67s | 2772992 |
| 129s | 2793472 |
| 192s | 2793472 |
| 254s | 2793472 |
| 317s | 2801664 |
| 379s | 2797568 |
| 441s | 2822144 |
| 504s | 2822144 |
| 566s | 2789376 |

Growth after the warmup: 414534 bytes an hour.

## The server, in bytes

| At | Resident |
|---|---|
| 4s | 10702848 |
| 67s | 10723328 |
| 129s | 10698752 |
| 192s | 10752000 |
| 254s | 10784768 |
| 317s | 10809344 |
| 379s | 10801152 |
| 441s | 10752000 |
| 504s | 10780672 |
| 566s | 10784768 |

Growth after the warmup: 0 bytes an hour.
