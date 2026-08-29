# Plan 0006 — Client API and Handoff — 🚧 In progress

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** 🚧 In progress.

- **Goal:** publish a stable C ABI over `iznik-client` with a byte-pipe surface shaped for libghostty, a golden-tested header and a C smoke program, diagnostics that isolate a fault to one layer, a normative client contract, and a soak that proves the system holds for hours.
- **Root cause:** the macOS application is built separately, in another language, on another machine — so the boundary has to be specified rather than discovered, proven with C rather than promised, and a fault spanning five layers has to be diagnosable from outside all of them.
- **Approach:** treat `docs/CLIENT.md` as the specification the implementation is held to, pin the ABI with a golden header and exercise it from C against a real local daemon reached by a `unix:` alias, ship one command that reports which layer is broken with secrets redacted by construction, and soak before release.
- **Progress:** 7/8 tasks done; 0 blocked; 0 dropped.
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

- **Review of `623a2c3`:** seven, and three of them were the benchmark
  measuring the wrong thing. It returned credit for the keystroke it sent
  rather than the bytes that came back, and none at all for anything it did
  not match, so the host's window drained through the run and a longer one
  would have stalled and called it a host that stopped answering. It found the
  pane by looking for a session with the right name, which after one run is
  the session the run before it left — so the second run typed into the first
  one's pane, and every run leaked a shell; it takes the session from the
  answer to the command that made it, and closes it at the end. And it timed
  each keystroke by waiting for output containing the character it had sent,
  which anything the keystroke before it left behind satisfies at once: the
  quickest of a hundred was meaningless. It settles what is owed before each
  one and times what that one caused.

  Two were the tail's. A pane the host does not have is refused after the call
  that asked for it returned, and the refusal was thrown away — so a tail of a
  pane that does not exist printed nothing, said nothing and waited for ever,
  which is exactly what a quiet pane looks like. And it returned credit for a
  screen, which arrives on the control channel and spends none of the pane's
  window: the host was handed room this reader had not made.

  The last two were about patience. The signal a tail ends on was listened for
  only after the host had been reached, and reaching one may mean installing a
  server on it — six minutes in which an interruption killed the process
  instead of ending it. And a host that could not be reached was treated as an
  ending, though it is a state that is tried again: one transient failure
  aborted a command that would have connected a moment later, and the harness
  that models this waits forty seconds through exactly that.

- **`client-contract`:** `docs/CLIENT.md` is held to the header by two cases,
  and both were run against a contract with a name changed and an obligation
  reworded to be sure they see it. "Verbatim" is read as the words and not the
  wrapping: a C comment wraps where a comment allows and a document where a
  paragraph does, so the two are compared with the wrapping of neither.

- **`diagnostics-bundle`:** the redaction is proven over text rather than
  through a planted configuration, because `ssh` finds a user's configuration
  from the account and not from the environment — no case can point it at a
  planted one without touching the real one. So what `ssh` would say is given
  to the filter directly, and the filter is a function of its own for exactly
  that reason. Beside it, a whole bundle is collected with secrets planted in
  the environment, which proves the other half: nothing here reads one.

  The bundle waits through a failure and stops when the same failure comes
  twice. A host that could not be reached is tried again, so giving up on the
  first would report as broken a host that connects a moment later — the
  mistake a review had just found in the commands beside it. But the same
  words twice are the host saying what it is, and waiting for a third is not
  diagnosis. A ceiling stands behind both.

  Nothing a transport says goes into the document. What `ssh` prints when it
  refuses carries whatever a proxy command wrote on its own standard error,
  which is a place a token appears — so the bundle carries the kind of failure
  and the state a host was in, in this program's own words, and `iznik probe`
  is where somebody looking at their own screen sees the rest.

- **`plumbing-commands`:** two things beyond the architecture's description.
  `tail` prints the pane as it stands before it prints what the pane says
  next: a cold subscription is answered with a screen and not with history, so
  a tail that printed only bytes would print nothing until something happened
  and a reader would be reading the middle of something. It is marked as what
  it is, so a script can tell the two apart. And every command but `probe`
  reaches a host through a bootstrap, which installs the server this build
  carries — so where that server is has to be sayable, and a program run from
  a shell says it the way a program run from a shell says anything:
  `IZNIK_ARTIFACTS_DIRECTORY`, falling back to a directory under the client's
  own runtime path.

