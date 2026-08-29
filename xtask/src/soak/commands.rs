//! What a soak asks the containers to do.
//!
//! Every command here is a shell fragment run inside a container, written for
//! the `dash` and the `mawk` these images carry. They are together because
//! what they say is a thing of its own: a census of processes, a client
//! started and stopped, a link cut and made good again.

use core::time::Duration;

use iznik_harness::fixture::Fixture;

use crate::soak::{
    ARTIFACTS, CLIENT_PROCESS, COMMAND_DEADLINE, DROP_SECONDS, KIBIBYTE, NOT_THE_DAEMON,
    SERVER_PROCESS, Sample, SoakError, TOOL, WAS, WATCHED,
};

/// Where the held client writes what it hears.
pub const TAIL_OUTPUT: &str = "/tmp/iznik-soak-tail.lines";

/// And where the program that compacts it on the way there lives.
const TAIL_FILTER: &str = "/tmp/iznik-soak-compact.awk";

/// What tells that program to read a line at a time rather than a block of
/// them. Without it the first thing the held client says sits unwritten until
/// enough has followed it, and what is waiting for that line is the soak
/// deciding whether the client ever attached at all.
const UNBUFFERED: &str = "-W interactive";

/// How many tenths of a second the held client is given to finish writing
/// after it is interrupted. Ten seconds, which is far longer than flushing a
/// file takes and short enough that a client that will not go is noticed.
const STOP_ATTEMPTS: u32 = 100;

/// What the held client's output is reduced to as it is written.
///
/// A soak of hours moves gigabytes through the pane it holds, and what the
/// byte-loss check needs of each delivery is three numbers: where it starts,
/// how many characters of base 64 carry it, and how many of those are
/// padding. Kept whole, the file would be tens of megabytes and a command's
/// output is captured up to one; reduced here, a six-hour run is a few
/// megabytes and it is read back a window of lines at a time.
///
/// Every field is written whatever the line held, because a field left empty
/// would move every field after it one place to the left and turn a delivery
/// the filter did not understand into one the check silently skips.
pub const COMPACT: &str = concat!(
    "{ kind = \"none\"; if (match($0, /\"kind\":\"[a-zA-Z_]+\"/)) ",
    "{ kind = substr($0, RSTART + 8, RLENGTH - 9) } ",
    "at = 0; if (match($0, /\"sequence\":[0-9]+/)) ",
    "{ at = substr($0, RSTART + 11, RLENGTH - 11) } ",
    "long = 0; padding = 0; if (match($0, /\"bytes\":\"[^\"]*\"/)) ",
    "{ held = substr($0, RSTART + 9, RLENGTH - 10); long = length(held); ",
    "while (substr(held, length(held), 1) == \"=\") ",
    "{ padding = padding + 1; held = substr(held, 1, length(held) - 1) } } ",
    "print kind, at, long, padding; fflush() }"
);

/// Where its standard error goes, so that a client that refused to start
/// says why rather than saying nothing.
pub const TAIL_TROUBLE: &str = "/tmp/iznik-soak-tail.err";

/// What a side weighs, read out of `/proc` because the engine image carries
/// no `ps` at all — it is as bare as a machine somebody has just installed,
/// which is the point of it.
///
/// Public, and written into the scenario that proves it word for word: a
/// census transcribed into a scenario is a replica, and a replica proves
/// nothing about the one a soak runs. A case holds the two texts together.
///
/// The name is matched whole and the relay is left out. A host runs one
/// daemon and one `--stdio` relay per client connection, and both are called
/// `iznik-server`: a weighing that took the first of them found would be of
/// the daemon at one sample and of a relay at the next, and the series would
/// say a leak and a recovery that neither happened. What is weighed is the
/// daemon — the process that holds the panes and the history, which is where
/// a leak would live — and what is printed is the total of every process that
/// answers to that, so that two of them would be seen rather than one of them
/// picked.
pub const WEIGH: &str = "for held in /proc/[0-9]*; do \
                     if grep -qsx NAME \"$held/comm\" && \
                     ! grep -qsa -- --stdio \"$held/cmdline\"; then \
                     awk '/VmRSS/{print $2}' \"$held/status\"; fi; done \
                     | awk '{total += $1} END {print total + 0}'";

