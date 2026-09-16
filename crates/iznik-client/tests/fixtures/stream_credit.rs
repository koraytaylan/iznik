//! Delivery-bound credit expectations, committed before the receipt implementation.

/// One operation at the manager's delivery and wire-send boundary.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Step {
    /// Announce a fresh pane stream, invalidating older ownership of its channel.
    Open {
        /// Host alias; channel numbers are independent between hosts.
        host: &'static str,
        /// Pane identity within that host.
        pane: u64,
        /// Nonzero channel carrying the pane's output.
        channel: u8,
    },
    /// Capture a receipt when output is delivered, before the UI consumes it.
    Deliver {
        /// Host that delivered the output.
        host: &'static str,
        /// Pane that produced the output.
        pane: u64,
        /// Exact output byte count that can be returned once.
        bytes: u32,
        /// Slot retaining the receipt across later transitions.
        receipt: usize,
    },
    /// Validate the retained receipt when its queued order is about to be carried.
    Return {
        /// The previously delivered receipt, cloned to exercise duplicate returns.
        receipt: usize,
        /// The only grant permitted on the wire, or none for an expired/used receipt.
        expected: Option<Grant>,
    },
    /// Lose one host's connection while retaining unrelated hosts.
    Disconnect {
        /// Host whose old streams must all expire.
        host: &'static str,
    },
    /// Remove one pane's current channel while retaining other panes.
    Detach {
        /// Host that held the pane.
        host: &'static str,
        /// Pane that no longer carries output.
        pane: u64,
    },
    /// Replace the entire manager-side stream registry; old receipts remain held by the UI.
    Restart,
    /// A missing or control-only channel cannot mint a delivery receipt.
    Missing {
        /// Host being queried.
        host: &'static str,
        /// Pane being queried.
        pane: u64,
    },
}

/// Exact destination and count allowed by a still-current delivery receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Grant {
    /// Host receiving the grant.
    pub(crate) host: &'static str,
    /// Pane whose current stream earned it.
    pub(crate) pane: u64,
    /// Channel in that exact stream incarnation.
    pub(crate) channel: u8,
    /// Bytes returned, without truncation or duplication.
    pub(crate) bytes: u32,
}

/// One fixed transition history and its expected wire grants.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Case {
    /// Human-readable behavior this history distinguishes.
    pub(crate) name: &'static str,
    /// Ordered transitions at delivery and carrying time.
    pub(crate) steps: &'static [Step],
}

/// Small distinct byte counts and channel numbers make stale grants visible in each history.
pub(crate) const CASES: &[Case] = &[
    Case {
        name: "current delivery is returned exactly once",
        steps: &[
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 5,
                receipt: 0,
            },
            Step::Return {
                receipt: 0,
                expected: Some(Grant {
                    host: "first",
                    pane: 1,
                    channel: 7,
                    bytes: 5,
                }),
            },
            Step::Return {
                receipt: 0,
                expected: None,
            },
        ],
    },
    Case {
        name: "same pane moves to a different channel",
        steps: &[
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 5,
                receipt: 0,
            },
            Step::Open {
                host: "first",
                pane: 1,
                channel: 8,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 9,
                receipt: 1,
            },
            Step::Return {
                receipt: 0,
                expected: None,
            },
            Step::Return {
                receipt: 1,
                expected: Some(Grant {
                    host: "first",
                    pane: 1,
                    channel: 8,
                    bytes: 9,
                }),
            },
        ],
    },
    Case {
        name: "channel is reassigned to another pane",
        steps: &[
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 5,
                receipt: 0,
            },
            Step::Open {
                host: "first",
                pane: 2,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 2,
                bytes: 9,
                receipt: 1,
            },
            Step::Return {
                receipt: 0,
                expected: None,
            },
            Step::Return {
                receipt: 1,
                expected: Some(Grant {
                    host: "first",
                    pane: 2,
                    channel: 7,
                    bytes: 9,
                }),
            },
        ],
    },
    Case {
        name: "identical pane and channel still form a new stream",
        steps: &[
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 5,
                receipt: 0,
            },
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 9,
                receipt: 1,
            },
            Step::Return {
                receipt: 0,
                expected: None,
            },
            Step::Return {
                receipt: 1,
                expected: Some(Grant {
                    host: "first",
                    pane: 1,
                    channel: 7,
                    bytes: 9,
                }),
            },
        ],
    },
    Case {
        name: "disconnect expires receipts before and after reopening",
        steps: &[
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 5,
                receipt: 0,
            },
            Step::Disconnect { host: "first" },
            Step::Return {
                receipt: 0,
                expected: None,
            },
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 9,
                receipt: 1,
            },
            Step::Return {
                receipt: 0,
                expected: None,
            },
            Step::Return {
                receipt: 1,
                expected: Some(Grant {
                    host: "first",
                    pane: 1,
                    channel: 7,
                    bytes: 9,
                }),
            },
        ],
    },
    Case {
        name: "detach expires only the removed pane",
        steps: &[
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 5,
                receipt: 0,
            },
            Step::Open {
                host: "first",
                pane: 2,
                channel: 8,
            },
            Step::Deliver {
                host: "first",
                pane: 2,
                bytes: 9,
                receipt: 1,
            },
            Step::Detach {
                host: "first",
                pane: 1,
            },
            Step::Return {
                receipt: 0,
                expected: None,
            },
            Step::Return {
                receipt: 1,
                expected: Some(Grant {
                    host: "first",
                    pane: 2,
                    channel: 8,
                    bytes: 9,
                }),
            },
        ],
    },
    Case {
        name: "host channel namespaces are independent",
        steps: &[
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 5,
                receipt: 0,
            },
            Step::Open {
                host: "second",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "second",
                pane: 1,
                bytes: 9,
                receipt: 1,
            },
            Step::Disconnect { host: "first" },
            Step::Return {
                receipt: 0,
                expected: None,
            },
            Step::Return {
                receipt: 1,
                expected: Some(Grant {
                    host: "second",
                    pane: 1,
                    channel: 7,
                    bytes: 9,
                }),
            },
        ],
    },
    Case {
        name: "manager replacement cannot revive an old receipt",
        steps: &[
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 5,
                receipt: 0,
            },
            Step::Restart,
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 9,
                receipt: 1,
            },
            Step::Return {
                receipt: 0,
                expected: None,
            },
            Step::Return {
                receipt: 1,
                expected: Some(Grant {
                    host: "first",
                    pane: 1,
                    channel: 7,
                    bytes: 9,
                }),
            },
        ],
    },
    Case {
        name: "missing and control channels never earn credit",
        steps: &[
            Step::Missing {
                host: "first",
                pane: 1,
            },
            Step::Open {
                host: "first",
                pane: 1,
                channel: 0,
            },
            Step::Missing {
                host: "first",
                pane: 1,
            },
            Step::Open {
                host: "first",
                pane: 1,
                channel: 7,
            },
            Step::Deliver {
                host: "first",
                pane: 1,
                bytes: 5,
                receipt: 0,
            },
            Step::Open {
                host: "first",
                pane: 1,
                channel: 0,
            },
            Step::Return {
                receipt: 0,
                expected: None,
            },
        ],
    },
];