- **`static-library-and-header`:** the header is generated with cbindgen and
  committed, and three things beyond the architecture's description are worth
  recording. The codes are exported with an `IZNIK_` prefix of their own,
  because `OK` and `REFUSED` in a header somebody includes beside their own
  are four of the commonest words there are. What cbindgen produces is passed
  through one presentation step before it is written: Rust's documentation
  syntax comes through unchanged, so a link is `[`Name`]` and a section is
  headed `# Safety`, neither of which means anything in C — the links are
  flattened to the names the header uses, taken from the same table that
  renames the types so the two cannot drift, and the heading is written as a
  sentence. And `xtask` gained development dependencies its `touches` does not
  name — the testkit, the protocol and a runtime — because the smoke case
  stands a whole daemon up in its own process for a C program to talk to.

- **Review of `4da527b`:** eight, and the first was the worst kind — a case
  that could not fail. The obligation check read everything above a
  declaration rather than the block belonging to it, and the callbacks
  typedef states obligations of its own and sits above every function there
  is, so it passed for all of them and could have failed for none. It reads
  the block now, and a header generated without documentation fails it.

  Three were the C program's. Its command number was written by one thread and
  read by another with nothing between them, which C calls undefined and a
  race detector calls a bug; it is atomic, and an answer arriving before the
  number is known is still the answer, because the program sends one command.
  Its three waits had ten seconds each against a deadline of ten seconds for
  the whole run, so a stall was a killed process rather than the sentence
  saying which thing stalled: the waits share one deadline now, inside the
  runner's. And it searched each delivery of bytes on its own for the line it
  typed, which a pseudoterminal is free to split in two; it keeps what the
  pane says and searches that. A fourth was the same shape: it sent its
  command before the host had said anything, and a command handed to a host
  with nowhere to send it is dropped while the call still answers well — it
  waits for the host's model first.

  The rest: `xtask header` resolved its root from the current directory, which
  this workspace has a function for and a documented reason not to do; the
  build deadline was twice the gate's own, so a build that would not finish
  was reported as a gate that timed out; and three assertions sat after calls
  that raise rather than return on failure, so their careful sentences could
  never be printed.

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

- **Review of `8f7ef2d`:** three, and none of them deep, which is where this
  stopped. An event about a command that could not be encoded carries the
  command's number on a kind whose documentation said the number was only ever
  on an answer — so an application reading the contract would still have
  waited for ever, which is the thing that fix existed to prevent. An empty
  payload crossed as a pointer that is aligned, not null, and not anything
  either, which a C caller testing `if (event->payload)` would read; no bytes
  is null now, everywhere at the boundary. And what a manager refuses before
  its log is installed is never in that log, which is right — those are
  returned to a caller who is still there to read them, and the log is for
  what happens afterwards with nobody waiting — but the code did not say so.

- **Carried back into 0005:** `pane-byte-pipe` found that a host's task let
  what a host was saying starve the orders already waiting for it, so a burst
  of input went out one to a round trip — a hundred lines took seconds instead
  of milliseconds. The task's loop now takes the orders it has before it hears,
  bounded by `ORDERS_PER_TURN` so a caller who never stops ordering still
  leaves it hearing. Proven by
  `connection_manager_carries_a_burst_as_fast_as_it_is_given`, which fails
  without the fix and passes in a thirtieth of its budget with it.