/// And what the held client weighs.
///
/// Told apart by what it is doing and not only by what it is called: the
/// churn runs a command of the same name in the same container every round,
/// and one that overran its deadline is still there when the weighing comes.
/// The held client is the one tailing a pane.
pub const WEIGH_CLIENT: &str = "for held in /proc/[0-9]*; do \
                                if grep -qsx CLIENT \"$held/comm\" && \
                                grep -qsa -- tail \"$held/cmdline\"; then \
                                awk '/VmRSS/{print $2}' \"$held/status\"; fi; done \
                                | awk '{total += $1} END {print total + 0}'";

/// A command that says how many processes answer to a name and are running.
///
/// A zombie is left out. The engine's first process is a sleep that never
/// reaps anything, so a held client that died is not gone: it keeps its
/// entry, and its name in it, until the container ends. Counting one would
/// be counting a client that is no longer listening as one that is.
///
/// And so is anything of that name that is not tailing a pane: the churn runs
/// a command called the same thing in the same container every round, and one
/// that overran its deadline outlives the command that started it.
#[must_use]
pub fn running(process: &str) -> String {
    format!(
        "found=0; for held in /proc/[0-9]*; do \
         if grep -qsx {process} \"$held/comm\" && \
         grep -qsa -- tail \"$held/cmdline\" && \
         ! awk '/^State:/{{print $2}}' \"$held/status\" 2>/dev/null | grep -qx Z; then \
         found=$((found + 1)); fi; \
         done; printf '%s\\n' \"$found\""
    )
}

/// The command that starts the held client.
///
/// The one client that lives for the whole soak, and the one that returns
/// credit for everything it takes. What it says goes through the filter above
/// on its way to a file, so that what is read back at the end is the whole
/// run rather than the end of it — and the filter is told to read a line at a
/// time, because the one in these containers otherwise waits for a block of
/// them and a pane that has just been attached to says one line and then goes
/// quiet.
#[must_use]
pub fn tail_command() -> String {
    format!(
        "cat > {TAIL_FILTER} <<'COMPACT'\n{COMPACT}\nCOMPACT\n\
         {{ IZNIK_ARTIFACTS_DIRECTORY={ARTIFACTS} nohup {TOOL} tail {WATCHED} 1 \
         | awk {UNBUFFERED} -f {TAIL_FILTER} > {TAIL_OUTPUT}; }} 2> {TAIL_TROUBLE} &"
    )
}

/// And the command that stops it.
///
/// Interrupted, which is the ending it is written to have, and then waited
/// for: what it wrote is only all there once both it and the filter it feeds
/// have gone. Found by name rather than by a process id it wrote down,
/// because what a pipeline answers with is the process id of its last command
/// and the one to interrupt is its first.
#[must_use]
pub fn stop_tail() -> String {
    format!(
        "for held in /proc/[0-9]*; do \
         if grep -qsx {CLIENT_PROCESS} \"$held/comm\"; then \
         kill -INT \"${{held#/proc/}}\"; fi; done; \
         for _ in $(seq 1 {STOP_ATTEMPTS}); do \
         if ! pidof awk {CLIENT_PROCESS} > /dev/null 2>&1; then break; fi; \
         sleep 0.1; done"
    )
}

