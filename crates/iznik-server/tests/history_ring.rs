//! The ring proven against a full model of every byte appended: it serves
//! exactly the bytes since any held sequence, ages the rest out by name, holds
//! the newest capacity, wraps into two contiguous slices, and appends without
//! allocating per byte; the budget shrinks the least recently focused pane
//! first. It runs on one thread with no runtime, and a counting global allocator
//! makes the allocation claim measurable.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use iznik_protocol::identity::{PaneId, Sequence};
use iznik_server::history::HistoryBudget;
use iznik_server::history::ring::PaneHistory;

/// Counts every allocation, so a test can assert appends do not allocate.
struct Counting;

/// The number of allocations since the process began.
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to the system allocator unchanged, only
// counting allocations, so it upholds the same contract `System` does.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarding the caller's layout to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarding the pointer and layout the system allocator gave.
        unsafe { System.dealloc(pointer, layout) }
    }
}

/// The process's global allocator, so appends can be counted.
#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The next value of an xorshift generator, for reproducible pseudo-random
/// lengths and bytes.
fn xorshift(state: &mut u64) -> u64 {
    let mut value = *state;
    value ^= value.wrapping_shl(13);
    value ^= value.wrapping_shr(7);
    value ^= value.wrapping_shl(17);
    *state = value;
    value
}

/// A `usize` from a `u64`, saturating.
fn as_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// A `u64` from a `usize`, saturating.
fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Sequences to probe a ring at: the oldest, the newest, and a few between.
fn probes(oldest: u64, newest: u64, state: &mut u64) -> Vec<u64> {
    let mut froms = vec![oldest, newest];
    let span = newest.saturating_sub(oldest);
    if span > 0 {
        for _ in 0..3 {
            froms.push(oldest.saturating_add(xorshift(state).checked_rem(span).unwrap_or(0)));
        }
    }
    froms
}

/// For ten thousand random appends, `range` and `copy_range` from every probed
/// sequence return exactly the bytes appended since it, and `oldest`/`newest`
/// are correct throughout.
///
/// # Panics
///
/// When any served range is not exactly the model's.
#[test]
fn history_ring_serves_exactly_the_bytes_since_a_sequence() {
    let capacity = 4096;
    let mut ring = PaneHistory::new(capacity);
    let mut model: Vec<u8> = Vec::new();
    let mut state = 0x2545_f491_4f6c_dd1d;
    for _ in 0..10000 {
        let length = as_usize(xorshift(&mut state) % 65);
        let mut chunk = Vec::with_capacity(length);
        for _ in 0..length {
            chunk.push(u8::try_from(xorshift(&mut state) % 256).unwrap_or(0));
        }
        ring.append(&chunk);
        model.extend_from_slice(&chunk);

        let total = as_u64(model.len());
        assert_eq!(ring.newest(), Sequence(total), "newest counts every byte");
        let held = model.len().min(capacity);
        assert_eq!(
            ring.oldest(),
            Sequence(total.saturating_sub(as_u64(held))),
            "oldest is newest minus what is held"
        );

        for from in probes(ring.oldest().0, total, &mut state) {
            let expected = model.get(as_usize(from)..).expect("a held sequence");
            let slices = ring.range(Sequence(from)).expect("a held range");
            let mut served = Vec::new();
            served.extend_from_slice(slices.first);
            served.extend_from_slice(slices.second);
            assert_eq!(
                served.as_slice(),
                expected,
                "range serves the bytes since `from`"
            );
            for maximum in [0, 1, 7, expected.len()] {
                let mut into = Vec::new();
                let next = ring
                    .copy_range(Sequence(from), maximum, &mut into)
                    .expect("a held range");
                let take = maximum.min(expected.len());
                assert_eq!(into.as_slice(), expected.get(..take).expect("a prefix"));
                assert_eq!(next, Sequence(from.saturating_add(as_u64(take))));
            }
        }
    }
}

