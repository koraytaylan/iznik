# Plan 0006 — Client API and Handoff — 🚧 In progress

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** 🚧 In progress.

- **Goal:** publish a stable C ABI over `iznik-client` with a byte-pipe surface shaped for libghostty, a golden-tested header and a C smoke program, diagnostics that isolate a fault to one layer, a normative client contract, and a soak that proves the system holds for hours.
- **Root cause:** the macOS application is built separately, in another language, on another machine — so the boundary has to be specified rather than discovered, proven with C rather than promised, and a fault spanning five layers has to be diagnosable from outside all of them.
- **Approach:** treat `docs/CLIENT.md` as the specification the implementation is held to, pin the ABI with a golden header and exercise it from C against a real local daemon reached by a `unix:` alias, ship one command that reports which layer is broken with secrets redacted by construction, and soak before release.
- **Progress:** 2/8 tasks done; 0 blocked; 0 dropped.
- **Integration:** `planned`; run —; base `develop`; validation base —; mode —; final integration —.
- **Exceptions:** — (coordinator-owned blocked/dropped reasons are recorded here).
- **Review of `bbf5669`:** nine findings, every one of them real. The high one
  was a command pinned for ever: a command answered but not yet announced is
  kept applied until the model reaches the generation the answer named, and a
  daemon that was replaced begins again at nothing — so the number it was
  answered at is never reached, nothing expires it (it was answered) and
  nothing rolls it back (it was not refused), and it was re-applied on top of
  every snapshot the new host sent. A generation that goes backwards is now
  read for what it is, another daemon's first word, and what the one that is
  gone answered stops being shown.

  Three more were about what one layer tells the next. Credit for a pane whose
  channel another pane had taken went out on the control channel, where no
  pane could get it, while the window this client believed it had returned
  grew — it is refused now, by a model that says such a pane has no channel at
  all. A change this client could not fit was passed on to the application
  regardless, which would have applied what this one refused. And an event
  carrying a change carried no generation, though the encoded change does not
  hold one and the protocol's own `apply` will not take it without one: an
  application could not have used what it was handed.

  Two were about letting go. A pane's handlers were read out of the map and
  called with the lock released, so an application that detached from one
  thread could free its context while a callback was running on another — the
  obligation the header states was not keepable. Letting a pane go, and
  replacing the event callback, now wait for a call that is already running,
  and a handler that lets its own pane go waits for nothing, because it is the
  call. And a host's task that died with an order in hand dropped it; it goes
  back on the pile the next connection carries, under the same rule as one
  that arrived while there was none.

  The rest: `log_path` had been part of the published ABI and was read by
  nobody, so an application that named a file got silence — it is honoured
  now, a host's every move is written to it, and a file that cannot be opened
  is a refusal rather than a quiet nothing; the regression harness's byte
  buffer assumed a stream with no holes in it, which a resume served by a
  screen puts a hole in; and two of the atomicity case's own markers were
  shaped exactly like a caller's line, so two of the hundred were proven by a
  line the case had typed itself.

  The two cases the review's findings needed are in a file of their own,
  `crates/iznik-client/tests/manager_traffic.rs`, which no task's `touches`
  names: `connection_manager.rs` was at eight hundred lines before them and a
  thousand is the most a file may have. A shared module would have been the
  other way, and this workspace forbids the waiver that makes one possible —
  every item a test binary compiles must be one it uses.

  One of the nine has no case of its own: making a write fail in the middle of
  a live session is a race, so the order a dying link drops is put through the
  same `keep` the disconnected path uses and read rather than driven.

