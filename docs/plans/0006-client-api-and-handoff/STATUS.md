# Plan 0006 — Client API and Handoff — ✅ Done

The roll-up row in [../STATUS.md](../STATUS.md) must stay in sync with this file. Task-level truth lives in [tasks/](tasks/) frontmatter; Makina's integration coordinator updates both layers.

- **Status:** ✅ Done.

- **Goal:** publish a stable C ABI over `iznik-client` with a byte-pipe surface shaped for libghostty, a golden-tested header and a C smoke program, diagnostics that isolate a fault to one layer, a normative client contract, and a soak that proves the system holds for hours.
- **Root cause:** the macOS application is built separately, in another language, on another machine — so the boundary has to be specified rather than discovered, proven with C rather than promised, and a fault spanning five layers has to be diagnosable from outside all of them.
- **Approach:** treat `docs/CLIENT.md` as the specification the implementation is held to, pin the ABI with a golden header and exercise it from C against a real local daemon reached by a `unix:` alias, ship one command that reports which layer is broken with secrets redacted by construction, and soak before release.
- **Progress:** 8/8 tasks done; 0 blocked; 0 dropped.
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

- **Review of `014cbef`, and of `06440a4` beside it:** twenty and fifteen
  findings. Two of them were the soak's own instruments lying about what they
  had seen.

  Every step waited for a string that was part of the line it had just typed,
  and a terminal echoes a line as it is typed — so no step ever waited for its
  flood. The awaits were over before the shell had read the command, which
  meant the cut landed mid-flood, the recovery step proved that a pane echoes
  keystrokes, and the wait that was supposed to let the ring-filling flood
  finish returned in milliseconds. Seven scenarios elsewhere in this suite
  already type `echo apart-$((6*7))` and wait for `apart-42` for exactly this
  reason; the soak had dropped a house convention. Its markers are typed with
  an empty pair of quotes in the middle of them now, which a shell takes off
  and a terminal does not.

  And nothing witnessed the flood that fills the ring. A step that ends on an
  input reports success whether or not the pane took it, and a pane that
  refuses one says so asynchronously to nobody. What witnesses it now is where
  the held client attached: the byte it is told the pane has already reached,
  which for a pane that has poured eight hundred thousand lines is exactly
  what the arithmetic says it should be.

  Growth was read from two points — the middles of the two halves — which sees
  only what happened between them: a leak beginning in the last quarter of a
  run reported zero bytes an hour at any size, and that is the shape a soak of
  hours exists to reach. It is the slope of the line fitted through every
  measured sample now, in whole numbers, which reads growth wherever it
  happens and moves by a fraction of one sample that caught a flood.

  Three more were checks that could not fire or fired wrongly. A stack that
  dies partway through fails every round from then on, but a failing round is
  slower than a healthy one, so counting them against the attempts could stay
  under half for hours — a run of failures one after another is refused now. A
  census that stopped finding a side left a series that ended early, and a
  rate from it is a rate from whenever it stopped. And the held client was
  interrupted with no chance to catch up, against a byte floor whose margin is
  a tenth of a percent — it is let drain first.

  The rest: the cut told the daemon from a relay by name alone, which is the
  distinction the census beside it exists to make, and said why it refused on
  the stream a failed command's words are dropped from; the client census
  counted any process of that name, and the churn runs one in the same
  container; the container margin was still under the deadlines this module
  allows; and the scenario proved the guard that leaves the relay out with a
  replica of it rather than with the guard.

  From `06440a4`: the README's handoff section named a library file cargo does
  not write, a socket path that is wrong on the platform the section is for, a
  distribution command that refuses on a stock Mac, and an environment
  variable the library does not read — all four now say what the code does.

- **What `handoff-documentation` found.** The documents said one thing that
  was not true and one that could not be: the root README described plan 0006
  as designed and not built, and every crate README said its tests would
  arrive with the tasks that filled its modules. Both are in the present tense
  now, and the README gained the section a person building the macOS
  application starts from — the contract, the header and the archive, the
  artifacts a bootstrap uploads, the `unix:` alias that needs no SSH, and the
  one command that says which layer is broken.

  It also found a command that could not be asked what it took. Five of the
  six `iznik` subcommands read `--help` as a host alias and went off to reach
  a machine by that name; only the dispatcher answered. The rule this
  workspace holds documents to — every command a document names answers
  `--help` — could not have been applied to that binary, and the README now
  names two of them. Each subcommand answers for itself now, with the flag
  anywhere in the line, proven by a scenario of `plumbing-commands`.

  **Deviations from the task's `touches`, all recorded.** The fix above is
  `crates/iznik-cli/src/*.rs` and its proof is a scenario and a claim of
  `plumbing-commands`, neither of which this task names; `CONTRIBUTING.md` was
  added to the documents `xtask/tests/readme_commands.rs` holds the binary to,
  because it is where a person is told which gate to run. What was *not* done:
  the same document rule wants a case for the `iznik` binary, and that needs
  `iznik-harness` as a development dependency of `iznik-cli`. Section 3.9 says
  a manifest is written once and adding a dependency is an architecture change
  that lands with its justification, so it is left for one rather than taken
  here; the scenario proves the behaviour in the meantime.

