//! How fast a paste is moving, and how long the bytes still to send would take.
//!
//! The samples live on the window, not in the upload record. A relaunch has
//! no clock for bytes that were sent before it started, so a resumed file
//! waits for two new readings before it names a rate.

use std::time::{Duration, Instant};

use iznik_protocol::identity::PaneId;

use crate::upload::{UploadRecord, byte_pair, byte_text, groups};

/// How long a rate looks back. Older samples are kept only as the start of
/// that window, so a stall at the beginning does not drag the reading forever.
const RATE_WINDOW_SECONDS: u64 = 8;
/// Milliseconds in one second, for a rate taken from a span shorter than a second.
const MILLISECONDS_PER_SECOND: u64 = 1_000;
/// Seconds in one minute.
const MINUTE_SECONDS: u64 = 60;
/// Seconds in one hour.
const HOUR_SECONDS: u64 = 3_600;
/// How many steps a bar distinguishes inside one percent.
const PERCENT_UNIT: u16 = 100;
/// A full bar in [`percent_points`]: [`PERCENT_UNIT`] steps for each percent.
const PERCENT_SCALE: u16 = 10_000;

/// One observation of how many bytes a paste had already sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateSample {
    /// When the observation was taken.
    pub at: Instant,
    /// Bytes sent across the paste at `at`.
    pub sent: u64,
}

/// The samples for one paste, keyed the way the panel keys its row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateTrack {
    /// The host the paste was sent to.
    pub host: String,
    /// The pane whose directory receives it.
    pub pane: PaneId,
    /// The directory the person pasted, or the file when they pasted one file.
    pub name: String,
    /// Observations, oldest first, inside the rate window.
    pub samples: Vec<RateSample>,
}

/// Bytes per second, and how many seconds the bytes still to send would take at that rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RateReading {
    /// Bytes sent per second across the samples.
    pub per_second: u64,
    /// Seconds left at `per_second`. Zero when less than a second of bytes remains.
    pub remaining: u64,
}

/// How far `sent` is through `total`, in hundredths of a percent.
///
/// [`crate::upload::percent`] stays at zero until a whole percent has been sent.
/// The bar uses this finer reading, so it moves with the bytes.
#[must_use]
pub fn percent_points(sent: u64, total: u64) -> u16 {
    if total == 0 {
        return PERCENT_SCALE;
    }
    let scaled = sent
        .min(total)
        .saturating_mul(u64::from(PERCENT_SCALE))
        .checked_div(total)
        .unwrap_or(0)
        .min(u64::from(PERCENT_SCALE));
    u16::try_from(scaled).unwrap_or(PERCENT_SCALE)
}

/// [`percent_points`] as the zero-to-one-hundred value a progress bar takes.
#[must_use]
pub(crate) fn percent_width(sent: u64, total: u64) -> f32 {
    f32::from(percent_points(sent, total)) / f32::from(PERCENT_UNIT)
}

/// Records how far the paste containing `name` has got.
pub fn note(
    tracks: &mut Vec<RateTrack>,
    records: &[UploadRecord],
    host: &str,
    name: &str,
    now: Instant,
) {
    let found = groups(records);
    let Some(group) = found.iter().find(|group| {
        group
            .records
            .iter()
            .any(|record| record.host == host && record.name == name)
    }) else {
        return;
    };
    let Some(root) = group
        .records
        .iter()
        .find(|record| record.type_path)
        .or(group.records.first())
    else {
        return;
    };
    let (sent, _) = byte_pair(&group.records);
    let located = tracks.iter().position(|track| {
        track.host == root.host && track.pane == root.pane && track.name == root.name
    });
    let index = if let Some(index) = located {
        index
    } else {
        tracks.push(RateTrack {
            host: root.host.clone(),
            pane: root.pane,
            name: root.name.clone(),
            samples: Vec::new(),
        });
        tracks.len().saturating_sub(1)
    };
    if let Some(track) = tracks.get_mut(index) {
        remember_sample(track, sent, now);
    }
}

/// The rate line for the paste `name` identifies, when two samples span some time.
#[must_use]
pub fn line(
    tracks: &[RateTrack],
    host: &str,
    pane: PaneId,
    name: &str,
    total: u64,
) -> Option<String> {
    if total == 0 {
        return None;
    }
    let track = tracks
        .iter()
        .find(|track| track.host == host && track.pane == pane && track.name == name)?;
    let reading = reading(&track.samples, total)?;
    let sent = track.samples.last().map_or(0, |sample| sample.sent);
    Some(format!(
        "{}/s · {}",
        byte_text(reading.per_second),
        time_left(reading.remaining, total.saturating_sub(sent))
    ))
}

/// The rate implied by `samples` against `total`.
#[must_use]
pub(crate) fn reading(samples: &[RateSample], total: u64) -> Option<RateReading> {
    let first = samples.first()?;
    let last = samples.last()?;
    let moved = last.sent.saturating_sub(first.sent);
    if moved == 0 {
        return None;
    }
    let elapsed = last.at.saturating_duration_since(first.at);
    let milliseconds = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    if milliseconds == 0 {
        return None;
    }
    let per_second = moved
        .saturating_mul(MILLISECONDS_PER_SECOND)
        .checked_div(milliseconds)
        .unwrap_or(0);
    if per_second == 0 {
        return None;
    }
    let left = total.saturating_sub(last.sent);
    Some(RateReading {
        per_second,
        remaining: left.checked_div(per_second).unwrap_or(0),
    })
}

/// Keeps `sent` when it has moved, and forgets samples older than the window.
fn remember_sample(track: &mut RateTrack, sent: u64, now: Instant) {
    if track
        .samples
        .last()
        .is_some_and(|sample| sample.sent == sent)
    {
        return;
    }
    track.samples.push(RateSample { at: now, sent });
    let window = Duration::from_secs(RATE_WINDOW_SECONDS);
    let Some(limit) = now.checked_sub(window) else {
        return;
    };
    let mut older = None;
    let mut kept = Vec::new();
    for sample in track.samples.drain(..) {
        if sample.at >= limit {
            kept.push(sample);
        } else {
            older = Some(sample);
        }
    }
    if let Some(anchor) = older {
        kept.insert(0, anchor);
    }
    track.samples = kept;
}

/// `seconds` as a short time left, given the bytes still to send.
fn time_left(seconds: u64, left: u64) -> String {
    if left == 0 {
        return "finishing".to_owned();
    }
    if seconds == 0 {
        return "under a second left".to_owned();
    }
    if seconds < MINUTE_SECONDS {
        return count_left(seconds, "second", "seconds");
    }
    if seconds < HOUR_SECONDS {
        return count_left(
            seconds.checked_div(MINUTE_SECONDS).unwrap_or(0),
            "minute",
            "minutes",
        );
    }
    count_left(
        seconds.checked_div(HOUR_SECONDS).unwrap_or(0),
        "hour",
        "hours",
    )
}

/// `count` of `one` or `many`, followed by "left".
fn count_left(count: u64, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one} left")
    } else {
        format!("{count} {many} left")
    }
}