- **What `soak-and-release` ran into.** Four things the architecture could not
  have known, each of which changed the shape of the soak rather than what it
  proves.

  A step of the driver returns no credit for a pane it subscribes to. So the
  client that watches the soak's flood cannot be one: it is `iznik tail`, which
  returns credit for every byte it takes and lives for the whole run. A round's
  own flood is sized to the two hundred and fifty-six kilobytes a subscription
  is given, because a round that poured more would stall against a host doing
  exactly the right thing, and the flood that is bigger than any window is the
  one poured before the clock starts — which also fills the four-mebibyte ring
  every pane keeps, so that what is measured afterwards is a stack at its
  steady state rather than one still filling.

  A host runs one daemon and one `--stdio` relay per connection, and both are
  called `iznik-server`. The first weighing took whichever `/proc` offered
  first, so the series alternated between two processes and reported a leak and
  a recovery that neither happened. The daemon is weighed and the relay is left
  out by name.

  Every container the fixture starts carries podman's own `--timeout`, ten
  minutes by default. Two ten-minute runs died at five hundred and fifty
  seconds before that was the answer; a soak asks for its own length and a
  quarter of an hour besides.

  And the held client's stream cannot be read back whole — twenty-six megabytes
  in ten minutes, and the harness returns the last mebibyte of what a command
  says. It is reduced to three numbers a delivery as it is written, by a filter
  the case runs against exactly what `iznik tail` prints, so that the check and
  the thing it checks are proven to agree.

  Two smaller ones. Growth is read as the middle of the first half of the
  measured samples against the middle of the second: a flood in flight puts a
  peak on whichever sample catches it, and first-against-last would have read
  megabytes an hour off a process that never grew. And a round that fails
  between the pause and the resume would leave the daemon stopped, so a failure
  starts it again and a soak in which fewer than half the rounds finished
  refuses rather than reporting the flat series it took while nothing was
  happening.

- **Review of `bed8722`:** twenty-one findings, and the two that matter said
  the same thing: the soak's advertised failure modes could not be reached.

  The byte-loss check was a tautology. `ManagerEvent::Bytes` carries the byte a
  delivery starts at, and that number is the client's own cursor before the
  payload is counted — pane frames carry no sequence at all — so a check that
  each delivery begins where the last one ended was the client's arithmetic
  compared with itself. It could not fail. Worse, the one place a lost byte is
  visible is exactly where the check looked away: a host that cannot carry a
  client on from where it was sends a screen instead, and screens were
  forgiven and not counted. What the check now does is count them — one is the
  attachment, every one after it is a resume that could not be served — hold
  what the held client heard against what its pane was made to say, which is
  arithmetic over `seq 1 30000`, and end the run on a detached pane, because a
  stream that stopped an hour in has no gap in it either. The gap check
  remains, for the one thing it can catch: a client whose own accounting
  broke.

  The held client also never experienced the drop it was supposed to prove
  continuity across. A round paused the daemon only until the round's own
  client noticed, and that client had been given a four-second pong deadline
  by the step this task generated, while the held client kept the ten seconds
  the product ships. The generated step now names no connection timing at all
  — a release soak that drove reconnection ten times faster than anything
  ships would be soaking timings nobody runs — and the cut is made by the soak
  itself, twenty seconds in one command that stops the daemon and starts it
  again, so a soak that dies in between leaves nothing frozen.

  Six more were about measuring nothing and passing for it. `grown` took the
  upper of the two middle samples, which for a half of two is the maximum: the
  committed report's "414534 bytes an hour" was a client whose last measured
  sample was below its first. A warmup at least as long as the run left
  nothing measured and passed — `cargo xtask soak --duration 10` was exactly
  that soak. A side that was never weighed passed. A held client that had died
  passed. One failed weighing five hours in threw away the whole run. And
  inverting the ceiling comparison left every case in the tree green, which is
  now the first thing `regression_soak_judges_a_run_by_what_it_measured`
  fails on.

  Three were the containers, and all three were things a person could only
  find by running them there: the engine has no `ps` at all rather than a
  busybox one, which is what the comment and a claim both said; `/bin/sh` is
  dash, where the scenario's `$((total + $(awk ...)))` is a fatal error the
  moment a process exits between being named and being weighed; and the filter
  the held client's output goes through is mawk, which reads a block at a time
  — so the first line of a quiet pane sat unwritten and the soak could not
  tell whether the client had attached at all.

  The rest: a six-hour run's stream is about thirty-eight megabytes and a
  command's output is captured up to one, tail kept, so it is now read a
  window of lines at a time; the churned daemon was never weighed, though it
  is the only one making and unmaking sessions; the report was printed only on
  success, which is exactly the run whose series somebody has to read; the
  scenario's weighing was satisfied by its own format string and proved
  nothing about the relay it claims to leave out, so it now holds one open and
  counts two; the machine was named by kernel and core count alone; and
  `Duration::from_mins` panics rather than refusing on a path whose job is to
  answer a bad command line.

  **Four deviations from the task's `touches`, all forced by one thing.** The
  module passed a thousand lines under the fixes and this workspace denies a
  file beside a directory of the same name, so `xtask/src/soak.rs` is now
  `xtask/src/soak/{mod,report,steps}.rs`. Three files follow from that move:
  `xtask/README.md` gains the two rows the documentation gate requires for the
  new modules, and `xtask/tests/skeleton.rs` names the module path in its stub
  table, which no longer existed. And `.config/nextest.toml` gains an
  override, which does not follow from the move but from the soak itself: its
  own deadlines allow far more than the ten minutes every other `regression_*`
  binary gets, so a cold machine would have reported a hung soak where there
  was a cold build. `regression_distribution_*` carries the same override for
  the same reason.

