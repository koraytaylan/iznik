//! What decides whether a soak passed.
//!
//! One function over a report and two things the run itself knows. It is
//! here, away from the running, because it is the part worth reading on its
//! own: everything a soak can refuse, in the order it refuses it, with the
//! reason beside it.

use core::time::Duration;

use crate::soak::{
    FAILING_STREAK, FINISHED_SHARE, FLOOD_LINES, MEASURABLE_SPAN, OPENING_FLOOD_LINES, Report,
    SOAK_GROWTH_CEILING_PER_HOUR, STALE_AFTER, SoakError, grown, poured,
};

/// Whether a soak proved anything, and what it proved.
///
/// Public so that a case can put a report to it directly. What decides
/// whether a six-hour run passed is worth proving without waiting six hours
/// for one.
///
/// # Errors
///
/// [`SoakError::Lost`] when too few rounds finished, when the held client
/// heard nothing or stopped hearing, when a side was never weighed, or when
/// the pane's stream was broken; [`SoakError::Grew`] when a side grew past
/// the ceiling after the warmup.
pub fn judged(report: &Report, refused: &str, living: bool) -> Result<(), SoakError> {
    let lost = |detail: String| SoakError::Lost { detail };
    // A soak in which most rounds did not finish is a soak that proved
    // nothing, however flat the series it took while nothing was happening.
    // Half rather than all of them, because a stack that stopped answering an
    // hour in leaves the rest of the run measuring a corpse.
    if report.drops.saturating_mul(FINISHED_SHARE) < report.rounds {
        return Err(lost(format!(
            "only {} of {} rounds finished; the last to fail said: {refused}",
            report.drops, report.rounds
        )));
    }
    // Counting them is not enough. A failing round takes the patience it was
    // given where a healthy one takes seconds, so a stack that dies partway
    // through attempts far fewer rounds from then on and the count above can
    // stay under half for hours. What a stack that has stopped answering
    // looks like is every round failing from then on, which is this.
    if report.streak > FAILING_STREAK {
        return Err(lost(format!(
            "{} rounds failed one after another, so the stack stopped answering rather \
             than the machine being busy; the last of them said: {refused}",
            report.streak
        )));
    }
    // The churn is the only thing making and unmaking sessions, and the
    // second daemon is weighed for exactly that reason. A run where it never
    // ran weighs an idle daemon and calls it no leak.
    if report.churn.saturating_mul(FINISHED_SHARE) < report.rounds {
        return Err(lost(format!(
            "only {} of {} rounds churned a session, so what the second daemon weighs is idle",
            report.churn, report.rounds
        )));
    }
    if !living {
        return Err(lost(
            "the held client was gone before the end, so what it heard is not the run".to_owned(),
        ));
    }
    if report.heard.deliveries == 0 {
        return Err(lost(
            "the held client heard nothing, so nothing it heard was whole".to_owned(),
        ));
    }
    // Every byte poured through the pane after the client attached was sent
    // to it, because it returns credit for all of them. Held against the
    // floods that were really poured and not against the rounds that
    // finished: a round that floods and then fails afterwards still made its
    // pane say every byte of it, and counting it out would hand the check a
    // whole flood's worth of slack.
    let owed = poured(FLOOD_LINES).saturating_mul(u64::try_from(report.floods).unwrap_or(0));
    if report.heard.bytes < owed {
        return Err(lost(format!(
            "the held client heard {} bytes of the {owed} its pane was made to say",
            report.heard.bytes
        )));
    }
    // What the pane had already said when the held client arrived. Nothing
    // watches the flood poured before the clock starts — a step that ends on
    // an input reports success whether or not the pane took it — and this is
    // where it shows: a client attaching to a pane that had said six
    // megabytes is told so, and one attaching to an empty pane is told that
    // instead.
    let filled = poured(OPENING_FLOOD_LINES);
    let attached = report.heard.attached.unwrap_or(0);
    if attached < filled {
        return Err(lost(format!(
            "the held client attached at byte {attached} of a pane that should have said \
             {filled} already, so the flood that fills the ring never happened"
        )));
    }
    // A screen is a host that could not carry a client on from where it was:
    // a resume it could not serve, or a pane that fell so far behind it
    // stopped streaming to it. Exactly one is right. The pane says nothing
    // while the daemon is stopped, so every cut is one a resume can be served
    // across, and the attachment is the one screen a run should see; none at
    // all would mean the attachment was never seen, which leaves one real
    // loss looking exactly like a healthy run.
    if report.heard.screens != 1 {
        return Err(lost(format!(
            "the held client was sent {} screens and exactly one is right: the first \
             is its attachment, and any after it are bytes it was not carried on from",
            report.heard.screens
        )));
    }
    for (side, samples) in report.weighed() {
        if samples.is_empty() {
            return Err(lost(format!("the {side} was never weighed at all")));
        }
        // Nothing measured after the warmup is not a pass, where there was
        // long enough to measure. A run whose census stopped matching after
        // its first sample would otherwise report no growth and exit as
        // though it had found none. A run too short for four samples cannot
        // be held to having taken them, and a warmup that leaves nothing at
        // all is refused before a soak starts.
        let measurable = report.duration.saturating_sub(report.warmup) >= MEASURABLE_SPAN;
        // A weighing that stops finding a side leaves its series short
        // rather than empty, and a rate read from samples that stopped an
        // hour ago is a rate from an hour ago. The last of them has to be
        // near the end of the run.
        let ended = samples.last().map_or(Duration::ZERO, |held| held.at);
        if ended.saturating_add(STALE_AFTER) < report.duration {
            return Err(lost(format!(
                "the {side} was last weighed at {} seconds of a run of {}, so it stopped \
                 being found rather than stopping growing",
                ended.as_secs(),
                report.duration.as_secs()
            )));
        }
        let Some(rate) = grown(samples, report.warmup) else {
            if measurable {
                return Err(lost(format!(
                    "the {side} has {} samples and none of them measure anything after the warmup",
                    samples.len()
                )));
            }
            continue;
        };
        if rate > SOAK_GROWTH_CEILING_PER_HOUR {
            return Err(SoakError::Grew {
                side: side.to_owned(),
                rate,
            });
        }
    }
    Ok(())
}
