# Soak report

This is a release's run: six hours with a ten-minute warmup, taken on
2026-08-29 from `24b057d`, by a person at a terminal. It is the first item on
the [release checklist](release-checklist.md) and the one item there that
cannot be automated away — a leak of a few kilobytes a reconnection is
invisible in every other item on that list and fatal by Thursday, and the only
thing that finds it is time.

It passed. A thousand and ten rounds, every one of them finished and flooded
and not one failing; a thousand and ten cuts, each seen to have stopped the
daemon it named; a thousand and ten sessions made and unmade beside them.
The held client took a hundred and ninety-one mebibytes over three hundred and
twenty-six thousand deliveries and saw one screen — the attachment — which is
to say it was carried across every one of those cuts without the host ever
having to redraw it. The growth: eight thousand three hundred and
eighty-eight bytes an hour on the held client and none at all on either
daemon, against a ceiling of four mebibytes.

Reproduce it with:

```sh
cargo xtask soak
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
- **Duration:** 360 minutes
- **Warmup:** 10 minutes
- **Rounds:** 1010 attempted, 1010 finished, 1010 flooded, longest run of failures 0
- **Cuts:** 1010, each seen to have stopped the daemon it named
- **Pane churn:** 1010 sessions made and unmade
- **Held client:** 326820 deliveries, 201015872 bytes, 1 screens, attached at byte 6288982
- **Growth ceiling:** 4194304 bytes an hour, after the warmup


## The held client, in bytes

| At | Resident |
|---|---|
| 21s | 2994176 |
| 404s | 3006464 |
| 795s | 3035136 |
| 1178s | 3026944 |
| 1562s | 3031040 |
| 1950s | 3022848 |
| 2341s | 3031040 |
| 2724s | 3031040 |
| 3107s | 3039232 |
| 3489s | 3026944 |
| 3872s | 3026944 |
| 4268s | 3031040 |
| 4651s | 3031040 |
| 5045s | 3031040 |
| 5439s | 3035136 |
| 5823s | 3026944 |
| 6220s | 3031040 |
| 6614s | 3026944 |
| 7008s | 3026944 |
| 7391s | 3022848 |
| 7774s | 3031040 |
| 8174s | 3035136 |
| 8568s | 3031040 |
| 8950s | 3026944 |
| 9333s | 3031040 |
| 9718s | 3022848 |
| 10104s | 3043328 |
| 10489s | 3067904 |
| 10872s | 3072000 |
| 11255s | 3072000 |
| 11639s | 3063808 |
| 12025s | 3084288 |
| 12410s | 3088384 |
| 12792s | 3080192 |
| 13175s | 3080192 |
| 13557s | 3076096 |
| 13940s | 3088384 |
| 14323s | 3076096 |
| 14705s | 3031040 |
| 15088s | 3031040 |
| 15470s | 3026944 |
| 15853s | 3031040 |
| 16235s | 3026944 |
| 16618s | 3047424 |
| 17000s | 3043328 |
| 17383s | 3047424 |
| 17766s | 3072000 |
| 18148s | 3063808 |
| 18531s | 3072000 |
| 18913s | 3072000 |
| 19296s | 3067904 |
| 19678s | 3067904 |
| 20061s | 3063808 |
| 20444s | 3076096 |
| 20826s | 3076096 |
| 21209s | 3072000 |
| 21592s | 3067904 |

Of 337 samples this shows 57, evenly spaced and ending on the last. The growth below is read from every one of them.

Growth after the warmup: 8388 bytes an hour.

## The daemon it watches, in bytes

| At | Resident |
|---|---|
| 21s | 11534336 |
| 404s | 11251712 |
| 795s | 11321344 |
| 1178s | 11317248 |
| 1562s | 11296768 |
| 1950s | 11288576 |
| 2341s | 11313152 |
| 2724s | 11313152 |
| 3107s | 11329536 |
| 3489s | 11300864 |
| 3872s | 11296768 |
| 4268s | 11333632 |
| 4651s | 11313152 |
| 5045s | 11337728 |
| 5439s | 11300864 |
| 5823s | 11321344 |
| 6220s | 11366400 |
| 6614s | 11325440 |
| 7008s | 11325440 |
| 7391s | 11300864 |
| 7774s | 11300864 |
| 8174s | 11296768 |
| 8568s | 11378688 |
| 8950s | 11341824 |
| 9333s | 11386880 |
| 9718s | 11436032 |
| 10104s | 11395072 |
| 10489s | 11354112 |
| 10872s | 11235328 |
| 11255s | 11268096 |
| 11639s | 11309056 |
| 12025s | 11280384 |
| 12410s | 11358208 |
| 12792s | 11309056 |
| 13175s | 11304960 |
| 13557s | 11280384 |
| 13940s | 11325440 |
| 14323s | 11280384 |
| 14705s | 11309056 |
| 15088s | 11321344 |
| 15470s | 11407360 |
| 15853s | 11321344 |
| 16235s | 11317248 |
| 16618s | 11296768 |
| 17000s | 11362304 |
| 17383s | 11354112 |
| 17766s | 11329536 |
| 18148s | 11333632 |
| 18531s | 11345920 |
| 18913s | 11264000 |
| 19296s | 11309056 |
| 19678s | 11276288 |
| 20061s | 11300864 |
| 20444s | 11280384 |
| 20826s | 11292672 |
| 21209s | 11317248 |
| 21592s | 11276288 |

Of 337 samples this shows 57, evenly spaced and ending on the last. The growth below is read from every one of them.

Growth after the warmup: 0 bytes an hour.

## The daemon it churns, in bytes

| At | Resident |
|---|---|
| 21s | 4431872 |
| 404s | 4259840 |
| 795s | 4259840 |
| 1178s | 4263936 |
| 1562s | 4259840 |
| 1950s | 4259840 |
| 2341s | 4247552 |
| 2724s | 4247552 |
| 3107s | 4259840 |
| 3489s | 4268032 |
| 3872s | 4268032 |
| 4268s | 4263936 |
| 4651s | 4268032 |
| 5045s | 4263936 |
| 5439s | 4259840 |
| 5823s | 4268032 |
| 6220s | 4263936 |
| 6614s | 4263936 |
| 7008s | 4268032 |
| 7391s | 4263936 |
| 7774s | 4263936 |
| 8174s | 4263936 |
| 8568s | 4263936 |
| 8950s | 4263936 |
| 9333s | 4259840 |
| 9718s | 4259840 |
| 10104s | 4268032 |
| 10489s | 4268032 |
| 10872s | 4202496 |
| 11255s | 4194304 |
| 11639s | 4194304 |
| 12025s | 4194304 |
| 12410s | 4198400 |
| 12792s | 4198400 |
| 13175s | 4198400 |
| 13557s | 4198400 |
| 13940s | 4198400 |
| 14323s | 4198400 |
| 14705s | 4194304 |
| 15088s | 4194304 |
| 15470s | 4198400 |
| 15853s | 4198400 |
| 16235s | 4198400 |
| 16618s | 4194304 |
| 17000s | 4202496 |
| 17383s | 4206592 |
| 17766s | 4190208 |
| 18148s | 4210688 |
| 18531s | 4210688 |
| 18913s | 4206592 |
| 19296s | 4194304 |
| 19678s | 4206592 |
| 20061s | 4206592 |
| 20444s | 4198400 |
| 20826s | 4202496 |
| 21209s | 4202496 |
| 21592s | 4202496 |

Of 337 samples this shows 57, evenly spaced and ending on the last. The growth below is read from every one of them.

Growth after the warmup: 0 bytes an hour.