- **The last of it.** A fifth pass found the soak's machinery clean and four
  things around it. The step that waits out the ring-filling flood had thirty
  seconds for everything — a dial the product allows forty for included — so a
  slow dial would have failed all twenty askings for the same reason and
  refused a healthy six-hour run before it measured anything; the step has as
  long as any other now and it is the *wait* that is short, which is what
  makes a failed asking cheap. What the held client heard was thrown away
  whenever a container would not answer, printing hours of hearing as a client
  that heard nothing — the one reading the counting exists to prevent — and a
  broken stream skipped the judgement entirely, so a run could end without the
  ceiling ever being applied. Both are kept and made now: the tally survives
  every ending, and the judgement is made beside whatever went wrong.

  The last was the container margin's own arithmetic, wrong for the second
  time. It is counted properly now — every deadline this module allows, named
  — and says what it is: three hours against four of allowance that cannot all
  be spent at once, because a margin covers the weather rather than the sum of
  every worst case.

- **Convergence.** A fourth pass over the soak found its machinery clean —
  the cut's lock path and guards, what `iznik tail` prints, the fit's
  arithmetic at six-hour magnitudes, and every figure in the committed note
  reproducing from its own samples. Six things remained, and four were
  documents: the README's `unix:` line dropped a path component on Linux and
  named a type nothing declares; two claim statements described the estimator
  this work replaced and miscounted the commands that used to mistake `--help`
  for a host; and the container margin's own arithmetic understated the
  deadlines it is sized against by about fifty minutes, which is now counted
  properly and given two hours.
  
  The sixth was a case that proved the wrong thing: the report meant to hold
  the "measured nothing after the warmup" branch was being refused by the
  staleness check beside it, so that branch was held by nothing. Its one
  sample is late in the run now, and removing the branch fails the case.

  And running everything twice more turned up a race of its own in
  `fidelity-suite`'s `wait_idle`: it returned as soon as two readings agreed,
  and on a loaded machine two readings of nothing agree — a pane that had not
  been scheduled yet looked exactly like one that had finished. It waits for
  the pane to have said something first.

- **Running everything, repeatedly.** Four passes of
  `cargo nextest run --workspace --run-ignored all` — 550 proofs — turned up
  three more things worth fixing and one that is the machine.

  The boundary's atomicity case, a hundred threads typing into one shell at
  once, counted for one slot in the group that exists for shell-driving
  tests. Beside five hundred others it got forty of its hundred lines inside
  its window and reported a byte loss that had not happened; it takes the
  group whole now, as the case that stands six shells up already did.

  The throughput baseline asked whether eight panes sustain strictly more than
  one. They saturate the same socket, so the two figures come out within a per
  cent of each other in either order, and the assertion was asking which way a
  coin landed — it failed about half the time and passed alone in under a
  second. What it means to prove is that the scheduler shares rather than
  serializes, and serializing would leave the eight an eighth; it asks for
  nine tenths now, and the claim says so.

  What is the machine: the fixture reaper's own case failed once when podman
  refused to tear down a network whose netns process was still going —
  "rootless netns: kill network process: permission denied" — and passed again
  as soon as the leavings were cleared by hand. That is podman's teardown
  racing a loaded machine, not the harness, and it is the same condition this
  plan and 0005 both record for the pane cases at their deadlines.

- **What the whole suite found.** Running every proof there is — 550 of them,
  `--run-ignored all` — turned up one that had rotted. `scenario-driver`'s
  `unsupported` scenario asked the driver for a `probe` step and expected it to
  say the kind would be filled by plan 0005; plan 0005 filled it, so the answer
  had become a parse error and the scenario had been failing quietly ever
  since. Nothing caught it because that task had no claims file at all: ten
  scenarios naming ten claims that nowhere declared them, so `claims coverage`
  had nothing to check.

  Both are closed. The driver no longer names a plan that will fill a kind —
  every plan has landed — and says instead that the kind is not one it knows;
  the scenario reaches that answer the only way anything can, by handing the
  driver a `fault`, which the runner executes outside both containers and
  never sends to it. And `regression/claims/scenario-driver.toml` now declares
  all ten.

  The only other failure in the 550 was `baseline_throughput_is_over_its_floor`
  — eight panes at 62.5 MB/s against one pane's 67.1 — at a load average of
  four, with five hundred other tests beside it. It passes on its own in under
  a second. This machine is shared with other work, and a timing figure taken
  under that load is a measurement of the machine.

