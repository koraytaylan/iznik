//! The steps a soak writes for the driver to run.
//!
//! Each is one client of its own, made and ended inside the step, so each has
//! its own credit window. What lives across all of them is the held client,
//! which is not a step at all.

use core::time::Duration;

use crate::soak::{
    FLOOD_LINES, OPENING_FLOOD_LINES, PATIENCE, SETTLED_PATIENCE, SPLIT, STEP_DEADLINE, WATCHED,
};

/// The step that makes the pane the soak holds open.
#[must_use]
pub fn opening_step() -> String {
    step_of(
        "opening",
        STEP_DEADLINE,
        &format!(
            "  {{ kind = \"add_host\", alias = \"{WATCHED}\" }},\n  \
             {{ kind = \"await_state\", alias = \"{WATCHED}\", is = \"connected\" }},\n  \
             {{ kind = \"create_session\", alias = \"{WATCHED}\", name = \"soak\" }},\n  \
             {{ kind = \"await_delta\", alias = \"{WATCHED}\", generation = 1 }},\n"
        ),
    )
}

/// The step that fills the ring every pane keeps, before anything is
/// measured.
///
/// A server still filling a four-mebibyte ring is growing for a reason that
/// is not a leak. It only types: a step of the driver returns no credit, so a
/// client of one could not take six megabytes and nothing here waits for it
/// to. What drains this flood is the held client, which credits for every
/// byte, and what waits for it is the soak watching that client go quiet.
#[must_use]
pub fn filling_step() -> String {
    step_of(
        "filling",
        STEP_DEADLINE,
        &format!(
            "  {{ kind = \"add_host\", alias = \"{WATCHED}\" }},\n  \
             {{ kind = \"await_state\", alias = \"{WATCHED}\", is = \"connected\" }},\n  \
             {{ kind = \"input\", alias = \"{WATCHED}\", pane = 1, \
             text = \"seq 1 {OPENING_FLOOD_LINES}\\n\" }},\n"
        ),
    )
}

/// The step that asks whether the pane has gone quiet.
///
/// Its own client, and so its own window and its own cursor: a subscription
/// made now starts at what the pane is saying now, so nothing that has
/// already been said is streamed to it. What it waits for is a line the shell
/// prints, which the shell only reaches once the flood before it is over —
/// and if the flood is still pouring, this client's window fills with it and
/// the wait runs out, which is the caller's cue to ask again.
///
/// The step is given as long as any other, because reaching a host is the
/// same work here as anywhere and a dial alone may take forty seconds; it is
/// the *wait* that is short, so that an asking which is going to fail fails
/// quickly rather than spending a whole step deadline finding out.
#[must_use]
pub fn settled_step() -> String {
    step_of(
        "settled",
        STEP_DEADLINE,
        &format!(
            "  {{ kind = \"add_host\", alias = \"{WATCHED}\" }},\n  \
             {{ kind = \"await_state\", alias = \"{WATCHED}\", is = \"connected\" }},\n  \
             {{ kind = \"subscribe\", alias = \"{WATCHED}\", pane = 1 }},\n  \
             {{ kind = \"input\", alias = \"{WATCHED}\", pane = 1, \
             text = \"echo set{SPLIT}tled\\n\" }},\n  \
             {{ kind = \"await_bytes\", alias = \"{WATCHED}\", pane = 1, \
             contains = \"settled\", within_milliseconds = {SETTLED_PATIENCE} }},\n"
        ),
    )
}

/// A round's flood: poured through the pane and waited for.
///
/// The flood is sized to the window a subscription is given. A step of the
/// driver returns no credit, so a round that poured more than its window
/// would stall for ever against a host that is behaving exactly as it should;
/// the flood that is bigger than any window is the one poured before the
/// clock starts, which the held client drains and pays for.
///
/// The line echoed after it comes back only once the flood has been produced,
/// which is what makes this a flood that happened rather than one that was
/// typed.
#[must_use]
pub fn flooding_step(round: usize) -> String {
    step_of(
        "flooding",
        STEP_DEADLINE,
        &format!(
            "  {{ kind = \"add_host\", alias = \"{WATCHED}\" }},\n  \
             {{ kind = \"await_state\", alias = \"{WATCHED}\", is = \"connected\" }},\n  \
             {{ kind = \"subscribe\", alias = \"{WATCHED}\", pane = 1 }},\n  \
             {{ kind = \"input\", alias = \"{WATCHED}\", pane = 1, \
             text = \"seq 1 {FLOOD_LINES}\\n\" }},\n  \
             {{ kind = \"input\", alias = \"{WATCHED}\", pane = 1, \
             text = \"echo flood{SPLIT}ed-{round}\\n\" }},\n  \
             {{ kind = \"await_bytes\", alias = \"{WATCHED}\", pane = 1, \
             contains = \"flooded-{round}\", within_milliseconds = {PATIENCE} }},\n"
        ),
    )
}

/// And what proves the host is still there after the link was cut.
///
/// A client of its own, made after the daemon was started again: what it
/// proves is the host, and what proves a client carried a pane across the
/// cut is the held one, which was subscribed throughout and never restarted.
#[must_use]
pub fn recovery_step(round: usize) -> String {
    step_of(
        "recovery",
        STEP_DEADLINE,
        &format!(
            "  {{ kind = \"add_host\", alias = \"{WATCHED}\" }},\n  \
             {{ kind = \"await_state\", alias = \"{WATCHED}\", is = \"connected\" }},\n  \
             {{ kind = \"subscribe\", alias = \"{WATCHED}\", pane = 1 }},\n  \
             {{ kind = \"input\", alias = \"{WATCHED}\", pane = 1, \
             text = \"echo rou{SPLIT}nd-{round}\\n\" }},\n  \
             {{ kind = \"await_bytes\", alias = \"{WATCHED}\", pane = 1, \
             contains = \"round-{round}\", within_milliseconds = {PATIENCE} }},\n"
        ),
    )
}

/// One step of the driver, around a list of actions, under a deadline of its
/// own.
///
/// The deadline is the step's rather than one constant for all of them,
/// because the driver clamps everything a step waits for to it: a step that
/// asked for twenty minutes of patience inside a four-minute deadline would
/// get four, and the flood poured before the clock starts is twenty-six times
/// the size of a round's.
///
/// It names no connection timing at all. What a release soak has to exercise
/// is the reconnection the product ships, and a step that named its own ping
/// interval and backoff would be soaking timings nobody runs. The patience is
/// the step's own and not the product's: it is how long the driver waits
/// before calling a step failed.
fn step_of(id: &str, deadline: Duration, actions: &str) -> String {
    format!(
        "scenario = \"soak\"\nid = \"{id}\"\ncontainer = \"engine\"\n\
         timeout_seconds = {}\n\n[manager]\npatience_milliseconds = {}\n\
         actions = [\n{actions}]\n",
        deadline.as_secs(),
        deadline.as_millis()
    )
}
