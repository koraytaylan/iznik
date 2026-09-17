//! The orders a tab's menu and a tab drop produce, and the tabs its close
//! entries close.

use iznik_app::tab_actions::{dropped_order, others, shifted_order, to_the_right};
use iznik_protocol::identity::TabId;

/// Four tabs, in order.
const ORDER: [TabId; 4] = [TabId(1), TabId(2), TabId(3), TabId(4)];

#[test]
/// A tab dropped onto another takes its place: after it when moved right,
/// before it when moved left, and nothing when dropped on itself.
///
/// # Panics
///
/// Panics when an order differs.
fn a_dropped_tab_takes_the_place_it_was_dropped_on() {
    assert_eq!(
        dropped_order(&ORDER, TabId(1), TabId(3)),
        Some(vec![TabId(2), TabId(3), TabId(1), TabId(4)])
    );
    assert_eq!(
        dropped_order(&ORDER, TabId(4), TabId(2)),
        Some(vec![TabId(1), TabId(4), TabId(2), TabId(3)])
    );
    assert_eq!(dropped_order(&ORDER, TabId(2), TabId(2)), None);
    assert_eq!(dropped_order(&ORDER, TabId(9), TabId(2)), None);
}

#[test]
/// Moving left or right trades places with the next tab, and does nothing at either end.
///
/// # Panics
///
/// Panics when an order differs.
fn moves_change_places_and_stop_at_either_end() {
    assert_eq!(
        shifted_order(&ORDER, TabId(2), false),
        Some(vec![TabId(2), TabId(1), TabId(3), TabId(4)])
    );
    assert_eq!(
        shifted_order(&ORDER, TabId(2), true),
        Some(vec![TabId(1), TabId(3), TabId(2), TabId(4)])
    );
    assert_eq!(shifted_order(&ORDER, TabId(1), false), None);
    assert_eq!(shifted_order(&ORDER, TabId(4), true), None);
}

#[test]
/// "Close Other Tabs" closes every other tab; "Close Tabs to the Right"
/// closes the ones after it, and none after the last.
///
/// # Panics
///
/// Panics when a set of closed tabs differs.
fn close_entries_close_the_right_tabs() {
    assert_eq!(others(&ORDER, TabId(3)), [TabId(1), TabId(2), TabId(4)]);
    assert_eq!(to_the_right(&ORDER, TabId(2)), [TabId(3), TabId(4)]);
    assert!(to_the_right(&ORDER, TabId(4)).is_empty());
}