- **Review of `9419e1a`:** six more, all in what the review before it added.
  The log was the substantial one: a subscriber belongs to a process and is
  installed once, so a second client naming a second file opened it, failed to
  install, and reported success — leaving exactly the empty log the refusal
  exists to prevent. Where this process writes is remembered now. Two were
  about saying what happened: credit for a pane between channel announcements
  was refused as an unknown host, which a held and connected host is not, and
  a snapshot settled mid-session dropped the account of the commands it gave
  up on. Two were one rule applied unevenly: a model that could not be read
  was passed on though a change that did not fit was not, and attaching over
  an attachment did not wait for a handler that was running though detaching
  did. And one was waste: an order was copied before every send so that a
  dying link could hold it, though the pile holds four kinds and none of them
  is a keystroke.

- **Review of `e9306aa`:** eight, and one of them was a conflict this session
  had just written: the burst measurement asks for the machine while the group
  it had been put in has twelve threads, so it is taken out of the group by
  name. Two were claims and comments saying more than their proofs did. Three
  were the log again — a refusal that left an empty file behind, an `# Errors`
  that named one of its three refusals, and a statement covering a case its
  test did not reach. One was a line written under the model's lock, which is
  a file every other caller would be waiting on: what a replaced daemon
  answered comes back as an effect now, and is written where the lock is not
  held. One was an attachment that installed itself before the subscription
  that could refuse it, leaving an application that frees what it passed
  pointing the boundary at freed memory. And one was a case that watched for a
  handler that had *ever* begun rather than one running now.

  Making the account an effect turned out to hide a second bug: a snapshot
  that was applied perfectly well now came back with something to say, and the
  rule "pass on what was taken" read that as a refusal and swallowed the
  snapshot. The rule names the two refusals instead, and a case drives a
  daemon that answers a command and then starts again to hold it there.

- **Review of `f264838`:** eight, and three of them were about what an event
  says. A pane that had gone arrived as a host state carrying "pane 7
  detached", which an application showing states where the connection belongs
  would print in place of "connected"; it has a kind of its own and names its
  pane. A screen arrived without the size it was drawn at, though the pane
  path's own obligation is to reset a surface to that size before feeding it
  the bytes — so an application watching without a pane handler repainted into
  a surface of the old size and put every byte after it in the wrong cell. And
  an answer that could not be encoded was dropped silently, leaving whoever
  held its number waiting for ever; it comes through in words instead.

  Two were about process-wide state and time. The log was claimed before the
  runtime and the artifacts, so a client that failed to be built for another
  reason left the file claimed for the life of the program and every corrected
  retry refused; it is claimed last, when nothing else can refuse. And giving
  up on what a replaced daemon answered was decided by the generation going
  backwards, which a daemon that starts again does not always do — a new
  connection gives up on every command it answered and never announced,
  because the announcement was owed on the link that is gone.

  The rest: `PendingCommand`'s documentation still described a field this work
  removed; `HostView::retire` had lost its only caller and, unlike its
  siblings, had not been taught that a command can be answered and still
  showing, so the next caller to reach for the obvious name would have dropped
  one; and the boundary never said that its events carry what the host has
  said and never what this client is showing ahead of it, which is what an
  application needs to know to decide whether to show a change of its own.

  The crate root passed a thousand lines under the fixes, so what turns an
  event of iznik's own into one of the application's is `shape.rs` now, and
  the root does the pointer work.

- **Carried back into 0005:** `pane-byte-pipe` found that a host's task let
  what a host was saying starve the orders already waiting for it, so a burst
  of input went out one to a round trip — a hundred lines took seconds instead
  of milliseconds. The task's loop now takes the orders it has before it hears,
  bounded by `ORDERS_PER_TURN` so a caller who never stops ordering still
  leaves it hearing. Proven by
  `connection_manager_carries_a_burst_as_fast_as_it_is_given`, which fails
  without the fix and passes in a thirtieth of its budget with it.
- **Outcome:** A native application can be built against a written, golden-pinned contract without reading Rust, a C program proves the ABI end to end, any fault in the stack can be isolated to one layer with a single command, and the release checklist has a soak behind it.

_Last updated: 2026-08-29, against `develop` @ `bbf00e4`._