- **What the release soak found, 2026-08-30.** The six hours the checklist
  asks a person for ran on 2026-08-29 from `24b057d` and passed: a thousand
  and ten rounds, every one finished and flooded, a thousand and ten cuts each
  seen to have stopped the daemon it named, one screen on the held client
  across all of them, and growth of 8388 bytes an hour on the held client
  against a ceiling of 4194304 — none at all on either daemon. `docs/notes/soak.md` is that run.

  Writing it into the note is where the defect was. The report prints a row
  per sample, so its length follows the length of the run: ten minutes is
  twelve rows a series and six hours is three hundred and thirty-seven, which
  made a note of 1150 lines against a repository that holds every file to a
  thousand — checked by `xtask`'s own length policy. The only run a release
  cares about was the one run whose report would not fit where the checklist
  sends it, and the checklist's item 1 was unexecutable as written.

  A series is now shown in at most sixty rows and the last, evenly spaced,
  with the count of samples said above the growth so nobody reads the table as
  the whole census — and the growth stays the line fitted through every
  sample, which is the half that matters. Both halves are proven and both
  fail without the fix: showing every sample again gives "a release's report
  under this note's 98 lines of prose is 1150 lines", and fitting the shown
  rows instead of all of them is caught by a series built to climb between
  them. `regression/claims/soak-and-release.toml` declares the two.

  One thing the soak swept in that should never have been committed: a
  `nohup.out` at the root, from the run's own console. It is out of the
  history rather than merely deleted from the tip — the two commits that
  carried it were rewritten before this repository was ever pushed —
  `.gitignore` refuses it now, and the transcript is kept beside the run's
  other artifacts.

- **What the release's `claims coverage` found, 2026-08-30.** Item 4 of the
  checklist — every claim over every task, which the gate does not do because
  it verifies only what a run touched — refused two of 393. Both were the
  proof and not the product, and both are races that only a run of everything
  at once is wide enough to open.

  `manager-passes-on-no-change-it-could-not-take` counted the models it was
  handed inside a loop it entered only while the scripted host had been asked
  once. Asked twice before this thread looked at all — which is what a machine
  with 381 proofs on it does — the loop never ran, and the drain behind it
  credited nothing, so a client that had done exactly what the claim says was
  reported as never passing a model on. It fails in fifteen milliseconds, not
  at a deadline, which is what said it was not load. The drain now counts what
  it drains. Reproduced by giving the client a two-second head start, which
  fails without the change and passes with it.

  `pane-assembly-exits-and-leaves-nothing` waited for a prompt mark on a
  subscription made after the pane was spawned. A broadcast keeps nothing for
  a receiver that was not yet there, so a shell that printed its first prompt
  in that gap left a wait that ended at its twenty-second deadline. The log
  says it was not the machine: the pane above it had spawned, prompted, closed
  and exited in twenty-nine milliseconds. It now types a newline after
  subscribing and waits for the prompt that answers it — a mark it caused,
  which cannot have been missed. Proven by forcing a two-second gap: reliably
  failing before, passing after.

  **The same race was latent in nine other cases in `pane.rs`**, every one of
  which spawns, subscribes and waits for the first prompt — and one of them,
  `remembers-the-alternate-screen`, then failed the gate the same way. It is
  closed for all ten now, and not by typing at the shell: three read-side
  attempts each broke five cases, because these are tight against the mark
  stream and against what the pane answers. What works is passive. A pane now
  counts the prompts its shell has printed, in `PaneState` beside `newest` and
  `exited`, incremented where the mark is sent — so it carries what the mark
  carried, that the emulator has been fed the bytes that said so, and carries
  it whenever it is asked rather than once. A case reads the count and then
  drains what the shell said starting up, which waiting for the mark used to
  take with it.

  Proven both ways: `pane-assembly-counts-a-prompt-that-was-not-heard` shows
  a receiver made after the prompt hears nothing of it while the pane still
  counts it, and all ten cases pass with a two-second gap forced between every
  spawn and its subscription — the window that reliably failed before.
  Dropping the increment fails ten of the eleven.

  With both closed, item 4 answers 392 proven, none failed, none missing, and
  the one deferred that is deferred by design: the Darwin artifacts, which
  need a Mac to build. Items 2, 3, 4 and 6 of the checklist have been run
  against this tree; item 5 waits on the two musl targets and that runner.

- **Outcome:** A native application can be built against a written, golden-pinned contract without reading Rust, a C program proves the ABI end to end, any fault in the stack can be isolated to one layer with a single command, and the release checklist has a soak behind it.

_Last updated: 2026-08-30, against `develop` @ `1a96848`._