/// How the link to every client on a host is cut, and made good again.
///
/// The daemon is stopped where it stands and started again in the same
/// command, so a soak that dies between the two leaves nothing frozen behind
/// it. Its process id is read from the lock it holds — the same lock the
/// driver's own fault reads — rather than found by name, because a host runs
/// a relay per connection under that name as well.
///
/// Stopped for the seconds the module names, which is longer than the ten a
/// client waits for a pong before it calls a link gone, so that every client
/// on that host has to notice and come back. A cut nobody noticed proves
/// nothing about coming back from one.
///
/// What it stopped is checked twice over. The process the lock names must
/// answer to the daemon's name and not be a relay — a lock a dead daemon left
/// behind names a process identifier a container will hand out again, and on
/// a host the likeliest thing to be holding it is another `iznik-server`,
/// which is the distinction the census beside this one exists to make. And
/// its state is read while it is meant to be stopped and said on the way out,
/// so that a cut which signalled something and stopped nothing is not
/// reported as a cut. The refusal goes to standard error, because what a
/// command that exits non-zero said on standard output is not kept.
#[must_use]
pub fn cut() -> String {
    format!(
        "if [ -n \"$XDG_RUNTIME_DIR\" ]; then lock=\"$XDG_RUNTIME_DIR/iznik/server.lock\"; \
         else lock=\"${{TMPDIR:-/tmp}}/iznik-$(id -u)/server.lock\"; fi; \
         held=$(cat \"$lock\"); \
         if ! grep -qsx {SERVER_PROCESS} \"/proc/$held/comm\" || \
         grep -qsa -- --stdio \"/proc/$held/cmdline\"; then \
         printf '{NOT_THE_DAEMON} %s\\n' \"$held\" >&2; exit 1; fi; \
         kill -STOP \"$held\"; sleep {DROP_SECONDS}; \
         printf '{WAS}%s\\n' \"$(awk '/^State:/{{print $2}}' \"/proc/$held/status\")\"; \
         kill -CONT \"$held\""
    )
}

/// And how a daemon left stopped is started again.
///
/// Run when the cut itself could not be, because a cut that stopped a daemon
/// and then failed before starting it again would leave every round after it
/// soaking a host that is not running. Continuing a process that was never
/// stopped is nothing, so this is safe whatever went wrong.
#[must_use]
pub fn revive() -> String {
    format!(
        "for held in /proc/[0-9]*; do \
         if grep -qsx {SERVER_PROCESS} \"$held/comm\"; then \
         kill -CONT \"${{held#/proc/}}\"; fi; done"
    )
}

/// What machine this is, as a person would say it.
///
/// The processor and the memory as well as the kernel, because a soak is a
/// measurement of a machine and two machines with the same kernel and the
/// same number of cores are not the same machine. Read from `/proc` inside a
/// container, which shares the host's kernel and sees the host's processor;
/// what a container narrows is how much of it this may use, and the count of
/// cores says that.
///
/// # Errors
///
/// [`SoakError::Fixture`] when the engine will not say.
pub fn machine(fixture: &Fixture) -> Result<String, SoakError> {
    let said = fixture.exec(
        "engine",
        "printf '%s, %s, %s memory, %s cores' \
         \"$(uname -srm)\" \
         \"$(awk -F: '/model name|Model name|^Hardware|^CPU part/{print $2; exit}' \
         /proc/cpuinfo | sed 's/^ *//')\" \
         \"$(awk '/MemTotal/{printf \"%d GiB\", $2 / 1048576}' /proc/meminfo)\" \
         \"$(nproc)\"",
        COMMAND_DEADLINE.0,
    )?;
    let named = String::from_utf8_lossy(&said.stdout).trim().to_owned();
    // A machine that would not say what processor it has is not described,
    // and a report that does not say which machine it measured is not a
    // measurement. `model name` is what an x86 kernel calls it and not what
    // every kernel does, so an empty one is refused rather than printed as
    // the gap between two commas.
    if named.contains(", ,") {
        return Err(SoakError::Lost {
            detail: format!("the machine would not say what processor it has: {named}"),
        });
    }
    Ok(named)
}

/// What one side weighs now.
///
/// # Errors
///
/// [`SoakError::Fixture`] when the container will not say.
pub fn weighed(
    fixture: &Fixture,
    container: &str,
    process: &str,
    at: Duration,
) -> Result<Sample, SoakError> {
    // The daemon is told from the relay beside it by what it is not doing,
    // and the held client from the churn beside it by what it is.
    let census = if process == CLIENT_PROCESS {
        WEIGH_CLIENT.replace("CLIENT", process)
    } else {
        WEIGH.replace("NAME", process)
    };
    let said = fixture.exec(container, &census, COMMAND_DEADLINE.0)?;
    let kibibytes = String::from_utf8_lossy(&said.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0);
    Ok(Sample {
        at,
        bytes: kibibytes.saturating_mul(KIBIBYTE),
    })
}
