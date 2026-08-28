//! The resume decision held to its inequality: a cold attach and a repaint
//! plan the truth at `newest` whatever the ring holds, a reconnect the ring
//! covers costs nothing, one it does not is exactly the cold attach, every
//! boundary is asserted by name, and over a thousand generated rings no plan
//! ever names a sequence the ring does not hold.

use iznik_protocol::identity::{PaneId, Sequence};
use iznik_server::resume::{StartPlan, StartRequest, plan_start};

/// How many rings the properties are checked over.
const RINGS: usize = 1_000;

/// The seed the generator starts from, so a failure is reproducible.
const SEED: u64 = 0x2026_0828_1401_0003;

/// How far past `newest` a generated `from` may reach, and how far below
/// `oldest`: enough to land on both sides of both boundaries often.
const REACH: u64 = 8;

/// The largest ring a generated case describes.
const WIDEST_RING: u64 = 64;

/// The pane every case names; the decision does not read it.
const PANE: PaneId = PaneId(7);

/// A deterministic source of `(oldest, newest, from)` triples.
#[derive(Debug)]
struct Rings {
    /// The xorshift state.
    state: u64,
}

impl Rings {
    /// The next value of the xorshift.
    fn next(&mut self) -> u64 {
        let mut state = self.state;
        state ^= state.wrapping_shl(13);
        state ^= state.wrapping_shr(7);
        state ^= state.wrapping_shl(17);
        self.state = state;
        state
    }

    /// The next value below `limit`.
    fn below(&mut self, limit: u64) -> u64 {
        self.next().checked_rem(limit).unwrap_or(0)
    }

    /// A ring and a position that may or may not be inside it.
    fn triple(&mut self) -> (Sequence, Sequence, Sequence) {
        let oldest = self.below(u64::from(u32::MAX));
        let newest = oldest.saturating_add(self.below(WIDEST_RING));
        let below = self.below(REACH);
        let from = oldest
            .saturating_add(self.below(WIDEST_RING.saturating_add(REACH)))
            .saturating_sub(below);
        (Sequence(oldest), Sequence(newest), Sequence(from))
    }
}

/// A cold attach asks for the truth at `newest`, whatever the ring holds.
///
/// # Panics
///
/// When it plans anything else.
#[test]
fn resume_a_cold_attach_plans_the_truth_at_newest() {
    let mut rings = Rings { state: SEED };
    for round in 0..RINGS {
        let (oldest, newest, _from) = rings.triple();
        assert_eq!(
            plan_start(&StartRequest::Subscribe { pane: PANE }, oldest, newest),
            StartPlan::Screen { at: newest },
            "round {round}: a cold attach over {oldest:?}..={newest:?}"
        );
    }
}

/// A repaint asks for the truth at `newest`, so no byte is delivered twice.
///
/// # Panics
///
/// When it plans anything else.
#[test]
fn resume_a_repaint_plans_the_truth_at_newest() {
    let mut rings = Rings { state: SEED };
    for round in 0..RINGS {
        let (oldest, newest, _from) = rings.triple();
        assert_eq!(
            plan_start(&StartRequest::ScreenRequest { pane: PANE }, oldest, newest),
            StartPlan::Screen { at: newest },
            "round {round}: a repaint over {oldest:?}..={newest:?}"
        );
    }
}

/// Every boundary of the reconnect inequality, by name: one before the oldest
/// byte the ring holds, the oldest itself, one inside, the newest itself, and
/// one past it.
///
/// # Panics
///
/// When a boundary falls the wrong way.
#[test]
fn resume_every_boundary_of_the_inequality_is_where_it_says() {
    let (oldest, newest) = (Sequence(100), Sequence(200));
    let cases = [
        (Sequence(99), StartPlan::Screen { at: newest }),
        (Sequence(100), StartPlan::Continue { from: oldest }),
        (
            Sequence(150),
            StartPlan::Continue {
                from: Sequence(150),
            },
        ),
        (Sequence(200), StartPlan::Continue { from: newest }),
        (Sequence(201), StartPlan::Screen { at: newest }),
    ];
    for (from, expected) in cases {
        assert_eq!(
            plan_start(&StartRequest::Resume { pane: PANE, from }, oldest, newest),
            expected,
            "resuming from {from:?} over {oldest:?}..={newest:?}"
        );
    }
    // A ring holding one byte: the two boundaries are the same sequence.
    let single = Sequence(42);
    assert_eq!(
        plan_start(
            &StartRequest::Resume {
                pane: PANE,
                from: single
            },
            single,
            single
        ),
        StartPlan::Continue { from: single },
        "a ring of one byte still covers its own position"
    );
}

/// Over a thousand generated rings the reconnect plan agrees with the
/// inequality exactly, and a plan the ring cannot honour is never made.
///
/// # Panics
///
/// When a plan disagrees with the inequality, or names a sequence the ring
/// does not hold.
#[test]
fn resume_no_plan_names_a_sequence_the_ring_does_not_hold() {
    let mut rings = Rings { state: SEED };
    let mut continued = 0_usize;
    let mut repainted = 0_usize;
    for round in 0..RINGS {
        let (oldest, newest, from) = rings.triple();
        let covered = oldest <= from && from <= newest;
        let plan = plan_start(&StartRequest::Resume { pane: PANE, from }, oldest, newest);
        match plan {
            StartPlan::Continue { from: planned } => {
                assert!(
                    covered,
                    "round {round}: {from:?} is not in {oldest:?}..={newest:?}"
                );
                assert_eq!(planned, from, "round {round}: continued elsewhere");
                assert!(
                    oldest <= planned && planned <= newest,
                    "round {round}: {planned:?} is outside {oldest:?}..={newest:?}"
                );
                continued = continued.saturating_add(1);
            }
            StartPlan::Screen { at } => {
                assert!(
                    !covered,
                    "round {round}: {from:?} is in {oldest:?}..={newest:?}"
                );
                assert_eq!(at, newest, "round {round}: a screen away from newest");
                repainted = repainted.saturating_add(1);
            }
        }
    }
    assert!(
        continued > RINGS / 10 && repainted > RINGS / 10,
        "{continued} continued and {repainted} repainted out of {RINGS}"
    );
}

/// A request names the pane the multiplexer routes it by, whichever it is.
///
/// # Panics
///
/// When a request names another pane.
#[test]
fn resume_a_request_names_its_pane() {
    let requests = [
        StartRequest::Subscribe { pane: PANE },
        StartRequest::Resume {
            pane: PANE,
            from: Sequence(1),
        },
        StartRequest::ScreenRequest { pane: PANE },
    ];
    for request in requests {
        assert_eq!(request.pane(), PANE, "{request:?}");
    }
}