- **Review of `b0cebe0`:** twenty findings, and the governing one was that
  several of the twenty-one the round before had been moved rather than
  closed.

  Three were checks that could not fire. `grown` returning nothing was still a
  pass, so a run whose samples all fell inside its warmup — `--duration 11`
  with the default ten-minute one — skipped the ceiling on every side and
  exited zero; nothing measured is now a refusal wherever there was long
  enough to measure. `alive` could not see a dead held client: the engine's
  first process is a sleep that never reaps, so an orphan that died kept its
  entry and its name, and the census counted a zombie as a client; it leaves
  them out now and reads its answer as a number rather than as anything that
  is not a nought. And the byte floor was scaled by the rounds that finished
  while the bytes came from every round attempted, so each failed round handed
  the check a whole flood's worth of slack — up to half the run. It is scaled
  by the floods that were really poured.

  Two were things nobody watched. The churn was counted behind an `is_ok` that
  threw the reason away and no judgement ever looked at it, so a soak where
  host1 refused every benchmark weighed an idle daemon and called it no leak.
  And nothing witnessed that the cut reached anything: the round's own wait
  for `reconnecting` had gone with the rewrite, and `kill -STOP` on a
  process identifier a dead daemon left in its lock would signal whatever the
  container handed that number to next. The cut now checks the process it
  names is the daemon before it signals, reads its state while it is meant to
  be stopped, and is counted only when that state says stopped.

  Two were about the one screen. `screens > 1` had no floor under it, so if
  the attachment screen ever stopped arriving one real loss would read as the
  harmless one; and `attended` waited for any line rather than for that line.
  Exactly one is right now, and the wait is for a first word that is a screen.

  The rest: the report was still discarded when the ending failed — a detached
  pane, a jumped stream, one flaky exec after six hours — which is the same
  hazard the weighing had already been changed to avoid; the container margin
  was fifteen minutes against about twenty-one of deadlines this module allows
  before its own clock starts; the attend window was thirty seconds against a
  client dial allowed forty; `SETTLING_PATIENCE` was inert, clamped to a
  quarter of itself by a step deadline that was one constant for every step;
  the census was hand-copied into the scenario and the two had already
  diverged in the clause the claim rests on, so a case now holds the parsed
  scenario against the constant; the median case's first half discriminated
  nothing, because for an odd count the shipped arithmetic and the rejected
  one are the same expression; the processor was read from a field only x86
  exports; the filter wrote empty fields, which move every field after them
  one place left; the scenario's backgrounded relay held the step's own
  standard error open for twenty seconds; the nextest override was by binary,
  so ten microsecond cases got a half-hour deadline and every core apiece; and
  `poured` did not terminate for a number no flood will ever be.

  The module passed a thousand lines again, so what a soak asks the containers
  to do is `soak/commands.rs` now.

- **Outcome:** A native application can be built against a written, golden-pinned contract without reading Rust, a C program proves the ABI end to end, any fault in the stack can be isolated to one layer with a single command, and the release checklist has a soak behind it.

_Last updated: 2026-08-29, against `develop` @ `b0cebe0`._