/// A sequence older than the oldest held returns `AgedOut` naming the oldest.
///
/// # Panics
///
/// When it does not.
#[test]
fn history_ring_an_aged_out_sequence_is_named() {
    let mut ring = PaneHistory::new(8);
    ring.append(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    let oldest = ring.oldest();
    assert_eq!(oldest, Sequence(2), "the first two bytes aged out");
    let error = ring
        .range(Sequence(0))
        .expect_err("sequence zero has aged out");
    assert_eq!(
        error,
        iznik_server::history::ring::HistoryError::AgedOut { oldest }
    );
}

/// After more than the capacity is appended, the ring holds exactly the newest
/// capacity bytes.
///
/// # Panics
///
/// When it holds anything else.
#[test]
fn history_ring_holds_exactly_the_newest_capacity() {
    let capacity = 100;
    let mut ring = PaneHistory::new(capacity);
    let mut model = Vec::new();
    let mut counter = 0_u8;
    for _ in 0..1000 {
        ring.append(&[counter]);
        model.push(counter);
        counter = counter.wrapping_add(1);
    }
    let slices = ring.range(ring.oldest()).expect("the held range");
    let mut held = Vec::new();
    held.extend_from_slice(slices.first);
    held.extend_from_slice(slices.second);
    assert_eq!(held.len(), capacity, "exactly the capacity is held");
    let expected = model
        .get(model.len().saturating_sub(capacity)..)
        .expect("the tail");
    assert_eq!(held.as_slice(), expected, "the newest bytes are held");
}

/// A wrapped ring's range is two contiguous slices whose concatenation is the
/// held bytes.
///
/// # Panics
///
/// When the ring never wraps, or the slices do not concatenate to the held
/// bytes.
#[test]
fn history_ring_wraps_into_two_contiguous_slices() {
    let capacity = 100;
    let mut ring = PaneHistory::new(capacity);
    let mut model = Vec::new();
    let mut counter = 0_u8;
    for _ in 0..1000 {
        ring.append(&[counter; 7]);
        model.extend_from_slice(&[counter; 7]);
        counter = counter.wrapping_add(1);
        let slices = ring.range(ring.oldest()).expect("the held range");
        if !slices.second.is_empty() {
            assert!(
                !slices.first.is_empty(),
                "a wrapped range has a first slice too"
            );
            let mut held = Vec::new();
            held.extend_from_slice(slices.first);
            held.extend_from_slice(slices.second);
            let expected = model
                .get(model.len().saturating_sub(capacity)..)
                .expect("the tail");
            assert_eq!(
                held.as_slice(),
                expected,
                "the two slices are the held bytes"
            );
            return;
        }
    }
    panic!("the ring never wrapped into two slices");
}

/// The budget shrinks the least recently focused pane's ring first, and a
/// `touch` changes which pane that is.
///
/// # Panics
///
/// When the wrong pane shrinks.
#[test]
fn history_ring_the_budget_shrinks_the_least_recently_focused_first() {
    let mut budget = HistoryBudget::new(250);
    budget.insert(PaneId(1), 100);
    budget.insert(PaneId(2), 100);
    budget.insert(PaneId(3), 100);
    assert_eq!(
        capacity_of(&budget, PaneId(1)),
        50,
        "the oldest shrank by the overage"
    );
    assert_eq!(capacity_of(&budget, PaneId(2)), 100);
    assert_eq!(capacity_of(&budget, PaneId(3)), 100);

    budget.touch(PaneId(1));
    budget.insert(PaneId(4), 100);
    assert_eq!(
        capacity_of(&budget, PaneId(2)),
        0,
        "the new oldest shrank instead"
    );
    assert_eq!(
        capacity_of(&budget, PaneId(1)),
        50,
        "the touched pane was spared"
    );
}

/// Re-inserting a pane replaces its ring rather than adding a second entry that
/// would double-count against the budget and shadow the newer ring. With a
/// budget of 250 the double-count would force pane one down to 50; replacement
/// leaves it at its full 200.
///
/// # Panics
///
/// When a re-inserted pane does not replace the old one.
#[test]
fn history_ring_re_inserting_a_pane_replaces_its_ring() {
    let mut budget = HistoryBudget::new(250);
    budget.insert(PaneId(1), 100);
    budget.insert(PaneId(1), 200);
    assert_eq!(
        capacity_of(&budget, PaneId(1)),
        200,
        "the re-inserted ring is the only one, not double-counted"
    );
}

/// A pane's ring capacity in a budget, or zero if it has none.
fn capacity_of(budget: &HistoryBudget, pane: PaneId) -> usize {
    budget.history(pane).map_or(0, PaneHistory::capacity)
}

/// Appending ten thousand chunks, once the ring is at capacity, allocates a
/// number of times independent of the number of bytes.
///
/// # Panics
///
/// When appending allocates.
#[test]
fn history_ring_appends_do_not_allocate_in_steady_state() {
    let capacity = 4096;
    let mut ring = PaneHistory::new(capacity);
    let chunk = vec![7_u8; 64];
    // Fill past capacity so the ring is in steady state and its buffer is grown.
    for _ in 0..1000 {
        ring.append(&chunk);
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..10000 {
        ring.append(&chunk);
    }
    let allocations = ALLOCATIONS.load(Ordering::Relaxed).saturating_sub(before);
    assert!(
        allocations < 8,
        "steady-state appends do not allocate: {allocations}"
    );
}
