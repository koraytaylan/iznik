//! The local terminal follows a submitted window size until the model does.

use iznik_app::chrome::{LocalSize, local_size};

/// Columns and rows of the terminal before a submission.
const NARROW: (u16, u16) = (80, 24);
/// Columns and rows submitted for a larger window.
const WIDE: (u16, u16) = (200, 60);
/// Columns and rows chosen by another client.
const REMOTE: (u16, u16) = (100, 40);

/// Assert one `local_size` decision.
///
/// # Panics
/// Panics when the decision differs.
fn check_local_size(
    measured: Option<(u16, u16)>,
    awaiting_model: Option<(u16, u16)>,
    native_size: Option<(u16, u16)>,
    desired: (u16, u16),
    shown: (u16, u16),
    baseline: Option<(u16, u16)>,
    action: LocalSize,
) {
    assert_eq!(
        local_size(measured, awaiting_model, native_size, desired, shown),
        (baseline, action),
        "the local size decision"
    );
}

/// A submission stays on the local terminal until the model catches up,
/// and a different model size replaces it.
///
/// # Panics
/// Panics when the decision is not that sequence.
#[test]
fn local_size_keeps_submitted_geometry() {
    check_local_size(
        Some(WIDE),
        Some(NARROW),
        None,
        NARROW,
        NARROW,
        Some(NARROW),
        LocalSize::Apply(WIDE),
    );
    check_local_size(
        Some(WIDE),
        Some(NARROW),
        Some(WIDE),
        NARROW,
        NARROW,
        Some(NARROW),
        LocalSize::Apply(WIDE),
    );
    check_local_size(
        Some(WIDE),
        Some(NARROW),
        Some(WIDE),
        NARROW,
        WIDE,
        Some(NARROW),
        LocalSize::Hold,
    );
    check_local_size(
        Some(WIDE),
        Some(NARROW),
        Some(WIDE),
        WIDE,
        WIDE,
        None,
        LocalSize::Settled,
    );
    check_local_size(
        Some(WIDE),
        Some(NARROW),
        Some(WIDE),
        REMOTE,
        WIDE,
        None,
        LocalSize::Apply(REMOTE),
    );
}
